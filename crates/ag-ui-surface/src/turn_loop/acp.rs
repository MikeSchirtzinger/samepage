//! ACP (Agent Client Protocol) backend — spawns a coding-agent subprocess and
//! drives it over newline-delimited JSON-RPC on stdio. Lifted from teaching-
//! canvas's `acp.rs` (M2 Phase C), generic over [`Surface`] instead of
//! `AppState`.
//!
//! ACP is newline-delimited JSON-RPC 2.0 over a spawned agent's stdio. This
//! host is the *client*: it spawns a provider agent, does the `initialize`
//! handshake, opens a session with the runtime-owned HTTP MCP endpoint wired
//! in, and then drives one `session/prompt` per turn —
//! streaming the agent's spoken text back as narration.
//!
//! The ACP agent calls `/mcp`; that handler projects schemas from the effective
//! `Surface::tools()` set and invokes the same in-process dispatcher as every
//! other actor. A caller-supplied [`super::McpBridge`] remains only as a stdio
//! compatibility fallback for adapters that do not advertise HTTP MCP.
//!
//! Robustness notes from probing real agents:
//! - stdout is exclusively the framed protocol channel. Any non-empty line
//!   that is not valid JSON-RPC terminates the adapter session; provider logs
//!   belong on stderr.
//! - The agent may ask us to approve tool calls (`session/request_permission`);
//!   we auto-allow, mirroring the old sidecar's `bypassPermissions`.

use std::collections::HashMap;
#[cfg(unix)]
use std::io;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as AsyncMutex};
use tracing::{info, warn};

use crate::narration::{finish_turn, narrate_cancel, narrate_flush, reset_narration};
use crate::runtime_state::{RuntimeState, TurnRequest};
use crate::turn_loop::{build_context, McpBridge, Outcome, Prompts};
use crate::Surface;

/// An in-flight JSON-RPC client over the agent's stdio. Requests are
/// correlated by integer id; notifications and agent→client requests are
/// handled by the reader loop.
pub struct AcpClient {
    stdin: AsyncMutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    /// What the browser has already been told about each ACP tool call.
    ///
    /// ACP reports one call across an open-ended series of `tool_call` /
    /// `tool_call_update` notifications, any of which may repeat or omit
    /// fields. AG-UI's contract is stricter — one START, then args, then one
    /// terminal RESULT — so the announced/settled state has to live somewhere
    /// per connection rather than be re-derived per notification.
    tool_calls: Mutex<HashMap<String, AnnouncedToolCall>>,
}

/// How far one ACP tool call has progressed in AG-UI terms.
#[derive(Default)]
struct AnnouncedToolCall {
    /// `TOOL_CALL_ARGS` + `TOOL_CALL_END` have gone out; `rawInput` was seen.
    arguments_sent: bool,
    /// A terminal `TOOL_CALL_RESULT` has gone out. Later duplicate updates for
    /// the same id are ignored.
    settled: bool,
}

struct PendingGuard<'a> {
    pending: &'a Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    id: i64,
    armed: bool,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.pending.lock().remove(&self.id);
        }
    }
}

impl AcpClient {
    fn new(stdin: ChildStdin) -> Arc<Self> {
        Arc::new(Self {
            stdin: AsyncMutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            tool_calls: Mutex::new(HashMap::new()),
        })
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let (id, rx) = self
            .start_request_with_timeout(method, params, timeout)
            .await?;
        let _guard = PendingGuard {
            pending: &self.pending,
            id,
            armed: true,
        };
        Self::wait_for_response(method, timeout, rx).await
    }

    /// Register and fully write a request before returning its response
    /// receiver. This split is what lets turn cancellation prove the prompt is
    /// on the wire before sending `session/cancel`.
    async fn start_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(i64, oneshot::Receiver<Result<Value, String>>), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let mut guard = PendingGuard {
            pending: &self.pending,
            id,
            armed: true,
        };
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_line(&line).await?;
        guard.armed = false;
        Ok((id, rx))
    }

    async fn start_request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<(i64, oneshot::Receiver<Result<Value, String>>), String> {
        tokio::time::timeout(timeout, self.start_request(method, params))
            .await
            .map_err(|_| {
                format!(
                    "ACP {method} write did not complete within {}ms",
                    timeout.as_millis()
                )
            })?
    }

    async fn wait_for_response(
        method: &str,
        timeout: Duration,
        rx: oneshot::Receiver<Result<Value, String>>,
    ) -> Result<Value, String> {
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!("agent closed before replying to {method}")),
            Err(_) => Err(format!(
                "agent did not reply to {method} within {}s",
                timeout.as_secs()
            )),
        }
    }

    /// Write one already-built JSON value as an ndjson line + flush.
    async fn write_line(&self, v: &Value) -> Result<(), String> {
        let mut s = v.to_string();
        s.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(s.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    /// Send a JSON-RPC *notification* (no id, no reply expected) — e.g.
    /// `session/cancel` to interrupt the agent's current turn.
    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// Respond to an agent→client request (we are the server for these).
    async fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        self.write_line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await
    }

    /// Respond to an unsupported agent→client request with the standard
    /// JSON-RPC method-not-found error.
    async fn respond_method_not_found(&self, id: Value) -> Result<(), String> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" }
        }))
        .await
    }

    /// Fail every outstanding request — called when the agent's stdout closes.
    async fn fail_all(&self, why: &str) {
        for (_, tx) in self.pending.lock().drain() {
            let _ = tx.send(Err(why.to_string()));
        }
    }

    /// Close the host side of the adapter's stdin before process teardown.
    /// Well-behaved ACP adapters treat EOF as a graceful shutdown request; the
    /// process-group guard below handles wrappers that ignore or spin on EOF.
    async fn close(&self) {
        let _ = self.stdin.lock().await.shutdown().await;
    }
}

const PROCESS_TERM_GRACE: Duration = Duration::from_secs(1);
const PROCESS_KILL_GRACE: Duration = Duration::from_secs(1);
const SETUP_TIMEOUT: Duration = Duration::from_secs(45);
const PRIME_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const TURN_CANCEL_TIMEOUT: Duration = Duration::from_secs(6);
/// ACP is newline-delimited JSON. Bound one frame before allocation so a
/// corrupt or hostile adapter cannot grow the host without limit.
const MAX_ACP_STDOUT_LINE_BYTES: usize = 1024 * 1024;

