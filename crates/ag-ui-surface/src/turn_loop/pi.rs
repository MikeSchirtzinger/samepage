//! Pi RPC backend.
//!
//! Pi remains a normal coding agent with an explicit built-in-tool allowlist.
//! An ephemeral extension registers the effective agent-visible Surface
//! actions and forwards them to the runtime-owned authenticated MCP endpoint.
//! No action implementation or schema is copied into this adapter.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ag_ui_core::types::MessageId;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as AsyncMutex};
use tracing::{info, warn};

use super::acp::{isolate_process_group, ManagedAgentProcess};
use super::{build_context, Outcome, Prompts};
use crate::narration::{finish_turn, narrate_cancel, narrate_flush, reset_narration};
use crate::runtime_state::{RuntimeState, TurnRequest};
use crate::Surface;

const BRIDGE_SOURCE: &str = include_str!("pi_bridge.mjs");
const MAX_STDOUT_LINE_BYTES: usize = 1024 * 1024;
const SETUP_TIMEOUT: Duration = Duration::from_secs(45);
const PRIME_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(6);
const DIALOG_METHODS: &[&str] = &["select", "confirm", "input", "editor"];

async fn verify_pi_version(program: &str) -> Result<(), String> {
    let mut command = tokio::process::Command::new(program);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(|_| format!("{program} --version timed out"))?
        .map_err(|error| format!("failed to run {program} --version: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} --version exited with {}", output.status));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| format!("{program} --version returned non-UTF-8 output"))?;
    let version = version.trim();
    if !crate::providers::pi_rpc_supported(version) {
        return Err(format!(
            "Pi RPC adapter has been checked against Pi {}, but {program} reports {version:?}. \
             Re-run the protocol probe against this version and add it to PI_RPC_VERSIONS; \
             assuming compatibility would present as an agent that connects and then streams nothing.",
            crate::providers::PI_RPC_VERSIONS.join(" and ")
        ));
    }
    Ok(())
}

struct PendingResponse {
    command: String,
    tx: oneshot::Sender<Result<Value, String>>,
}

struct PendingGuard<'a> {
    pending: &'a Mutex<HashMap<String, PendingResponse>>,
    id: String,
    armed: bool,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.pending.lock().remove(&self.id);
        }
    }
}

struct PiClient {
    stdin: AsyncMutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<String, PendingResponse>>,
}

impl PiClient {
    fn new(stdin: ChildStdin) -> Arc<Self> {
        Arc::new(Self {
            stdin: AsyncMutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        })
    }

    async fn request(
        &self,
        command: &str,
        fields: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = format!("ag-ui-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut object = fields
            .as_object()
            .cloned()
            .ok_or_else(|| format!("Pi RPC {command} fields must be an object"))?;
        object.insert("id".into(), Value::String(id.clone()));
        object.insert("type".into(), Value::String(command.to_string()));

        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(
            id.clone(),
            PendingResponse {
                command: command.to_string(),
                tx,
            },
        );
        let _guard = PendingGuard {
            pending: &self.pending,
            id,
            armed: true,
        };

        tokio::time::timeout(CANCEL_TIMEOUT, self.write_line(&Value::Object(object)))
            .await
            .map_err(|_| format!("Pi RPC {command} write timed out"))??;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!("Pi RPC closed before replying to {command}")),
            Err(_) => Err(format!(
                "Pi RPC did not reply to {command} within {}s",
                timeout.as_secs()
            )),
        }
    }

    async fn write_line(&self, value: &Value) -> Result<(), String> {
        let mut line = value.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("failed writing Pi RPC stdin: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("failed flushing Pi RPC stdin: {error}"))
    }

    async fn cancel_dialog(&self, id: &str) -> Result<(), String> {
        self.write_line(&json!({
            "type": "extension_ui_response",
            "id": id,
            "cancelled": true
        }))
        .await
    }

    async fn close(&self) {
        let _ = self.stdin.lock().await.shutdown().await;
    }

    fn fail_all(&self, reason: &str) {
        for (_, pending) in self.pending.lock().drain() {
            let _ = pending.tx.send(Err(reason.to_string()));
        }
    }
}

