//! The generic, provider-agnostic turn loop — lifted from teaching-canvas's
//! `acp.rs`/`openai.rs` (M2 Phase C; see `docs/ag-ui-surface-m2-handoff.md`
//! §4 steps 5-6). Three backends share one supervisor: ACP and Pi RPC
//! subprocesses plus an in-process OpenAI-compatible HTTP client.
//! Both drive the SAME turn-loop contract — prime, serve `ask`/`mission`
//! turns, watch for a provider switch, narrate + support barge-in identically
//! (via [`crate::narration`], written once — see its module docs) — over
//! `&dyn Surface`/`&dyn SurfaceState` instead of teaching-canvas's `AppState`.
//!
//! This is the active turn loop used by teaching-canvas, Vellum, Colab, and the
//! starter through [`crate::App`].

pub mod acp;
pub mod openai;
mod pi;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use ag_ui_core::event::{
    BaseEvent, Event as AgUiEvent, ToolCallArgsEvent, ToolCallEndEvent, ToolCallResultEvent,
    ToolCallStartEvent,
};
use ag_ui_core::types::{MessageId, Role, ToolCallId};
use ag_ui_core::JsonValue;

use crate::runtime_state::{PendingDecision, RuntimeState, TurnRequest};
use crate::{providers, Caller, Effect, Surface};

/// Announce that the agent has begun emitting a tool call, before its
/// arguments are known.
///
/// Every adapter must call this the moment a call's id and name are readable,
/// **not** once the call is complete. A model composing a large structured
/// argument payload can hold the stream for a minute or more (measured: 98s
/// for one `publish_project_walkthrough`), and for that whole window the
/// browser is the only thing the human can see. Announcing late is what makes
/// a working turn look like a hung one.
pub(crate) fn emit_tool_call_start(rt: &RuntimeState, tool_call_id: &str, tool_call_name: &str) {
    let run_id = rt.current_activity_run.lock().clone();
    rt.activity.record_tool(
        "tool.started",
        crate::ActivityOutcome::Started,
        run_id.as_deref(),
        tool_call_id,
        Some(tool_call_name),
    );
    crate::narration::emit_event(
        rt,
        AgUiEvent::<JsonValue>::ToolCallStart(ToolCallStartEvent {
            base: BaseEvent::default(),
            tool_call_id: ToolCallId::new(tool_call_id),
            tool_call_name: tool_call_name.to_string(),
            parent_message_id: None,
        }),
    );
}

/// Forward one incremental slice of a tool call's arguments.
///
/// `delta` is an append to the argument text, exactly as AG-UI defines it —
/// never the accumulated buffer. Adapters that only ever learn the complete
/// arguments (Pi hands them over whole) send a single delta; adapters reading
/// a token stream (OpenAI) send many, which is what lets a client render the
/// payload as it is composed rather than after.
pub(crate) fn emit_tool_call_args(rt: &RuntimeState, tool_call_id: &str, delta: &str) {
    if delta.is_empty() {
        return;
    }
    crate::narration::emit_event(
        rt,
        AgUiEvent::<JsonValue>::ToolCallArgs(ToolCallArgsEvent {
            base: BaseEvent::default(),
            tool_call_id: ToolCallId::new(tool_call_id),
            delta: delta.to_string(),
        }),
    );
}

/// Close a tool call's argument stream: the arguments are complete, execution
/// has not necessarily happened yet.
pub(crate) fn emit_tool_call_end(rt: &RuntimeState, tool_call_id: &str) {
    let run_id = rt.current_activity_run.lock().clone();
    rt.activity.record_tool(
        "tool.arguments_finished",
        crate::ActivityOutcome::Info,
        run_id.as_deref(),
        tool_call_id,
        None,
    );
    crate::narration::emit_event(
        rt,
        AgUiEvent::<JsonValue>::ToolCallEnd(ToolCallEndEvent {
            base: BaseEvent::default(),
            tool_call_id: ToolCallId::new(tool_call_id),
        }),
    );
}