/// Read one newline-delimited frame without the unbounded allocation behavior
/// of `AsyncBufReadExt::lines`. The returned bytes do not include `\n`.
async fn read_bounded_line<R>(reader: &mut R, limit: usize) -> Result<Option<Vec<u8>>, String>
where
    R: AsyncBufRead + Unpin,
{
    super::read_bounded_jsonl_line(reader, limit, "ACP").await
}

/// Consume ACP stdout until EOF or the first protocol/transport failure.
/// Every terminal path drains pending requests before returning its reason.
async fn consume_acp_stdout<R>(
    reader: &mut R,
    client: &Arc<AcpClient>,
    rt: &Arc<RuntimeState>,
) -> String
where
    R: AsyncBufRead + Unpin,
{
    let reason = loop {
        let line = match read_bounded_line(reader, MAX_ACP_STDOUT_LINE_BYTES).await {
            Ok(Some(line)) => line,
            Ok(None) => break "agent stdout closed".to_string(),
            Err(error) => break error,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let msg = match serde_json::from_slice::<Value>(&line) {
            Ok(msg) => msg,
            Err(error) => break format!("malformed JSON on ACP stdout: {error}"),
        };
        if let Err(error) = dispatch(client, rt, msg).await {
            break format!("invalid ACP stdout protocol: {error}");
        }
    };

    client.fail_all(&reason).await;
    reason
}

/// Put an ACP adapter in its own Unix process group. Providers are frequently
/// wrapper chains (`npx` -> Node -> native adapter), so owning only the direct
/// child is not enough to clean up the real agent.
pub(super) fn isolate_process_group(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
}

/// Owns the direct ACP child and, on Unix, every descendant that inherits its
/// dedicated process group. Normal exits use [`shutdown`](Self::shutdown) to
/// TERM, wait, KILL if needed, and reap. `Drop` is the cancellation fallback:
/// if the surrounding async task disappears, it still kills the whole group.
pub(super) struct ManagedAgentProcess {
    child: Option<Child>,
    #[cfg(unix)]
    process_group: Option<libc::pid_t>,
}

impl ManagedAgentProcess {
    pub(super) fn new(child: Child) -> Self {
        #[cfg(unix)]
        let process_group = child.id().and_then(|pid| i32::try_from(pid).ok());

        Self {
            child: Some(child),
            #[cfg(unix)]
            process_group,
        }
    }

    pub(super) async fn shutdown(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            if process_group_exists(process_group) {
                if let Err(error) = signal_process_group(process_group, libc::SIGTERM) {
                    warn!(process_group, %error, "failed to terminate managed-agent process group");
                }

                if !self
                    .wait_for_process_group_exit(process_group, PROCESS_TERM_GRACE)
                    .await
                {
                    warn!(
                        process_group,
                        "managed-agent process group ignored SIGTERM; sending SIGKILL"
                    );
                    if let Err(error) = signal_process_group(process_group, libc::SIGKILL) {
                        warn!(process_group, %error, "failed to kill managed-agent process group");
                    }
                    let _ = self
                        .wait_for_process_group_exit(process_group, PROCESS_KILL_GRACE)
                        .await;
                }
            }
        }

        #[cfg(not(unix))]
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }

        // Explicitly wait for the direct child. Tokio's kill-on-drop is only a
        // best-effort reaper and cannot provide this guarantee by itself.
        if let Some(mut child) = self.child.take() {
            if tokio::time::timeout(PROCESS_KILL_GRACE, child.wait())
                .await
                .is_err()
            {
                warn!("managed-agent direct child did not exit; forcing shutdown");
                let _ = child.kill().await;
            }
        }

        #[cfg(unix)]
        if self
            .process_group
            .is_some_and(|process_group| !process_group_exists(process_group))
        {
            self.process_group = None;
        }
    }

    #[cfg(unix)]
    async fn wait_for_process_group_exit(
        &mut self,
        process_group: libc::pid_t,
        grace: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            // Reap the group leader as soon as it exits; otherwise its zombie
            // keeps the process group visible even after descendants are gone.
            if let Some(child) = self.child.as_mut() {
                let _ = child.try_wait();
            }
            if !process_group_exists(process_group) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for ManagedAgentProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            let _ = signal_process_group(process_group, libc::SIGKILL);
        }

        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