fn dispatch_response(
    pending: &Mutex<HashMap<String, PendingResponse>>,
    message: &Value,
) -> Result<(), String> {
    let object = message
        .as_object()
        .ok_or_else(|| "Pi RPC response is not an object".to_string())?;
    if object.get("type").and_then(Value::as_str) != Some("response") {
        return Err("Pi RPC response has the wrong type".to_string());
    }
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Pi RPC response id is not a string".to_string())?;
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "Pi RPC response command is not a string".to_string())?;
    let success = object
        .get("success")
        .and_then(Value::as_bool)
        .ok_or_else(|| "Pi RPC response success is not a boolean".to_string())?;

    let Some(pending) = pending.lock().remove(id) else {
        warn!(id, "discarding Pi RPC response for unknown request id");
        return Ok(());
    };
    let result = if command != pending.command {
        Err(format!(
            "Pi RPC response command {command:?} did not match {:?}",
            pending.command
        ))
    } else if success {
        Ok(message.clone())
    } else {
        let error = object
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown Pi RPC error");
        Err(format!("Pi RPC {command} failed: {error}"))
    };
    let _ = pending.tx.send(result);
    Ok(())
}

async fn consume_stdout<R>(
    reader: &mut R,
    client: &Arc<PiClient>,
    event_tx: mpsc::UnboundedSender<Value>,
) -> String
where
    R: AsyncBufRead + Unpin,
{
    let reason = loop {
        let mut line =
            match super::read_bounded_jsonl_line(reader, MAX_STDOUT_LINE_BYTES, "Pi RPC").await {
                Ok(Some(line)) => line,
                Ok(None) => break "Pi RPC stdout closed".to_string(),
                Err(error) => break error,
            };
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }

        let message = match serde_json::from_slice::<Value>(&line) {
            Ok(message) => message,
            Err(error) => break format!("malformed JSON on Pi RPC stdout: {error}"),
        };
        let Some(kind) = message.get("type").and_then(Value::as_str) else {
            break "Pi RPC stdout message has no string type".to_string();
        };
        if kind == "response" {
            if let Err(error) = dispatch_response(&client.pending, &message) {
                break error;
            }
            continue;
        }

        if kind == "extension_ui_request" {
            let method = message.get("method").and_then(Value::as_str).unwrap_or("");
            if DIALOG_METHODS.contains(&method) {
                let Some(id) = message.get("id").and_then(Value::as_str) else {
                    break "Pi RPC extension dialog has no string id".to_string();
                };
                if let Err(error) = client.cancel_dialog(id).await {
                    break error;
                }
                continue;
            }
        }

        if event_tx.send(message).is_err() {
            break "Pi RPC event consumer closed".to_string();
        }
    };

    client.fail_all(&reason);
    reason
}

struct BridgeFile {
    path: PathBuf,
    removed: bool,
}

impl BridgeFile {
    fn create() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "ag-ui-pi-bridge-{}-{}.mjs",
            std::process::id(),
            MessageId::random()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| format!("failed creating Pi bridge {}: {error}", path.display()))?;
        file.write_all(BRIDGE_SOURCE.as_bytes())
            .map_err(|error| format!("failed writing Pi bridge {}: {error}", path.display()))?;
        file.sync_all()
            .map_err(|error| format!("failed syncing Pi bridge {}: {error}", path.display()))?;
        Ok(Self {
            path,
            removed: false,
        })
    }

    fn remove(&mut self) -> Result<(), String> {
        if self.removed {
            return Ok(());
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                self.removed = true;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.removed = true;
                Ok(())
            }
            Err(error) => Err(format!(
                "failed removing ephemeral Pi bridge {}: {error}",
                self.path.display()
            )),
        }
    }
}

impl Drop for BridgeFile {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            warn!(%error);
        }
    }
}

fn extension_error(event: &Value) -> Option<String> {
    (event.get("type").and_then(Value::as_str) == Some("extension_error")).then(|| {
        event
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown extension error")
            .to_string()
    })
}