/// Report what the action actually returned to the agent.
///
/// The content is the same text [`dispatch_tool`] handed the model, so the
/// human and the agent read one result rather than two descriptions of it.
pub(crate) fn emit_tool_call_result(rt: &RuntimeState, tool_call_id: &str, content: &str) {
    let run_id = rt.current_activity_run.lock().clone();
    rt.activity.record_tool(
        "tool.result",
        crate::ActivityOutcome::Succeeded,
        run_id.as_deref(),
        tool_call_id,
        Some(&format!("{} characters", content.chars().count())),
    );
    crate::narration::emit_event(
        rt,
        AgUiEvent::<JsonValue>::ToolCallResult(ToolCallResultEvent {
            base: BaseEvent::default(),
            message_id: MessageId::random(),
            tool_call_id: ToolCallId::new(tool_call_id),
            content: content.to_string(),
            role: Role::Tool,
        }),
    );
}

/// Read one strict LF-delimited protocol frame without the unbounded
/// allocation behavior of `AsyncBufReadExt::lines`.
pub(super) async fn read_bounded_jsonl_line<R>(
    reader: &mut R,
    limit: usize,
    protocol: &str,
) -> Result<Option<Vec<u8>>, String>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| format!("failed reading {protocol} stdout: {error}"))?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }

        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            let frame_len = line
                .len()
                .checked_add(newline)
                .ok_or_else(|| format!("{protocol} stdout frame length overflowed"))?;
            if frame_len > limit {
                return Err(format!(
                    "{protocol} stdout frame exceeded the {limit}-byte limit"
                ));
            }
            let frame = available
                .get(..newline)
                .ok_or_else(|| format!("{protocol} stdout newline position was invalid"))?;
            line.extend_from_slice(frame);
            reader.consume(newline + 1);
            return Ok(Some(line));
        }

        let chunk_len = available.len();
        let frame_len = line
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| format!("{protocol} stdout frame length overflowed"))?;
        if frame_len > limit {
            return Err(format!(
                "{protocol} stdout frame exceeded the {limit}-byte limit"
            ));
        }
        line.extend_from_slice(available);
        reader.consume(chunk_len);
    }
}

/// The two fixed prompt pieces `App::prompt()` supplies — the agent's
/// persona/method (teaching-canvas's `TUTOR_METHOD`) and how to frame a
/// "mission" turn kickoff (teaching-canvas's `mission_prompt`). Both are
/// surface-flavored prose the runtime treats as opaque; see the M2 handoff's
/// `TUTOR_METHOD`/`build_context`/`build_prime`/`mission_prompt` row
/// (`ag-ui-surface-m2-handoff.md` §2) and step 5's "minus TUTOR_METHOD/
/// mission_prompt (→ App::prompt() input)" instruction. [`crate::App::prompt`]
/// stores this value and the active turn loop consumes it directly.
pub struct Prompts {
    /// The agent's persona + method — prepended to `SurfaceStore::context()`
    /// on every (re)prime (see [`build_context`]).
    pub system: String,
    /// Frame a just-set "mission" as a turn: given the raw text posted to
    /// `RuntimeState::mission_tx`, return the prompt to send as that turn's
    /// user message (teaching-canvas: acknowledge it, then begin lesson
    /// one). A plain `Fn`, not baked into `system`, because it needs the
    /// LIVE text every time — mirrors the original `mission_prompt`'s
    /// signature exactly.
    pub mission: Box<dyn Fn(&str) -> String + Send + Sync>,
}

impl Prompts {
    pub fn new(
        system: impl Into<String>,
        mission: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            system: system.into(),
            mission: Box::new(mission),
        }
    }
}

/// Lets `App::prompt(...)` (trait-review round 3, M2, 2026-07-08, leak 1)
/// accept a bare string for a Surface with no mission-framing needs — the
/// mission text passes through unchanged, same default `App::mission()` used
/// to fall back to before it was folded into this method.
impl From<&str> for Prompts {
    fn from(system: &str) -> Self {
        Prompts::new(system, |text: &str| text.to_string())
    }
}

impl From<String> for Prompts {
    fn from(system: String) -> Self {
        Prompts::new(system, |text: &str| text.to_string())
    }
}