#[cfg(unix)]
fn signal_process_group(process_group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    if process_group <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group must be positive",
        ));
    }

    // A negative pid targets every process in that group. The child was made
    // its group leader at spawn, so this cannot signal the AG-UI host itself.
    // SAFETY: `kill` is called with a validated positive process-group id; the
    // negation deliberately targets the isolated child group, not this host.
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn process_group_exists(process_group: libc::pid_t) -> bool {
    if process_group <= 0 {
        return false;
    }

    // SAFETY: signal 0 performs an existence/permission check and does not
    // mutate the validated isolated process group.
    let result = unsafe { libc::kill(-process_group, 0) };
    if result == 0 {
        return true;
    }

    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Build the ACP prime turn: the standing context plus the READY handshake
/// (ACP has no portable system-prompt field, so the persona goes in as turn
/// one).
fn build_prime(prompts: &Prompts, surface: &dyn Surface) -> String {
    let mut p = build_context(prompts, surface);
    p.push_str(
        "\n\nThis first message is setup only. Do NOT take any actions yet and do NOT call any tools. \
         Just reply with the single word READY.",
    );
    p
}

fn mcp_server_descriptor(
    initialize_result: &Value,
    rt: &RuntimeState,
    bridge: Option<&McpBridge>,
) -> Option<Value> {
    mcp_server_descriptor_for(initialize_result, rt.port, &rt.mcp_token, bridge)
}

fn mcp_server_descriptor_for(
    initialize_result: &Value,
    port: u16,
    token: &str,
    bridge: Option<&McpBridge>,
) -> Option<Value> {
    let supports_http = initialize_result
        .pointer("/agentCapabilities/mcpCapabilities/http")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if supports_http {
        return Some(json!({
            "type": "http",
            "name": "surface",
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "headers": [{
                "name": "Authorization",
                "value": format!("Bearer {token}")
            }]
        }));
    }

    bridge.map(|bridge| {
        json!({
            "name": bridge.name,
            "command": bridge.program,
            "args": bridge.args,
            "env": bridge.env.iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect::<Vec<_>>()
        })
    })
}

/// Run a single provider agent end to end: spawn → handshake → prime → serve
/// turns until a switch is requested or the agent dies.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_one(
    rt: &Arc<RuntimeState>,
    surface: &Arc<dyn Surface>,
    prompts: &Arc<Prompts>,
    bridge: Option<&McpBridge>,
    agent_cwd: &std::path::Path,
    provider_id: &str,
    provider_auth: &str,
    program: String,
    args: Vec<String>,
    ask_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    mission_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    switch_rx: &mut mpsc::UnboundedReceiver<String>,
) -> Outcome {
    rt.ready.store(false, Ordering::Relaxed);
    rt.warming.store(true, Ordering::Relaxed);
    crate::narration::tutor_event(rt, "warming", None);
    info!("starting provider '{provider_id}' ({provider_auth})");

    // Inherit the host env (so each agent finds its own credentials/config in
    // $HOME).
    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .current_dir(agent_cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    isolate_process_group(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to spawn '{program}' ({provider_id}): {e}");
            return Outcome::Exited;
        }
    };
    let Some(stdin) = child.stdin.take() else {
        warn!("provider '{provider_id}' spawned without piped stdin");
        let mut process = ManagedAgentProcess::new(child);
        process.shutdown().await;
        return Outcome::Exited;
    };
    let Some(stdout) = child.stdout.take() else {
        warn!("provider '{provider_id}' spawned without piped stdout");
        let mut process = ManagedAgentProcess::new(child);
        process.shutdown().await;
        return Outcome::Exited;
    };
    let mut process = ManagedAgentProcess::new(child);
    let client = AcpClient::new(stdin);

    // Reader: dispatch responses, notifications, and agent→client requests.
    let reader_rt = rt.clone();
    let reader_client = client.clone();
    let (reader_closed_tx, mut reader_closed_rx) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let reason =
            consume_acp_stdout(&mut BufReader::new(stdout), &reader_client, &reader_rt).await;
        warn!(%reason, "ACP stdout reader terminated");
        let _ = reader_closed_tx.send(());
    });

    let provider_session = async {
        // ── handshake ────────────────────────────────────────────────────
        let init = client
            .request_with_timeout(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false
                    },
                    "clientInfo": { "name": "ag-ui-surface", "version": "0.1.0" }
                }),
                SETUP_TIMEOUT,
            )
            .await;
        let init = match init {
            Ok(init) => init,
            Err(e) => {
                warn!("initialize failed for '{provider_id}': {e}");
                return Outcome::Exited;
            }
        };

        let Some(mcp_server) = mcp_server_descriptor(&init, rt, bridge) else {
            warn!(
                "provider '{provider_id}' does not advertise HTTP MCP and no custom \
                 App::mcp_bridge(...) stdio fallback was configured"
            );
            return Outcome::Exited;
        };
        info!(
            "provider '{provider_id}' MCP transport: {}",
            if mcp_server.get("type").and_then(|value| value.as_str()) == Some("http") {
                "runtime HTTP"
            } else {
                "custom stdio fallback"
            }
        );
        let cwd = agent_cwd.to_string_lossy().to_string();
        let session = client
            .request_with_timeout(
                "session/new",
                json!({
                    "cwd": cwd,
                    "mcpServers": [mcp_server]
                }),
                SETUP_TIMEOUT,
            )
            .await;
        let session_id = match session.as_ref().map(|v| v.get("sessionId").cloned()) {
            Ok(Some(Value::String(s))) => s,
            other => {
                warn!("session/new failed for '{provider_id}': {other:?}");
                return Outcome::Exited;
            }
        };
        info!("provider '{provider_id}' session {session_id}");

        // ── prime (suppressed narration), then mark ready ────────────────────
        // The Surface's store context is read fresh here, so a returning learner
        // (or a provider switch) resumes at the edge of what it already knows.
        let prime = build_prime(prompts, surface.as_ref());
        if let Err(error) = client
            .request_with_timeout(
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": [{ "type": "text", "text": prime }]
                }),
                PRIME_TIMEOUT,
            )
            .await
        {
            warn!("prime failed for '{provider_id}': {error}");
            return Outcome::Exited;
        }
        reset_narration(rt);
        rt.warming.store(false, Ordering::Relaxed);
        rt.ready.store(true, Ordering::Relaxed);
        crate::narration::tutor_event(rt, "ready", None);
        info!("provider '{provider_id}' ready");

        // ── serve questions + missions ───────────────────────────────────────
        loop {
            tokio::select! {
                maybe_q = ask_rx.recv() => {
                    let Some(mut q) = maybe_q else { return Outcome::Exited };
                    if let Err(e) = run_turn(
                        &client,
                        rt,
                        &session_id,
                        &q.text,
                        &mut q.cancel,
                    ).await {
                        warn!("prompt failed on '{provider_id}': {e}");
                        return Outcome::Exited;
                    }
                }
                maybe_m = mission_rx.recv() => {
                    let Some(mut m) = maybe_m else { return Outcome::Exited };
                    let text = (prompts.mission)(&m.text);
                    if let Err(e) = run_turn(&client, rt, &session_id, &text, &mut m.cancel).await {
                        warn!("mission turn failed on '{provider_id}': {e}");
                        return Outcome::Exited;
                    }
                }
            }
        }
    };
    let mut provider_session = Box::pin(provider_session);
    // Keep switching responsive during process setup, initialize, session/new,
    // priming, and active turns. Dropping the provider-session future cancels
    // any outstanding request; the owned process group is torn down below.
    let outcome = tokio::select! {
        biased;
        maybe_switch = switch_rx.recv() => {
            rt.ready.store(false, Ordering::Relaxed);
            rt.warming.store(true, Ordering::Relaxed);
            narrate_cancel(rt);
            rt.busy.store(false, Ordering::SeqCst);
            match maybe_switch {
                Some(new_id) => {
                    info!("switching provider '{provider_id}' -> '{new_id}'");
                    Outcome::Switch(new_id)
                }
                None => Outcome::Exited,
            }
        }
        _ = &mut reader_closed_rx => {
            warn!("provider '{provider_id}' stdout closed");
            Outcome::Exited
        }
        outcome = &mut provider_session => outcome,
    };

    // Destroy any request future before taking the shared writer lock, then
    // stop the reader so an agent→client response cannot retain that lock.
    drop(provider_session);
    reader.abort();
    let _ = reader.await;
    let _ = tokio::time::timeout(Duration::from_millis(500), client.close()).await;
    process.shutdown().await;
    outcome
}