async fn wait_for_bridge_status(
    events: &mut mpsc::UnboundedReceiver<Value>,
    expected: &str,
    timeout: Duration,
) -> Result<(), String> {
    tokio::time::timeout(timeout, async {
        loop {
            let event = events
                .recv()
                .await
                .ok_or_else(|| "Pi RPC event stream closed during bridge setup".to_string())?;
            if let Some(error) = extension_error(&event) {
                return Err(format!("Pi bridge extension failed: {error}"));
            }
            if event.get("type").and_then(Value::as_str) == Some("extension_ui_request")
                && event.get("method").and_then(Value::as_str) == Some("setStatus")
                && event.get("statusKey").and_then(Value::as_str) == Some("ag-ui-bridge")
                && event.get("statusText").and_then(Value::as_str) == Some(expected)
            {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| {
        format!(
            "Pi bridge did not report {expected:?} within {}s",
            timeout.as_secs()
        )
    })?
}

fn model_identity(state: &Value) -> Result<String, String> {
    let model = state
        .pointer("/data/model")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            "Pi has no selected model; configure Pi before using this provider".to_string()
        })?;
    let provider = model
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("unknown-provider");
    let id = model
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Pi selected model has no id".to_string())?;
    if state.pointer("/data/isStreaming").and_then(Value::as_bool) != Some(false) {
        return Err("Pi RPC started in an unexpected streaming state".to_string());
    }
    Ok(format!("{provider}/{id}"))
}

fn required_surface_tool_arguments<'a>(
    event: &'a Value,
    name: &str,
    raw_id: &str,
) -> Result<&'a Value, String> {
    event.get("args").ok_or_else(|| {
        format!(
            "Pi tool_execution_start event for Surface action {name:?} \
             ({raw_id}) has no arguments"
        )
    })
}

fn emit_surface_tool_event(
    rt: &RuntimeState,
    event: &Value,
    surface_tools: &HashSet<String>,
) -> Result<(), String> {
    let Some(kind) = event.get("type").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(name) = event.get("toolName").and_then(Value::as_str) else {
        return Ok(());
    };
    if !surface_tools.contains(name) {
        return Ok(());
    }
    let raw_id = event
        .get("toolCallId")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Pi {kind} event for Surface action {name:?} has no toolCallId"))?;
    match kind {
        // Pi reports a call only once its arguments are already complete, so
        // this adapter cannot stream them the way `super::openai` does: one
        // delta carries the whole payload. The event shape stays identical, so
        // a client renders both adapters through the same path.
        "tool_execution_start" => {
            let args = required_surface_tool_arguments(event, name, raw_id)?;
            super::emit_tool_call_start(rt, raw_id, name);
            super::emit_tool_call_args(rt, raw_id, &args.to_string());
            super::emit_tool_call_end(rt, raw_id);
        }
        "tool_execution_end" => {
            let mut text = event
                .pointer("/result/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    (item.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| item.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                text = "action returned non-text content".to_string();
            }
            if event.get("isError").and_then(Value::as_bool) == Some(true) {
                text = format!("action failed: {text}");
            }
            super::emit_tool_call_result(rt, raw_id, &text);
        }
        _ => {}
    }
    Ok(())
}