/// Optional compatibility config for an ACP adapter that does not advertise
/// Streamable HTTP MCP. Current built-in adapters use the runtime-owned `/mcp`
/// endpoint, whose schemas come directly from agent-visible
/// [`crate::ActionDef`] metadata; applications normally never construct this.
pub struct McpBridge {
    /// The fallback MCP server name advertised to the ACP agent.
    pub name: String,
    /// The bridge binary to spawn. Custom stdio bridges should call
    /// [`guard_mcp_bridge_parent`] before reading stdin so an adapter crash
    /// cannot leave them orphaned.
    pub program: String,
    pub args: Vec<String>,
    /// Environment variables required by the custom fallback bridge.
    pub env: Vec<(String, String)>,
}

/// Start a lightweight guard inside a stdio MCP bridge so it cannot outlive
/// the ACP process that owns its transport.
///
/// Some ACP adapters launch MCP servers in a separate process group. That is
/// useful isolation, but it means killing the adapter's group cannot reach the
/// bridge. On Unix this guard records the bridge's original parent and exits
/// immediately if the OS reparents it. Call it at the very start of an MCP
/// bridge binary, before blocking on stdin. Windows is currently a no-op; its
/// equivalent belongs in the custom bridge implementation.
pub fn guard_mcp_bridge_parent() {
    #[cfg(unix)]
    {
        // SAFETY: `getppid` has no preconditions and only reads process state.
        let expected_parent = unsafe { libc::getppid() };
        if expected_parent <= 1 {
            // SAFETY: immediate process termination is intentional before any
            // bridge work starts; no Rust destructors need to run here.
            unsafe { libc::_exit(0) };
        }

        if let Err(error) = std::thread::Builder::new()
            .name("ag-ui-mcp-parent-guard".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                // SAFETY: `getppid` has no preconditions and only reads process state.
                if unsafe { libc::getppid() } != expected_parent {
                    // The stdio owner is gone. `_exit` avoids waiting on locks
                    // the main thread may hold while blocked in stdin or HTTP.
                    // SAFETY: the owner is gone; `_exit` avoids deadlocking on
                    // locks held by the bridge's blocked main thread.
                    unsafe { libc::_exit(0) };
                }
            })
        {
            eprintln!("[ag-ui-mcp] failed to start parent guard: {error}");
        }
    }
}

/// Outcome of one provider session, telling the supervisor what to do next.
pub enum Outcome {
    /// The user asked to switch to this provider id.
    Switch(String),
    /// The agent process/backend ended or failed. The supervisor reports the
    /// failure and waits for an explicit switch/retry.
    Exited,
}

/// The agent's full standing context: the fixed `prompts.system` text
/// followed by whatever standing context the Surface's store contributes
/// ([`crate::SurfaceStore::context`]) — teaching-canvas's mission + recent
/// learning records, Vellum's recent ADRs, or nothing at all if `store()` is
/// `None` or its `context()` is empty. Read fresh on every (re)prime, so
/// provider switches and restarts pick up state changes. Shared by both
/// backends — the ACP path wraps it in a READY handshake (`turn_loop::acp`'s
/// `build_prime`); the OpenAI backend uses it directly as the system message.
pub fn build_context(prompts: &Prompts, surface: &dyn Surface) -> String {
    let mut p = prompts.system.clone();
    if let Some(store) = surface.store() {
        let ctx = store.context();
        if !ctx.is_empty() {
            p.push_str("\n\n");
            p.push_str(&ctx);
        }
    }
    p
}