/// Run one turn: mark busy, prompt the agent, stream its spoken text as
/// narration, then settle back to idle. `question` is the line to echo as
/// the "thinking" caption (`None` for a mission turn, which the page echoes
/// itself).
///
/// The turn is cancellable (barge-in): it races the agent's `session/prompt`
/// against the interrupt broadcast. On interrupt it sends `session/cancel` to
/// the agent, hushes narration, frees the UI, and drains the now-cancelled
/// prompt so the session stays clean for the next turn.
async fn run_turn(
    client: &Arc<AcpClient>,
    rt: &Arc<RuntimeState>,
    session_id: &str,
    text: &str,
    cancel: &mut broadcast::Receiver<()>,
) -> Result<(), String> {
    let mut active_turn = ActiveAcpTurn::begin(rt, session_id);
    let turn_start = std::time::Instant::now();
    match cancel.try_recv() {
        Ok(()) | Err(broadcast::error::TryRecvError::Lagged(_)) => {
            active_turn.clear();
            narrate_cancel(rt);
            finish_turn(rt);
            return Ok(());
        }
        Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {}
    }
    let (request_id, response_rx) = client
        .start_request_with_timeout(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": text }]
            }),
            TURN_CANCEL_TIMEOUT,
        )
        .await?;
    let _pending_guard = PendingGuard {
        pending: &client.pending,
        id: request_id,
        armed: true,
    };
    let req = AcpClient::wait_for_response("session/prompt", REQUEST_TIMEOUT, response_rx);
    tokio::pin!(req);

    tokio::select! {
        biased;
        _ = cancel.recv() => {
            info!("barge-in: cancelling current ACP turn");
            // Revoke output authorization before notifying/draining the old
            // prompt. A newly queued HTTP turn may set `busy=true` during this
            // window; that must not make late chunks from this prompt valid.
            active_turn.clear();
            narrate_cancel(rt);
            // Bound the entire cancel protocol, including acquiring/writing
            // ACP stdin and draining the old prompt response.
            let cancellation = async {
                client
                    .notify("session/cancel", json!({ "sessionId": session_id }))
                    .await?;
                let _ = (&mut req).await;
                Ok::<(), String>(())
            };
            match tokio::time::timeout(TURN_CANCEL_TIMEOUT, cancellation).await {
                // Both a normal result and a JSON-RPC cancellation error prove
                // the outstanding prompt has settled, so the session is clean.
                Ok(Ok(())) => {
                    // Only a settled prompt is an interrupt acknowledgement.
                    // Publish done + idle atomically after the provider has
                    // proved the old turn can no longer overlap a new one.
                    finish_turn(rt);
                }
                Ok(Err(error)) => {
                    return Err(format!("failed to send ACP cancellation: {error}"));
                }
                Err(_) => {
                    return Err(
                        "agent did not acknowledge cancellation within 6 seconds; provider session must restart"
                            .to_string(),
                    );
                }
            }
            Ok(())
        }
        r = &mut req => {
            active_turn.clear();
            narrate_flush(rt);
            info!("acp turn complete in {:?}", turn_start.elapsed());
            finish_turn(rt);
            r.map(|_| ())
        }
    }
}

struct ActiveAcpTurn {
    rt: Arc<RuntimeState>,
    session_id: String,
    active: bool,
}

impl ActiveAcpTurn {
    fn begin(rt: &Arc<RuntimeState>, session_id: &str) -> Self {
        let _replay_guard = rt.transcript_replay_lock.lock();
        *rt.stream_active_session.lock() = Some(session_id.to_string());
        Self {
            rt: rt.clone(),
            session_id: session_id.to_string(),
            active: true,
        }
    }

    fn clear(&mut self) {
        if !self.active {
            return;
        }
        let _replay_guard = self.rt.transcript_replay_lock.lock();
        let mut active = self.rt.stream_active_session.lock();
        if active.as_deref() == Some(self.session_id.as_str()) {
            *active = None;
        }
        self.active = false;
    }
}

impl Drop for ActiveAcpTurn {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Handle one inbound JSON-RPC message: response, notification, or request.
async fn dispatch(
    client: &Arc<AcpClient>,
    rt: &Arc<RuntimeState>,
    msg: Value,
) -> Result<(), String> {
    let object = msg
        .as_object()
        .ok_or_else(|| "JSON-RPC message is not an object".to_string())?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("JSON-RPC message does not declare jsonrpc=2.0".to_string());
    }

    if let Some(method_value) = object.get("method") {
        let method = method_value
            .as_str()
            .ok_or_else(|| "JSON-RPC method is not a string".to_string())?;
        if object.contains_key("result") || object.contains_key("error") {
            return Err("JSON-RPC request contains response fields".to_string());
        }

        let Some(id) = object.get("id") else {
            handle_notification(&client.tool_calls, rt, method, &msg);
            return Ok(());
        };
        if !(id.is_i64() || id.is_u64() || id.is_string()) {
            return Err("JSON-RPC request id is not a string or integer".to_string());
        }
        return handle_agent_request(client, method, &msg, id.clone()).await;
    }

    dispatch_response(&client.pending, &msg)
}