async fn wait_for_settled(
    rt: &Arc<RuntimeState>,
    events: &mut mpsc::UnboundedReceiver<Value>,
    surface_tools: &HashSet<String>,
    stream_session: Option<&str>,
    require_start: bool,
    allow_abort: bool,
    timeout: Duration,
) -> Result<String, String> {
    tokio::time::timeout(timeout, async {
        let mut started = !require_start;
        let mut text = String::new();
        let mut model_error: Option<String> = None;
        loop {
            let event = events
                .recv()
                .await
                .ok_or_else(|| "Pi RPC event stream closed during a turn".to_string())?;
            if let Some(error) = extension_error(&event) {
                return Err(format!("Pi extension failed: {error}"));
            }
            emit_surface_tool_event(rt, &event, surface_tools)?;

            match event.get("type").and_then(Value::as_str) {
                Some("agent_start") => started = true,
                Some("agent_settled") if started => {
                    if let Some(error) = model_error {
                        if !allow_abort {
                            return Err(error);
                        }
                    }
                    return Ok(text);
                }
                Some("message_update") => {
                    let update = event.get("assistantMessageEvent");
                    match update
                        .and_then(|value| value.get("type"))
                        .and_then(Value::as_str)
                    {
                        Some("text_delta") => {
                            if let Some(delta) = update
                                .and_then(|value| value.get("delta"))
                                .and_then(Value::as_str)
                            {
                                text.push_str(delta);
                                if let Some(session) = stream_session {
                                    crate::narration::narrate_provider_chunk(rt, session, delta);
                                }
                            }
                        }
                        Some("error") => {
                            let reason = update
                                .and_then(|value| value.get("reason"))
                                .and_then(Value::as_str)
                                .unwrap_or("model error");
                            model_error = Some(format!("Pi model turn failed: {reason}"));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| format!("Pi turn did not settle within {}s", timeout.as_secs()))?
}

struct ActivePiTurn {
    rt: Arc<RuntimeState>,
    token: String,
    active: bool,
}

impl ActivePiTurn {
    fn begin(rt: &Arc<RuntimeState>, session_id: &str) -> Self {
        let token = format!("{session_id}:{}", MessageId::random());
        let _replay_guard = rt.transcript_replay_lock.lock();
        *rt.stream_active_session.lock() = Some(token.clone());
        Self {
            rt: rt.clone(),
            token,
            active: true,
        }
    }

    fn clear(&mut self) {
        if !self.active {
            return;
        }
        let _replay_guard = self.rt.transcript_replay_lock.lock();
        let mut active = self.rt.stream_active_session.lock();
        if active.as_deref() == Some(self.token.as_str()) {
            *active = None;
        }
        self.active = false;
    }
}

impl Drop for ActivePiTurn {
    fn drop(&mut self) {
        self.clear();
    }
}

async fn run_turn(
    client: &Arc<PiClient>,
    rt: &Arc<RuntimeState>,
    events: &mut mpsc::UnboundedReceiver<Value>,
    surface_tools: &HashSet<String>,
    session_id: &str,
    text: &str,
    cancel: &mut broadcast::Receiver<()>,
) -> Result<(), String> {
    let mut active = ActivePiTurn::begin(rt, session_id);
    match cancel.try_recv() {
        Ok(()) | Err(broadcast::error::TryRecvError::Lagged(_)) => {
            active.clear();
            narrate_cancel(rt);
            finish_turn(rt);
            return Ok(());
        }
        Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {}
    }

    client
        .request("prompt", json!({ "message": text }), CANCEL_TIMEOUT)
        .await?;
    let stream_token = active.token.clone();
    let mut settle = Box::pin(wait_for_settled(
        rt,
        events,
        surface_tools,
        Some(&stream_token),
        true,
        false,
        REQUEST_TIMEOUT,
    ));

    let settled = tokio::select! {
        biased;
        _ = cancel.recv() => None,
        result = &mut settle => Some(result),
    };
    drop(settle);

    match settled {
        None => {
            active.clear();
            narrate_cancel(rt);
            let cancellation = async {
                client.request("abort", json!({}), CANCEL_TIMEOUT).await?;
                wait_for_settled(rt, events, surface_tools, None, false, true, CANCEL_TIMEOUT)
                    .await?;
                Ok::<(), String>(())
            };
            tokio::time::timeout(CANCEL_TIMEOUT, cancellation)
                .await
                .map_err(|_| {
                    "Pi did not acknowledge cancellation within 6 seconds; provider session must restart"
                        .to_string()
                })??;
            finish_turn(rt);
            Ok(())
        }
        Some(result) => {
            active.clear();
            result?;
            narrate_flush(rt);
            finish_turn(rt);
            Ok(())
        }
    }
}

fn build_prime(prompts: &Prompts, surface: &dyn Surface) -> String {
    let mut prompt = build_context(prompts, surface);
    prompt.push_str(
        "\n\nThis first message is setup only. You have no active tools. \
         Do not take any actions. Reply with the single word READY.",
    );
    prompt
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_one(
    rt: &Arc<RuntimeState>,
    surface: &Arc<dyn Surface>,
    prompts: &Arc<Prompts>,
    agent_cwd: &Path,
    provider_id: &str,
    provider_auth: &str,
    program: String,
    args: Vec<String>,
    builtin_tools: Vec<String>,
    ask_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    mission_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    switch_rx: &mut mpsc::UnboundedReceiver<String>,
) -> Outcome {
    rt.ready.store(false, Ordering::Relaxed);
    rt.warming.store(true, Ordering::Relaxed);
    crate::narration::tutor_event(rt, "warming", None);

    if let Err(error) = verify_pi_version(&program).await {
        warn!("{error}");
        return Outcome::Exited;
    }

    let catalog = match crate::mcp::Catalog::from_actions(surface.tools()) {
        Ok(catalog) => catalog,
        Err(error) => {
            warn!("cannot build Pi Surface catalog: {error}");
            return Outcome::Exited;
        }
    };
    let wire_tools = catalog.wire_tools();
    let surface_tools: HashSet<String> = wire_tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut allowed_tools = builtin_tools;
    for name in &surface_tools {
        if allowed_tools.iter().any(|builtin| builtin == name) {
            warn!("Pi Surface action {name:?} collides with an allowed built-in tool");
            return Outcome::Exited;
        }
        allowed_tools.push(name.clone());
    }
    if allowed_tools.iter().any(|name| {
        name.is_empty() || name.contains(',') || name.bytes().any(|byte| byte.is_ascii_whitespace())
    }) {
        warn!("Pi tool allowlist contains an invalid name");
        return Outcome::Exited;
    }
    let mut unique = HashSet::new();
    if !allowed_tools.iter().all(|name| unique.insert(name.clone())) {
        warn!("Pi tool allowlist contains a duplicate name");
        return Outcome::Exited;
    }

    let mut bridge = match BridgeFile::create() {
        Ok(bridge) => bridge,
        Err(error) => {
            warn!("{error}");
            return Outcome::Exited;
        }
    };
    let catalog_json = match serde_json::to_string(&wire_tools) {
        Ok(value) => value,
        Err(error) => {
            warn!("failed serializing Pi Surface catalog: {error}");
            return Outcome::Exited;
        }
    };
    let allowed_tools_json = match serde_json::to_string(&allowed_tools) {
        Ok(value) => value,
        Err(error) => {
            warn!("failed serializing Pi tool allowlist: {error}");
            return Outcome::Exited;
        }
    };
    let nonce = MessageId::random().to_string();
    info!(
        provider = provider_id,
        adapter = "pi-rpc",
        surface_tools = surface_tools.len(),
        builtin_tools = allowed_tools.len() - surface_tools.len(),
        "starting managed provider ({provider_auth})"
    );

    let mut command = tokio::process::Command::new(&program);
    command
        .args(&args)
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--approve",
        ])
        .arg("--extension")
        .arg(&bridge.path)
        .arg("--tools")
        .arg(allowed_tools.join(","))
        .current_dir(agent_cwd)
        .env(
            "AG_UI_PI_MCP_URL",
            format!("http://127.0.0.1:{}/mcp", rt.port),
        )
        .env("AG_UI_PI_MCP_TOKEN", &rt.mcp_token)
        .env("AG_UI_PI_BRIDGE_NONCE", &nonce)
        .env("AG_UI_PI_ACTION_CATALOG", catalog_json)
        .env("AG_UI_PI_ALLOWED_TOOLS", allowed_tools_json)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    isolate_process_group(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            warn!("failed to spawn Pi provider {program:?}: {error}");
            return Outcome::Exited;
        }
    };
    let Some(stdin) = child.stdin.take() else {
        warn!("Pi provider spawned without piped stdin");
        let mut process = ManagedAgentProcess::new(child);
        process.shutdown().await;
        return Outcome::Exited;
    };
    let Some(stdout) = child.stdout.take() else {
        warn!("Pi provider spawned without piped stdout");
        let mut process = ManagedAgentProcess::new(child);
        process.shutdown().await;
        return Outcome::Exited;
    };
    let mut process = ManagedAgentProcess::new(child);
    let client = PiClient::new(stdin);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let reader_client = client.clone();
    let (reader_closed_tx, mut reader_closed_rx) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let reason = consume_stdout(&mut BufReader::new(stdout), &reader_client, event_tx).await;
        warn!(%reason, "Pi RPC stdout reader terminated");
        let _ = reader_closed_tx.send(());
    });

    let provider_session = async {
        let ready_status = format!("ready:{nonce}");
        if let Err(error) =
            wait_for_bridge_status(&mut event_rx, &ready_status, SETUP_TIMEOUT).await
        {
            warn!("Pi bridge setup failed: {error}");
            return Outcome::Exited;
        }
        if let Err(error) = bridge.remove() {
            warn!("{error}");
        }

        let state = match client.request("get_state", json!({}), SETUP_TIMEOUT).await {
            Ok(state) => state,
            Err(error) => {
                warn!("Pi get_state failed: {error}");
                return Outcome::Exited;
            }
        };
        let model = match model_identity(&state) {
            Ok(model) => model,
            Err(error) => {
                warn!("{error}");
                return Outcome::Exited;
            }
        };
        let session_id = state
            .pointer("/data/sessionId")
            .and_then(Value::as_str)
            .unwrap_or("ephemeral")
            .to_string();
        info!(provider = provider_id, %model, %session_id, "Pi RPC model selected");

        let prime = build_prime(prompts, surface.as_ref());
        if let Err(error) = client
            .request("prompt", json!({ "message": prime }), CANCEL_TIMEOUT)
            .await
        {
            warn!("Pi prime was not accepted: {error}");
            return Outcome::Exited;
        }
        let prime_text = match wait_for_settled(
            rt,
            &mut event_rx,
            &surface_tools,
            None,
            true,
            false,
            PRIME_TIMEOUT,
        )
        .await
        {
            Ok(text) => text,
            Err(error) => {
                warn!("Pi prime failed: {error}");
                return Outcome::Exited;
            }
        };
        if prime_text.trim() != "READY" {
            warn!(
                response = %prime_text.trim(),
                "Pi setup response was not the required READY handshake"
            );
            return Outcome::Exited;
        }

        if let Err(error) = client
            .request(
                "prompt",
                json!({ "message": "/ag-ui-activate" }),
                SETUP_TIMEOUT,
            )
            .await
        {
            warn!("Pi tool activation command failed: {error}");
            return Outcome::Exited;
        }
        let active_status = format!("active:{nonce}");
        if let Err(error) =
            wait_for_bridge_status(&mut event_rx, &active_status, SETUP_TIMEOUT).await
        {
            warn!("Pi tool activation was not acknowledged: {error}");
            return Outcome::Exited;
        }

        reset_narration(rt);
        rt.warming.store(false, Ordering::Relaxed);
        rt.ready.store(true, Ordering::Relaxed);
        crate::narration::tutor_event(rt, "ready", None);
        info!(provider = provider_id, %model, "Pi RPC provider ready after a real prime");

        loop {
            tokio::select! {
                maybe_question = ask_rx.recv() => {
                    let Some(mut question) = maybe_question else {
                        return Outcome::Exited;
                    };
                    if let Err(error) = run_turn(
                        &client,
                        rt,
                        &mut event_rx,
                        &surface_tools,
                        &session_id,
                        &question.text,
                        &mut question.cancel,
                    ).await {
                        warn!("Pi prompt failed: {error}");
                        return Outcome::Exited;
                    }
                }
                maybe_mission = mission_rx.recv() => {
                    let Some(mut mission) = maybe_mission else {
                        return Outcome::Exited;
                    };
                    let text = (prompts.mission)(&mission.text);
                    if let Err(error) = run_turn(
                        &client,
                        rt,
                        &mut event_rx,
                        &surface_tools,
                        &session_id,
                        &text,
                        &mut mission.cancel,
                    ).await {
                        warn!("Pi mission failed: {error}");
                        return Outcome::Exited;
                    }
                }
            }
        }
    };
    let mut provider_session = Box::pin(provider_session);
    let outcome = tokio::select! {
        biased;
        maybe_switch = switch_rx.recv() => {
            rt.ready.store(false, Ordering::Relaxed);
            rt.warming.store(true, Ordering::Relaxed);
            narrate_cancel(rt);
            rt.busy.store(false, Ordering::SeqCst);
            match maybe_switch {
                Some(new_id) => Outcome::Switch(new_id),
                None => Outcome::Exited,
            }
        }
        _ = &mut reader_closed_rx => Outcome::Exited,
        outcome = &mut provider_session => outcome,
    };

    drop(provider_session);
    reader.abort();
    let _ = reader.await;
    let _ = tokio::time::timeout(Duration::from_millis(500), client.close()).await;
    process.shutdown().await;
    drop(bridge);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_validation_is_correlated_and_fail_closed() {
        let pending = Mutex::new(HashMap::new());
        let (tx, rx) = oneshot::channel();
        pending.lock().insert(
            "one".to_string(),
            PendingResponse {
                command: "get_state".to_string(),
                tx,
            },
        );
        dispatch_response(
            &pending,
            &json!({
                "id": "one",
                "type": "response",
                "command": "prompt",
                "success": true
            }),
        )
        .expect("syntactically valid response");
        assert!(rx
            .blocking_recv()
            .expect("response delivered")
            .expect_err("mismatched command must fail")
            .contains("did not match"));

        assert!(dispatch_response(
            &Mutex::new(HashMap::new()),
            &json!({"type":"response","command":"prompt","success":true})
        )
        .is_err());
    }

    #[test]
    fn bridge_locks_tools_and_has_no_fallback_action_path() {
        assert!(BRIDGE_SOURCE.contains("pi.setActiveTools([])"));
        assert!(BRIDGE_SOURCE.contains("/mcp") || BRIDGE_SOURCE.contains("tools/call"));
        assert!(BRIDGE_SOURCE.contains("AG-UI actions are locked until setup completes"));
        assert!(!BRIDGE_SOURCE.contains("fallback"));
    }

    #[test]
    fn model_state_requires_a_selected_idle_model() {
        let state = json!({
            "data": {
                "model": { "provider": "example", "id": "real-model" },
                "isStreaming": false
            }
        });
        assert_eq!(model_identity(&state), Ok("example/real-model".to_string()));
        assert!(model_identity(&json!({"data":{"model":null,"isStreaming":false}})).is_err());
        assert!(model_identity(&json!({
            "data": {
                "model": { "provider": "example", "id": "real-model" },
                "isStreaming": true
            }
        }))
        .is_err());
    }

    #[test]
    fn surface_tool_start_requires_real_arguments() {
        let missing = json!({
            "type": "tool_execution_start",
            "toolName": "draw",
            "toolCallId": "call-1"
        });
        let error = required_surface_tool_arguments(&missing, "draw", "call-1")
            .expect_err("missing arguments must reject the Pi event");
        assert!(error.contains("has no arguments"));

        let present = json!({
            "type": "tool_execution_start",
            "toolName": "draw",
            "toolCallId": "call-1",
            "args": { "x": 4 }
        });
        assert_eq!(
            required_surface_tool_arguments(&present, "draw", "call-1"),
            Ok(&json!({ "x": 4 }))
        );
    }

    #[test]
    fn ephemeral_bridge_is_removed_explicitly() {
        let mut bridge = BridgeFile::create().expect("create ephemeral bridge");
        assert!(bridge.path.is_file());
        bridge.remove().expect("remove ephemeral bridge");
        assert!(!bridge.path.exists());
    }
}