/// Apply one tool call: look it up in `Surface::tools()`, run its `apply`
/// closure to get an [`Effect`], then dispatch that Effect against
/// `Surface::state()`/`Surface::store()`/the runtime's decision map. Returns a
/// `(text, was_query, ok)` triple:
///
/// - **`text`** — the tool-result text to feed back to the model (the generic
///   ack `format!("applied {name}")` when the Effect had nothing more specific
///   to say — mirroring `openai.rs`'s original `.unwrap_or_else(...)` fallback,
///   `ag-ui-surface-m2-handoff.md` §2's `apply_tool` row).
/// - **`was_query`** — the effect was a pure [`Effect::Query`]. A deliberate
///   genericization, not a mechanical port: the original `openai.rs` triggered
///   its vision re-injection on `tc.name == "read_canvas"` — a canvas-specific
///   string that can't survive the lift (Vellum has no `read_canvas` tool).
///   `Effect::Query` already means exactly "the agent asked to see the current
///   state, without mutating it" — generically, for ANY Surface —
///   `turn_loop::openai::run_turn` uses this instead of matching a tool name.
/// - **`ok`** — `false` when the action is unknown, its arguments fail the
///   startup-compiled JSON Schema, or it rejects its own otherwise schema-valid
///   call ([`Effect::Reject`]); HTTP callers surface that as a retryable error
///   (Vellum's `render_choice_set` validation).
///
/// **`async`, and `RuntimeState`-aware**, because of [`Effect::EmitAndAwait`]:
/// that arm stashes a keyed [`PendingDecision`], emits the event, and suspends
/// — racing the human's `POST /decision` reply against barge-in
/// (`rt.interrupt_tx`) — so the whole function is a suspension point now, not a
/// synchronous match.
///
/// Used by the OpenAI-compatible backend, the runtime-owned `/mcp` endpoint,
/// and the generic `POST /surface/action` route (legacy alias:
/// `/canvas-tool`). Every actor therefore executes the same closure.
///
/// **`caller` is enforced here, not at the transport.** Each transport used to
/// decide for itself, and the three that reach an agent all checked
/// `audience.agent()` while `POST /surface/action` checked nothing — so an
/// `agent_only` action was reachable from the browser even when the extension
/// manifest advertised no human actions at all. The manifest only decides what
/// is *listed*. This is what decides what may *run*, so a transport cannot
/// widen the surface by omission.
pub async fn dispatch_tool(
    rt: &RuntimeState,
    surface: &dyn Surface,
    caller: Caller,
    name: &str,
    args: &JsonValue,
) -> (String, bool, bool) {
    let run_id = rt.current_activity_run.lock().clone();
    let before = match surface.state().activity_state_revision() {
        Ok(revision) => revision,
        Err(error) => {
            rt.activity.record_failure(
                "state_revision.before_unavailable",
                Some(caller),
                run_id.as_deref(),
                &error,
            );
            Vec::new()
        }
    };
    let result = dispatch_tool_inner(rt, surface, caller, name, args).await;
    let after = match surface.state().activity_state_revision() {
        Ok(revision) => revision,
        Err(error) => {
            rt.activity.record_failure(
                "state_revision.after_unavailable",
                Some(caller),
                run_id.as_deref(),
                &error,
            );
            Vec::new()
        }
    };
    let outcome = if result.2 {
        crate::ActivityOutcome::Succeeded
    } else {
        crate::ActivityOutcome::Failed
    };
    rt.activity.record_action(
        caller,
        name,
        outcome,
        run_id.as_deref(),
        before,
        after,
        (!result.2).then_some(result.0.as_str()),
    );
    if !result.2 {
        rt.activity.record_failure(
            "action.failed",
            Some(caller),
            run_id.as_deref(),
            &format!("{name}: {}", result.0),
        );
    }
    result
}