/// Validate the response subset of JSON-RPC 2.0 used by this client.
///
/// Requests originated here use integer ids, so another id type cannot match
/// an outstanding request. JSON-RPC errors require integer `code` and string
/// `message` members.
fn validated_response(msg: &Value) -> Result<(i64, Result<Value, String>), &'static str> {
    let object = msg.as_object().ok_or("response is not an object")?;
    let id = object
        .get("id")
        .and_then(Value::as_i64)
        .ok_or("response id is not a matching integer id")?;
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");

    if has_result == has_error {
        return Err("response must contain exactly one of result or error");
    }

    if has_result {
        return Ok((id, Ok(object["result"].clone())));
    }

    let error = object["error"]
        .as_object()
        .ok_or("response error is not an object")?;
    if error.get("code").and_then(Value::as_i64).is_none()
        || error.get("message").and_then(Value::as_str).is_none()
    {
        return Err("response error requires an integer code and string message");
    }
    Ok((id, Err(Value::Object(error.clone()).to_string())))
}

fn dispatch_response(
    pending: &Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>,
    msg: &Value,
) -> Result<(), String> {
    let (id, result) = validated_response(msg).map_err(str::to_string)?;

    match pending.lock().remove(&id) {
        Some(tx) => {
            let _ = tx.send(result);
        }
        None => warn!(id, "discarding ACP response for unknown request id"),
    }
    Ok(())
}

/// Streamed `session/update` notifications: pull spoken text out as narration,
/// and project tool-call progress onto the AG-UI event stream.
fn handle_notification(
    tool_calls: &Mutex<HashMap<String, AnnouncedToolCall>>,
    rt: &Arc<RuntimeState>,
    method: &str,
    msg: &Value,
) {
    if method != "session/update" {
        return;
    }
    let Some(params) = msg.get("params") else {
        return;
    };
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    let Some(update) = params.get("update") else {
        return;
    };
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("agent_message_chunk") => {
            if let Some(text) = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(Value::as_str)
            {
                crate::narration::narrate_provider_chunk(rt, session_id, text);
            }
        }
        // These notifications do not *execute* anything — a Surface action
        // runs through the runtime-owned MCP endpoint, and the agent's own
        // tools run inside the agent. They are the only signal the browser
        // gets that work is underway, so they are forwarded as visibility and
        // never re-applied as an action.
        Some("tool_call") | Some("tool_call_update") => {
            handle_tool_call_notification(tool_calls, rt, update);
        }
        _ => {}
    }
}

/// Project one ACP `tool_call`/`tool_call_update` onto AG-UI's stricter
/// START → ARGS → END → RESULT contract.
fn handle_tool_call_notification(
    tool_calls: &Mutex<HashMap<String, AnnouncedToolCall>>,
    rt: &Arc<RuntimeState>,
    update: &Value,
) {
    let Some(tool_call_id) = update.get("toolCallId").and_then(Value::as_str) else {
        return;
    };
    let status = update.get("status").and_then(Value::as_str);
    let terminal = matches!(status, Some("completed") | Some("failed"));

    let mut tracked = tool_calls.lock();
    // A settled call is done; later duplicates for the same id are dropped so
    // one execution never renders as two.
    if tracked.get(tool_call_id).is_some_and(|call| call.settled) {
        return;
    }
    let known = tracked.contains_key(tool_call_id);
    let call = tracked.entry(tool_call_id.to_string()).or_default();

    if !known {
        // ACP has no tool-name field: `title` is the human-readable label the
        // agent chose, which is the thing worth showing. `kind` is only a
        // coarse category, used when a title is missing.
        let name = update
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .or_else(|| update.get("kind").and_then(Value::as_str))
            .unwrap_or("tool");
        super::emit_tool_call_start(rt, tool_call_id, name);
    }

    // ACP delivers `rawInput` whole rather than as a token stream, so this is
    // a single delta followed immediately by END.
    if !call.arguments_sent {
        if let Some(raw_input) = update.get("rawInput") {
            super::emit_tool_call_args(rt, tool_call_id, &raw_input.to_string());
            super::emit_tool_call_end(rt, tool_call_id);
            call.arguments_sent = true;
        }
    }

    if terminal {
        if !call.arguments_sent {
            // The call completed without ever reporting its input. Close the
            // argument stream anyway so the client is not left waiting.
            super::emit_tool_call_end(rt, tool_call_id);
            call.arguments_sent = true;
        }
        let mut content = acp_tool_content_text(update);
        if content.is_empty() {
            content = match status {
                Some("failed") => "action failed".to_string(),
                _ => "action completed".to_string(),
            };
        } else if status == Some("failed") {
            content = format!("action failed: {content}");
        }
        super::emit_tool_call_result(rt, tool_call_id, &content);
        call.settled = true;
    }
}