async fn dispatch_tool_inner(
    rt: &RuntimeState,
    surface: &dyn Surface,
    caller: Caller,
    name: &str,
    args: &JsonValue,
) -> (String, bool, bool) {
    let Some(tool) = surface.tools().iter().find(|t| t.name == name) else {
        return (format!("unknown action '{name}'"), false, false);
    };
    // Before argument validation: an action this caller may not run should not
    // have its arguments inspected, and the refusal should not depend on
    // whether the payload happened to be well formed.
    if !caller.may_call(tool.audience) {
        return (
            format!("action '{name}' is not available to {}", caller.label()),
            false,
            false,
        );
    }
    if let Err(error) = rt.validate_action_arguments(name, args) {
        return (error, false, false);
    }
    match (tool.apply)(args) {
        Effect::Query(f) => match f(surface.state()) {
            Ok(text) => (text, true, true),
            Err(error) => (error, true, false),
        },
        Effect::AsyncQuery(future) => match future.await {
            Ok(text) => (text, true, true),
            Err(error) => (error, true, false),
        },
        Effect::Mutate(f) => match f(surface.state()) {
            Ok(text) => (
                text.unwrap_or_else(|| format!("applied {name}")),
                false,
                true,
            ),
            Err(error) => (error, false, false),
        },
        Effect::AsyncMutate(future) => match future.await {
            Ok(text) => (
                text.unwrap_or_else(|| format!("applied {name}")),
                false,
                true,
            ),
            Err(error) => (error, false, false),
        },
        Effect::Commit { apply, .. } => match surface.store() {
            Some(store) => match apply(store) {
                Ok(text) => (
                    text.unwrap_or_else(|| format!("applied {name}")),
                    false,
                    true,
                ),
                Err(error) => (error, false, false),
            },
            None => (
                format!("action '{name}' requires a durable store, but none is configured"),
                false,
                false,
            ),
        },
        Effect::Reject(msg) => (msg, false, false),
        Effect::EmitAndAwait {
            id,
            event,
            reply_kind,
            on_reply,
        } => {
            let event_json = match serde_json::to_string(&AgUiEvent::<JsonValue>::Custom(event)) {
                Ok(json) => json,
                Err(error) => {
                    return (
                        format!("failed to serialize decision {id:?}: {error}"),
                        false,
                        false,
                    );
                }
            };
            // Subscribe before exposing the decision or emitting its event so
            // an interrupt racing either step is still observed by this
            // waiter. A subscription created after the emit can miss a
            // barge-in that lands in that narrow window.
            let mut interrupt = rt.interrupt_tx.subscribe();

            // Stash + emit under the SSE replay boundary. A reconnect sees the
            // decision exactly once, either in its pending snapshot or in its
            // already-subscribed live queue.
            let (reply_tx, mut reply_rx) = oneshot::channel();
            let decision_token = MessageId::random().to_string();
            if rt
                .register_pending_decision(
                    id.clone(),
                    PendingDecision {
                        token: decision_token.clone(),
                        reply_kind,
                        on_reply: Some(on_reply),
                        reply_tx: Some(reply_tx),
                        event_json,
                    },
                )
                .is_err()
            {
                return (format!("decision '{id}' is already pending"), false, false);
            }
            let _pending_guard = PendingDecisionGuard {
                rt,
                id: id.clone(),
                token: decision_token.clone(),
            };

            enum DecisionOutcome {
                Interrupted,
                Reply(Result<crate::EffectResult<String>, oneshot::error::RecvError>),
            }

            // Suspend: race the human reply against barge-in. The decision map
            // is the ownership arbiter: an interrupt only wins if it removes
            // this exact waiter generation. If `/decision` already claimed the
            // token, finish receiving that reply even when the interrupt future
            // was selected first.
            let outcome = tokio::select! {
                biased;
                _ = interrupt.recv() => {
                    if rt
                        .close_pending_decision_if_token(
                            &id,
                            &decision_token,
                            "interrupted",
                        )
                        .is_some()
                    {
                        DecisionOutcome::Interrupted
                    } else {
                        DecisionOutcome::Reply(reply_rx.await)
                    }
                }
                reply = &mut reply_rx => DecisionOutcome::Reply(reply),
            };
            let result = match outcome {
                DecisionOutcome::Interrupted => Ok(format!(
                    "decision '{id}' interrupted (barge-in): the human moved on without answering"
                )),
                DecisionOutcome::Reply(reply) => match reply {
                    Ok(result) => result,
                    // A provider cleanup may drop the sender. Token-scoped
                    // cleanup must never delete a newer waiter reusing `id`.
                    Err(_) => {
                        rt.close_pending_decision_if_token(&id, &decision_token, "cancelled");
                        Ok(format!("decision '{id}' cancelled"))
                    }
                },
            };
            match result {
                Ok(text) => (text, false, true),
                Err(error) => (error, false, false),
            }
        }
    }
}

/// Ownership guard for a suspended decision wait. If its HTTP/provider task is
/// aborted or the server drops the future, the pending map and browser panel
/// are closed synchronously rather than replaying forever.
struct PendingDecisionGuard<'a> {
    rt: &'a RuntimeState,
    id: String,
    token: String,
}

impl Drop for PendingDecisionGuard<'_> {
    fn drop(&mut self) {
        self.rt
            .close_pending_decision_if_token(&self.id, &self.token, "wait_dropped");
    }
}

/// Supervise the agent: run the current provider, and on a switch request
/// tear it down and start the requested one. Loops for the life of the
/// server.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_supervisor(
    rt: Arc<RuntimeState>,
    surface: Arc<dyn Surface>,
    prompts: Arc<Prompts>,
    // Optional stdio compatibility fallback. HTTP-capable ACP adapters ignore
    // it and connect to the runtime-owned `/mcp` endpoint.
    bridge: Option<McpBridge>,
    agent_cwd: std::path::PathBuf,
    mut ask_rx: mpsc::UnboundedReceiver<TurnRequest>,
    mut mission_rx: mpsc::UnboundedReceiver<TurnRequest>,
    mut switch_rx: mpsc::UnboundedReceiver<String>,
) {
    loop {
        // An MCP bearer token lives for the runtime process, but a negotiated
        // MCP session must not outlive its provider. Clear on every provider
        // loop entry, including ACP -> OpenAI switches; an ACP client sets a
        // fresh version only after its own initialize request succeeds.
        *rt.mcp_protocol_version.lock() = None;
        let provider_id = rt.provider.lock().clone();
        match run_provider(
            &rt,
            &surface,
            &prompts,
            bridge.as_ref(),
            &agent_cwd,
            &provider_id,
            &mut ask_rx,
            &mut mission_rx,
            &mut switch_rx,
        )
        .await
        {
            Outcome::Switch(new_id) => {
                rt.close_all_pending_decisions("provider_switched");
                adopt_provider(&rt, new_id);
                *rt.provider_error.lock() = None;
                // loop: respawn with the new provider
            }
            Outcome::Exited => {
                rt.close_all_pending_decisions("provider_exited");
                rt.ready.store(false, Ordering::Relaxed);
                rt.warming.store(false, Ordering::Relaxed);
                rt.busy.store(false, Ordering::Relaxed);
                let error = format!(
                    "provider {provider_id:?} stopped; select it again to retry or choose another provider"
                );
                *rt.provider_error.lock() = Some(error.clone());
                rt.activity.record_failure(
                    "provider.exited",
                    None,
                    rt.current_activity_run.lock().as_deref(),
                    &error,
                );
                crate::narration::tutor_event(&rt, "failed", Some(&error));
                warn!("{error}");

                // A broken executable, adapter, or credential must not create
                // a permanent process-spawn loop. Stay down until the user
                // explicitly retries this provider or selects a different one.
                let Some(new_id) = switch_rx.recv().await else {
                    return;
                };
                adopt_provider(&rt, new_id);
                *rt.provider_error.lock() = None;
                rt.warming.store(true, Ordering::Relaxed);
                crate::narration::tutor_event(&rt, "warming", None);
            }
        }
    }
}

/// Pair the selected provider id with its resolved endpoint configuration
/// before advancing the HTTP-visible adoption generation.
fn adopt_provider(rt: &RuntimeState, new_id: String) {
    let provider = providers::find(&rt.providers.read(), &new_id).cloned();
    if let Some(provider) = provider {
        if matches!(provider.backend, providers::Backend::OpenAi) {
            let byok = rt.byok.lock().clone();
            let override_model = rt.model_overrides.lock().get(&new_id).cloned();
            let config = openai::resolve_openai_config(
                &rt.auth,
                &byok,
                &provider,
                override_model.as_deref(),
            );
            *rt.openai.lock() = config;
        }
    }
    *rt.provider.lock() = new_id.clone();
    rt.provider_generation.fetch_add(1, Ordering::SeqCst);
    rt.activity.record_provider(
        "provider.adopted",
        crate::ActivityOutcome::Succeeded,
        rt.current_activity_run.lock().as_deref(),
        &new_id,
    );
}