/// Flatten an ACP tool call's `content` blocks into the text a human reads.
///
/// Only text blocks are rendered. Anything else is named rather than
/// described, so an unrendered block is visible as a gap instead of silently
/// reading as an empty result.
fn acp_tool_content_text(update: &Value) -> String {
    let Some(blocks) = update.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            // ACP wraps a tool's output as { "type": "content", "content": { .. } }.
            let inner = block.get("content").unwrap_or(block);
            match inner.get("type").and_then(Value::as_str) {
                Some("text") => inner
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                Some(other) => Some(format!("[{other} content]")),
                None => None,
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Agent→client requests. We auto-allow permission prompts and reject
/// anything else (we advertised no fs/terminal capability).
async fn handle_agent_request(
    client: &Arc<AcpClient>,
    method: &str,
    msg: &Value,
    id: Value,
) -> Result<(), String> {
    if method == "session/request_permission" {
        let chosen = msg
            .get("params")
            .and_then(|p| p.get("options"))
            .and_then(Value::as_array)
            .and_then(|opts| pick_allow(opts));
        match chosen {
            Some(option_id) => {
                client
                    .respond(
                        id,
                        json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
                    )
                    .await
            }
            None => {
                client
                    .respond(id, json!({ "outcome": { "outcome": "cancelled" } }))
                    .await
            }
        }
    } else {
        client.respond_method_not_found(id).await
    }
    .map_err(|error| format!("failed responding to ACP request '{method}': {error}"))
}

/// Prefer an "allow always" option, then "allow once", then any non-deny.
fn pick_allow(options: &[Value]) -> Option<String> {
    let kind = |o: &Value| {
        o.get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let id_of = |o: &Value| {
        o.get("optionId")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    options
        .iter()
        .find(|o| kind(o) == "allow_always")
        .or_else(|| options.iter().find(|o| kind(o) == "allow_once"))
        .or_else(|| options.iter().find(|o| kind(o).starts_with("allow")))
        .and_then(id_of)
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use parking_lot::Mutex as ParkingMutex;
    use std::collections::VecDeque;

    fn test_runtime() -> Arc<RuntimeState> {
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _) = tokio::sync::broadcast::channel(8);
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-acp-notification-test-{}.json",
                std::process::id()
            )))
            .expect("temporary auth store should open"),
        );
        RuntimeState::new(
            ws_tx,
            sse_tx,
            Arc::new(ParkingMutex::new(VecDeque::new())),
            Vec::new(),
            auth,
            "test".to_string(),
            crate::turn_loop::openai::ByokConfig {
                base_url: "http://localhost".to_string(),
                model: "test".to_string(),
            },
            false,
            0,
        )
        .0
    }

    /// Drive `handle_notification` over a fixed script of ACP updates and
    /// collect the AG-UI events the browser would actually receive.
    fn project_updates(updates: &[Value]) -> Vec<Value> {
        let rt = test_runtime();
        let mut sse = rt.sse_tx.subscribe();
        let tracked = Mutex::new(HashMap::new());
        for update in updates {
            handle_notification(
                &tracked,
                &rt,
                "session/update",
                &json!({ "params": { "sessionId": "s1", "update": update } }),
            );
        }
        let mut events = Vec::new();
        while let Ok(raw) = sse.try_recv() {
            events.push(serde_json::from_str::<Value>(&raw).expect("emitted valid JSON"));
        }
        events
    }

    fn event_types(events: &[Value]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .collect()
    }

    #[test]
    fn a_tool_call_projects_start_args_end_then_one_result() {
        let events = project_updates(&[
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Read src/lib.rs",
                "kind": "read",
                "status": "pending",
                "rawInput": { "path": "src/lib.rs" }
            }),
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "in_progress"
            }),
            json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "fn main() {}" } }]
            }),
        ]);

        assert_eq!(
            event_types(&events),
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TOOL_CALL_RESULT"
            ]
        );
        // The title is what the human reads, so it is the announced name.
        assert_eq!(events[0]["toolCallName"], json!("Read src/lib.rs"));
        assert_eq!(events[1]["delta"], json!(r#"{"path":"src/lib.rs"}"#));
        assert_eq!(events[3]["content"], json!("fn main() {}"));
    }

    #[test]
    fn repeated_and_post_terminal_updates_never_duplicate_a_tool_call() {
        let completed = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "done" } }]
        });
        let events = project_updates(&[
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Edit",
                "status": "pending",
                "rawInput": { "a": 1 }
            }),
            // A resent creation notification, which ACP permits.
            json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "Edit",
                "status": "pending",
                "rawInput": { "a": 1 }
            }),
            completed.clone(),
            completed,
        ]);

        assert_eq!(
            event_types(&events),
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TOOL_CALL_RESULT"
            ],
            "one execution must render as exactly one tool call"
        );
    }

    #[test]
    fn a_failure_is_reported_as_a_failure_not_an_empty_result() {
        let events = project_updates(&[json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-1",
            "title": "Write",
            "status": "failed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "permission denied" } }]
        })]);

        // No rawInput ever arrived, so END still closes the argument stream.
        assert_eq!(
            event_types(&events),
            vec!["TOOL_CALL_START", "TOOL_CALL_END", "TOOL_CALL_RESULT"]
        );
        assert_eq!(
            events[2]["content"],
            json!("action failed: permission denied")
        );
    }

    #[test]
    fn a_terminal_update_with_no_content_still_says_what_happened() {
        let events = project_updates(&[json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-1",
            "kind": "execute",
            "status": "completed"
        })]);

        assert_eq!(events[0]["toolCallName"], json!("execute"));
        assert_eq!(events.last().unwrap()["content"], json!("action completed"));
    }

    #[test]
    fn response_requires_exactly_one_result_or_well_formed_error() {
        let result = validated_response(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": null
        }));
        assert_eq!(result, Ok((7, Ok(Value::Null))));

        let error = validated_response(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": { "code": -32603, "message": "failed" }
        }));
        assert!(matches!(error, Ok((7, Err(_)))));

        for invalid in [
            json!({ "jsonrpc": "2.0", "id": 7 }),
            json!({ "jsonrpc": "2.0", "id": 7, "result": 1, "error": { "code": -1, "message": "bad" } }),
            json!({ "jsonrpc": "2.0", "id": "7", "result": 1 }),
            json!({ "jsonrpc": "2.0", "id": 7, "error": null }),
            json!({ "jsonrpc": "2.0", "id": 7, "error": { "code": "-1", "message": "bad" } }),
            json!({ "jsonrpc": "2.0", "id": 7, "error": { "code": -1 } }),
        ] {
            assert!(validated_response(&invalid).is_err(), "accepted {invalid}");
        }
    }

    #[tokio::test]
    async fn malformed_response_is_terminal_without_consuming_matching_slot() {
        let pending = Mutex::new(HashMap::new());
        let (tx, rx) = oneshot::channel();
        pending.lock().insert(7, tx);

        let error = dispatch_response(
            &pending,
            &json!({ "jsonrpc": "2.0", "id": 7, "result": 1, "error": { "code": -1, "message": "bad" } }),
        )
        .expect_err("malformed response must fail the protocol");
        assert!(error.contains("exactly one"));
        assert!(pending.lock().contains_key(&7));

        dispatch_response(
            &pending,
            &json!({ "jsonrpc": "2.0", "id": 7, "result": { "ok": true } }),
        )
        .expect("valid response should dispatch");
        assert!(!pending.lock().contains_key(&7));
        assert_eq!(rx.await, Ok(Ok(json!({ "ok": true }))));
    }

    #[tokio::test]
    async fn stdout_frame_reader_rejects_oversized_lines_before_parsing() {
        let input = b"123456789\n";
        let mut reader = BufReader::new(&input[..]);
        let error = read_bounded_line(&mut reader, 8)
            .await
            .expect_err("nine-byte frame must exceed eight-byte bound");
        assert!(error.contains("8-byte limit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_or_invalid_stdout_is_terminal_and_promptly_fails_pending_requests() {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn stdio fixture");
        let stdin = child.stdin.take().expect("fixture stdin");
        let client = AcpClient::new(stdin);
        let (tx, rx) = oneshot::channel();
        client.pending.lock().insert(41, tx);
        let input = b"provider log noise is protocol corruption\n";
        let mut reader = BufReader::new(&input[..]);

        let reason = tokio::time::timeout(
            Duration::from_millis(100),
            consume_acp_stdout(&mut reader, &client, &test_runtime()),
        )
        .await
        .expect("protocol corruption must terminate immediately");
        assert!(reason.contains("malformed JSON on ACP stdout"));
        assert!(client.pending.lock().is_empty());
        assert_eq!(
            rx.await.expect("pending receiver should be resolved"),
            Err(reason)
        );

        let (tx, rx) = oneshot::channel();
        client.pending.lock().insert(42, tx);
        let input = b"{\"jsonrpc\":\"1.0\",\"id\":42,\"result\":{}}\n";
        let mut reader = BufReader::new(&input[..]);
        let reason = tokio::time::timeout(
            Duration::from_millis(100),
            consume_acp_stdout(&mut reader, &client, &test_runtime()),
        )
        .await
        .expect("invalid JSON-RPC must terminate immediately");
        assert!(reason.contains("does not declare jsonrpc=2.0"));
        assert!(client.pending.lock().is_empty());
        assert_eq!(
            rx.await.expect("pending receiver should be resolved"),
            Err(reason)
        );

        child.kill().await.expect("stop stdio fixture");
        let _ = child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_agent_request_receives_standard_method_not_found_error() {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stdio echo fixture");
        let stdin = child.stdin.take().expect("echo stdin");
        let stdout = child.stdout.take().expect("echo stdout");
        let client = AcpClient::new(stdin);

        dispatch(
            &client,
            &test_runtime(),
            json!({
                "jsonrpc": "2.0",
                "id": "agent-request-1",
                "method": "fs/read_text_file",
                "params": {}
            }),
        )
        .await
        .expect("method-not-found response should be written");

        let mut reader = BufReader::new(stdout);
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            read_bounded_line(&mut reader, MAX_ACP_STDOUT_LINE_BYTES),
        )
        .await
        .expect("method-not-found response should be prompt")
        .expect("response line should be readable")
        .expect("response line should exist");
        let response: Value =
            serde_json::from_slice(&response).expect("response should be valid JSON");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "agent-request-1");
        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(response["error"]["message"], "Method not found");
        assert!(response.get("result").is_none());

        child.kill().await.expect("stop echo fixture");
        let _ = child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn agent_response_write_failure_is_propagated() {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn stdio fixture");
        let stdin = child.stdin.take().expect("fixture stdin");
        let client = AcpClient::new(stdin);
        child.kill().await.expect("stop stdio fixture");
        child.wait().await.expect("fixture should be reaped");

        let error = dispatch(
            &client,
            &test_runtime(),
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "unsupported/method",
                "params": {}
            }),
        )
        .await
        .expect_err("closed response transport must surface an error");
        assert!(error.contains("failed responding to ACP request"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_is_sent_only_after_the_prompt_is_on_the_wire() {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stdio echo fixture");
        let stdin = child.stdin.take().expect("echo stdin");
        let stdout = child.stdout.take().expect("echo stdout");
        let client = AcpClient::new(stdin);
        let rt = test_runtime();
        rt.busy.store(true, Ordering::SeqCst);
        let mut cancel = rt.interrupt_tx.subscribe();
        let turn_client = client.clone();
        let turn_rt = rt.clone();
        let turn = tokio::spawn(async move {
            run_turn(&turn_client, &turn_rt, "session-1", "question", &mut cancel).await
        });

        let mut stdout = BufReader::new(stdout);
        let prompt_line = tokio::time::timeout(
            Duration::from_secs(1),
            read_bounded_line(&mut stdout, MAX_ACP_STDOUT_LINE_BYTES),
        )
        .await
        .expect("prompt should be written")
        .expect("read prompt line")
        .expect("prompt line should exist");
        let prompt: Value = serde_json::from_slice(&prompt_line).expect("valid prompt JSON");
        assert_eq!(prompt["method"], "session/prompt");
        let request_id = prompt["id"].as_i64().expect("integer request id");

        assert_eq!(rt.interrupt_tx.send(()).unwrap_or(0), 1);
        let cancel_line = tokio::time::timeout(
            Duration::from_secs(1),
            read_bounded_line(&mut stdout, MAX_ACP_STDOUT_LINE_BYTES),
        )
        .await
        .expect("cancel should be written")
        .expect("read cancel line")
        .expect("cancel line should exist");
        let cancellation: Value =
            serde_json::from_slice(&cancel_line).expect("valid cancellation JSON");
        assert_eq!(cancellation["method"], "session/cancel");

        dispatch_response(
            &client.pending,
            &json!({ "jsonrpc": "2.0", "id": request_id, "result": {} }),
        )
        .expect("prompt response should dispatch");
        assert!(turn.await.expect("turn task should finish").is_ok());
        assert!(!rt.busy.load(Ordering::SeqCst));
        assert!(rt.stream_active_session.lock().is_none());

        child.kill().await.expect("stop echo fixture");
        let _ = child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_queued_before_provider_dequeue_sends_no_prompt() {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn stdio echo fixture");
        let stdin = child.stdin.take().expect("echo stdin");
        let stdout = child.stdout.take().expect("echo stdout");
        let client = AcpClient::new(stdin);
        let rt = test_runtime();
        rt.busy.store(true, Ordering::SeqCst);
        let mut cancel = rt.interrupt_tx.subscribe();
        assert_eq!(rt.interrupt_tx.send(()).unwrap_or(0), 1);

        run_turn(&client, &rt, "session-1", "question", &mut cancel)
            .await
            .expect("pre-start cancellation should settle cleanly");
        assert!(!rt.busy.load(Ordering::SeqCst));
        let mut stdout = BufReader::new(stdout);
        assert!(tokio::time::timeout(
            Duration::from_millis(100),
            read_bounded_line(&mut stdout, MAX_ACP_STDOUT_LINE_BYTES),
        )
        .await
        .is_err());

        child.kill().await.expect("stop echo fixture");
        let _ = child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backpressured_prompt_write_times_out_and_cleans_pending_request() {
        let mut child = tokio::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn stdio fixture");
        let stdin = child.stdin.take().expect("fixture stdin");
        let client = AcpClient::new(stdin);
        let held_writer = client.stdin.lock().await;

        let error = client
            .start_request_with_timeout(
                "session/prompt",
                json!({ "sessionId": "session-1", "prompt": [] }),
                Duration::from_millis(25),
            )
            .await
            .expect_err("blocked writer must be bounded");
        assert!(error.contains("write did not complete"));
        assert!(client.pending.lock().is_empty());

        drop(held_writer);
        child.kill().await.expect("stop stdio fixture");
        let _ = child.wait().await;
    }

    #[test]
    fn cancelled_turn_cannot_append_late_chunk_after_next_turn_is_queued() {
        let rt = test_runtime();
        rt.warming.store(false, Ordering::Relaxed);
        rt.busy.store(true, Ordering::SeqCst);
        *rt.stream_active_session.lock() = Some("session-old".to_string());
        let old_update = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "session-old",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "text": "late old text" }
                }
            }
        });

        // Pause after the reader owns the append boundary but before it checks
        // authorization. Revoke the old prompt and simulate the queued next
        // turn (`busy=true`), then resume. A check performed before the append
        // boundary would append the stale text and fail these assertions.
        let (at_boundary_tx, at_boundary_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let reader_rt = rt.clone();
        let reader = std::thread::spawn(move || {
            let text = old_update["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap();
            crate::narration::narrate_provider_chunk_with_boundary_hook(
                &reader_rt,
                "session-old",
                text,
                || {
                    at_boundary_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                },
            );
        });
        at_boundary_rx.recv().unwrap();
        *rt.stream_active_session.lock() = None;
        rt.busy.store(false, Ordering::SeqCst);
        rt.busy.store(true, Ordering::SeqCst);
        resume_tx.send(()).unwrap();
        reader.join().unwrap();

        assert!(rt.active_message.lock().is_none());
        assert!(rt.narration_buf.lock().is_empty());
        assert!(rt.history.lock().is_empty());

        *rt.stream_active_session.lock() = Some("session-new".to_string());
        handle_notification(
            &Mutex::new(HashMap::new()),
            &rt,
            "session/update",
            &json!({
                "params": {
                    "sessionId": "session-new",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "text": "new turn text" }
                    }
                }
            }),
        );
        assert_eq!(
            rt.active_message
                .lock()
                .as_ref()
                .map(|message| message.text.as_str()),
            Some("new turn text")
        );
    }
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    #[test]
    fn http_mcp_is_negotiated_and_stdio_is_only_a_fallback() {
        let http = json!({
            "agentCapabilities": { "mcpCapabilities": { "http": true, "sse": false } }
        });
        let legacy = McpBridge {
            name: "legacy".to_string(),
            program: "legacy-mcp".to_string(),
            args: vec!["--stdio".to_string()],
            env: vec![("TOKEN".to_string(), "x".to_string())],
        };
        let descriptor = mcp_server_descriptor_for(&http, 8123, "secret", Some(&legacy)).unwrap();
        assert_eq!(descriptor["type"], "http");
        assert_eq!(descriptor["url"], "http://127.0.0.1:8123/mcp");
        assert_eq!(descriptor["headers"][0]["value"], "Bearer secret");
        assert!(descriptor.get("command").is_none());

        let no_http = json!({
            "agentCapabilities": { "mcpCapabilities": { "http": false } }
        });
        let descriptor =
            mcp_server_descriptor_for(&no_http, 8123, "secret", Some(&legacy)).unwrap();
        assert_eq!(descriptor["command"], "legacy-mcp");
        assert_eq!(descriptor["args"], json!(["--stdio"]));
        assert!(mcp_server_descriptor_for(&no_http, 8123, "secret", None).is_none());
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use tokio::io::AsyncBufReadExt as _;

    async fn wait_for_process_exit(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let result = unsafe { libc::kill(pid, 0) };
            if result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn spawn_wrapper_process_group() -> (ManagedAgentProcess, libc::pid_t, libc::pid_t) {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30 & printf '%s\\n' \"$!\"; wait"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        isolate_process_group(&mut cmd);

        let mut child = cmd.spawn().expect("spawn process-group wrapper");
        let parent_pid = i32::try_from(child.id().expect("wrapper pid")).expect("pid fits i32");
        let stdout = child.stdout.take().expect("wrapper stdout");
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            BufReader::new(stdout).read_line(&mut line),
        )
        .await
        .expect("wrapper reported grandchild pid")
        .expect("read grandchild pid");
        let grandchild_pid: libc::pid_t = line.trim().parse().expect("numeric grandchild pid");

        assert_eq!(
            unsafe { libc::getpgid(grandchild_pid) },
            parent_pid,
            "wrapper descendant must inherit the isolated process group"
        );

        (ManagedAgentProcess::new(child), parent_pid, grandchild_pid)
    }

    #[tokio::test]
    async fn shutdown_kills_and_reaps_wrapper_process_group() {
        let (mut process, parent_pid, grandchild_pid) = spawn_wrapper_process_group().await;
        process.shutdown().await;

        assert!(
            wait_for_process_exit(parent_pid).await,
            "direct wrapper process survived shutdown"
        );
        assert!(
            wait_for_process_exit(grandchild_pid).await,
            "wrapper descendant survived shutdown"
        );
        assert!(
            !process_group_exists(parent_pid),
            "ACP process group survived shutdown"
        );
    }

    #[tokio::test]
    async fn drop_kills_wrapper_process_group_when_host_task_is_cancelled() {
        let (process, parent_pid, grandchild_pid) = spawn_wrapper_process_group().await;
        drop(process);

        assert!(
            wait_for_process_exit(parent_pid).await,
            "direct wrapper process survived guard drop"
        );
        assert!(
            wait_for_process_exit(grandchild_pid).await,
            "wrapper descendant survived guard drop"
        );
        assert!(
            !process_group_exists(parent_pid),
            "ACP process group survived guard drop"
        );
    }
}