/// Resolve `provider_id` in the registry and dispatch to the right backend:
/// Process-backed providers spawn their protocol adapter; `Backend::OpenAi`
/// hands off to the in-process driver ([`openai::run_one_openai`]).
#[allow(clippy::too_many_arguments)]
async fn run_provider(
    rt: &Arc<RuntimeState>,
    surface: &Arc<dyn Surface>,
    prompts: &Arc<Prompts>,
    bridge: Option<&McpBridge>,
    agent_cwd: &std::path::Path,
    provider_id: &str,
    ask_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    mission_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    switch_rx: &mut mpsc::UnboundedReceiver<String>,
) -> Outcome {
    let provider = providers::find(&rt.providers.read(), provider_id).cloned();
    let (backend, provider_auth) = match provider {
        Some(p) => (p.backend, p.auth_note),
        None => {
            warn!("selected provider '{provider_id}' is not registered");
            return Outcome::Exited;
        }
    };

    rt.activity.record_provider(
        "provider.starting",
        crate::ActivityOutcome::Started,
        rt.current_activity_run.lock().as_deref(),
        provider_id,
    );
    let outcome = match backend {
        providers::Backend::Acp { program, args } => {
            acp::run_one(
                rt,
                surface,
                prompts,
                bridge,
                agent_cwd,
                provider_id,
                &provider_auth,
                program,
                args,
                ask_rx,
                mission_rx,
                switch_rx,
            )
            .await
        }
        providers::Backend::PiRpc {
            program,
            args,
            builtin_tools,
        } => {
            pi::run_one(
                rt,
                surface,
                prompts,
                agent_cwd,
                provider_id,
                &provider_auth,
                program,
                args,
                builtin_tools,
                ask_rx,
                mission_rx,
                switch_rx,
            )
            .await
        }
        providers::Backend::OpenAi => {
            openai::run_one_openai(rt, surface, prompts, ask_rx, mission_rx, switch_rx).await
        }
    };
    let activity_outcome = match &outcome {
        Outcome::Switch(_) => crate::ActivityOutcome::Cancelled,
        Outcome::Exited => crate::ActivityOutcome::Failed,
    };
    rt.activity.record_provider(
        "provider.session_finished",
        activity_outcome,
        rt.current_activity_run.lock().as_deref(),
        provider_id,
    );
    outcome
}

#[cfg(all(test, unix))]
mod parent_guard_tests {
    use super::guard_mcp_bridge_parent;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const CHILD_ENV: &str = "AG_UI_MCP_PARENT_GUARD_TEST_CHILD";
    const READY_PREFIX: &str = "AG_UI_MCP_PARENT_GUARD_READY";

    #[test]
    fn parent_guard_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }

        guard_mcp_bridge_parent();
        println!("{READY_PREFIX} {} {}", std::process::id(), unsafe {
            libc::getppid()
        });
        std::io::stdout().flush().expect("flush child readiness");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn parent_guard_exits_bridge_after_owner_is_killed() {
        let test_binary = std::env::current_exe().expect("current test binary");
        let child_test = "turn_loop::parent_guard_tests::parent_guard_child";
        let mut wrapper = Command::new("/bin/sh")
            .args([
                "-c",
                "\"$1\" --exact \"$2\" --nocapture & wait",
                "bridge-parent",
                test_binary.to_str().expect("UTF-8 test binary path"),
                child_test,
            ])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bridge owner");
        let wrapper_pid = i32::try_from(wrapper.id()).expect("wrapper pid fits i32");
        let stdout = wrapper.stdout.take().expect("wrapper stdout");
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(rest) = line.strip_prefix(READY_PREFIX) {
                    let mut fields = rest.split_whitespace();
                    let pid = fields.next().and_then(|v| v.parse::<i32>().ok());
                    let ppid = fields.next().and_then(|v| v.parse::<i32>().ok());
                    let _ = ready_tx.send((pid, ppid));
                    return;
                }
            }
        });

        let (child_pid, child_parent) = ready_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("bridge child reported readiness");
        let child_pid = child_pid.expect("numeric bridge child pid");
        assert_eq!(
            child_parent,
            Some(wrapper_pid),
            "test wrapper must directly own the bridge"
        );

        wrapper.kill().expect("kill bridge owner");
        let _ = wrapper.wait();

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let exists = unsafe { libc::kill(child_pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            if !exists {
                break;
            }
            if Instant::now() >= deadline {
                unsafe { libc::kill(child_pid, libc::SIGKILL) };
                panic!("MCP bridge survived after its parent was killed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
