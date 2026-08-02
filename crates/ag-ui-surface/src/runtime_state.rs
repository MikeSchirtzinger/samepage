//! `RuntimeState` — the generic-runtime half of teaching-canvas's old
//! `AppState` (M2 Phase C; see `docs/ag-ui-surface-m2-handoff.md` §2's
//! `AppState` row and §4 step 5). Everything here is turn-loop/transport/
//! provider-config chrome common to ANY Surface — a Surface's own live state
//! is NOT held here at all; it lives behind `Surface::state()`
//! (teaching-canvas's `CanvasState`, which this module has no knowledge of).
//!
//! Field-for-field, this is the "generic-runtime" column of the `AppState`
//! coupling-inventory row, unchanged in shape from the original struct —
//! only the surface-specific fields (`scene`, `blobs`, `groups`, `render`,
//! `workspace`, `current_focus`, `chat_config`, `widget`, …) were left
//! behind, in `CanvasState`.

use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tokio::sync::{broadcast, mpsc, oneshot};

use ag_ui_core::event::{BaseEvent, CustomEvent, Event as AgUiEvent};
use ag_ui_core::types::MessageId;
use ag_ui_core::JsonValue;

use crate::action_schema::ActionSchemas;
use crate::activity::ActivityFeed;
use crate::auth::AuthStore;
use crate::narration::Narration;
use crate::providers::Provider;
use crate::turn_loop::openai::{ByokConfig, OpenAiConfig};
use crate::{DecisionReply, EffectResult, ReplyKind};

/// An agent that attached over `/mcp` and said who it is.
///
/// `label` is what the room shows. It starts from the client's proposed name
/// and is disambiguated on collision, because four agents all called "claude"
/// is exactly the failure this exists to prevent. It is display only —
/// authority still comes from [`crate::Caller`], and `participant_id` is
/// minted here rather than accepted from the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedAgent {
    pub session: String,
    pub participant_id: String,
    pub label: String,
    pub client_name: String,
    pub client_version: Option<String>,
    /// The [`Principal`](crate::identity::Principal) key of the person who
    /// brought this agent, when the surface knows it.
    ///
    /// Optional here and required at the meetings tier, which is the whole
    /// distinction between the two: an open room lets an agent wander in, and a
    /// meeting insists every agent in it is somebody's. Without this an agent's
    /// writes are attributable to a model but to no one accountable, which is a
    /// record of what happened with the responsible party cropped out.
    pub responsible: Option<String>,
}

/// One suspended [`Effect::EmitAndAwait`](crate::Effect::EmitAndAwait),
/// awaiting the human's reply. Stashed in [`RuntimeState::decision`] keyed by
/// the decision id; `POST /decision` removes it and sends the posted reply on
/// [`reply_tx`](Self::reply_tx), which the suspended `dispatch_tool` is racing
/// against barge-in.
pub struct PendingDecision {
    /// Unique waiter identity. Guards compare this before cleanup so an older
    /// dropped future cannot close a newer decision that reused the same id.
    pub(crate) token: String,
    /// The typed reply shape enforced by `POST /decision` before the pending
    /// wait is consumed. The Surface-owned `on_reply` closure then interprets
    /// the already-valid bespoke fields.
    pub reply_kind: ReplyKind,
    /// Exact serialized custom event originally emitted for this decision.
    /// Replayed to reconnecting SSE clients until the reply resolves.
    pub event_json: String,
    /// Surface-owned durable reply handler. The HTTP route executes this after
    /// it atomically claims the decision, so an accepted reply cannot be lost
    /// if its provider task is cancelled immediately afterward.
    pub on_reply: Option<DecisionReply>,
    /// Delivers the already-applied action result back to the suspended tool
    /// call for inclusion in the provider conversation.
    pub reply_tx: Option<oneshot::Sender<EffectResult<String>>>,
}

/// One assistant message currently streaming over the standard AG-UI text
/// lifecycle. The browser appends every `TEXT_MESSAGE_CONTENT` delta carrying
/// this id into one bubble; `text` is retained until the matching end event so
/// reconnecting clients can reconstruct an in-flight message exactly.
#[derive(Clone)]
pub struct ActiveMessage {
    pub id: MessageId,
    pub text: String,
}

/// How long a reported focus (deixis pointing, see [`RuntimeState::current_focus`])
/// stays relevant to the next `/ask`. Long enough to point-then-type a
/// sentence; short enough that a point you've moved on from doesn't haunt an
/// unrelated later question. Lifted verbatim (value + rationale) from
/// teaching-canvas's `CanvasState::FOCUS_TTL` (M2 Phase D — the slot itself
/// moved here since it's now filled generically via `SurfaceState::resolve`,
/// see [`RuntimeState::current_focus`]'s docs).
pub const FOCUS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// One accepted human turn plus the cancellation receiver created while the
/// HTTP acceptance lock is still held. The receiver belongs to exactly this
/// queued turn, so an immediate interrupt cannot be missed and a late signal
/// cannot leak into the next turn.
pub(crate) struct TurnRequest {
    pub text: String,
    pub cancel: broadcast::Receiver<()>,
}

/// The generic-runtime half of the old `AppState`: transport channels, the
/// turn-protocol control channels (ask/mission/switch/interrupt), narration
/// chrome, and the resolved provider/auth/config a turn loop drives against.
/// Fields are `pub` because — exactly as the original `AppState`'s
/// `pub(crate)` fields were reached from every HTTP handler in
/// teaching-canvas's `main.rs` — Phase D's `main.rs` (now outside this
/// crate) needs the same direct access.
pub struct RuntimeState {
    /// Every effective [`ActionDef`](crate::ActionDef) input schema, validated
    /// and compiled before the server binds. The shared dispatcher reads this
    /// one catalog for human, OpenAI, and MCP calls.
    pub(crate) action_schemas: OnceLock<ActionSchemas>,
    /// Persistent host-owned operational journal. Browser subscribers consume
    /// this state; they never report activity back as authoritative evidence.
    pub activity: Arc<ActivityFeed>,
    /// Host-generated correlation for the accepted turn currently executing.
    pub(crate) current_activity_run: Mutex<Option<String>>,
    /// Binary state-sync frames broadcast to every connected client. A
    /// Surface's own tool-application code typically holds a second handle
    /// onto this SAME channel (see teaching-canvas's `CanvasState::ws_tx`) —
    /// this is the one send side the transport layer (WS route) owns.
    pub ws_tx: broadcast::Sender<Vec<u8>>,
    /// AG-UI SSE custom events: narration, tutor lifecycle (`surface.*`) —
    /// see [`crate::narration`]. A Surface's own chrome events (teaching-
    /// canvas's `canvas.chat`/`canvas.widget`/`canvas.clear`)
    /// go out over a second handle onto this same channel, same pattern as
    /// `ws_tx` above.
    pub sse_tx: broadcast::Sender<String>,
    /// Assistant message currently streaming, if any. Finished messages move
    /// into [`Self::history`]; an in-flight one is replayed as start + content
    /// after history when a new SSE client connects.
    pub active_message: Mutex<Option<ActiveMessage>>,
    /// Provider session currently authorized to publish streamed narration. This is
    /// deliberately separate from [`Self::busy`]: HTTP may queue a new turn
    /// while the provider is still draining cancellation of the old prompt.
    /// During that drain this stays `None`, so late old chunks cannot leak into
    /// the queued turn's transcript or speech.
    pub(crate) stream_active_session: Mutex<Option<String>>,
    /// Establishes an atomic boundary between transcript mutation+broadcast
    /// and a reconnecting SSE client's history/active-message snapshot.
    pub transcript_replay_lock: Arc<Mutex<()>>,
    /// Recent transcript lines (agent + human), replayed on reconnect so the
    /// conversation survives a reload. Capped ring buffer (see
    /// [`crate::narration::push_history`]).
    pub history: Arc<Mutex<VecDeque<JsonValue>>>,
    /// Server-side speak (voice) on/off — `App::voice()`'s runtime toggle.
    pub audio_on: AtomicBool,
    /// Commands to the single narration-audio worker (serializes playback).
    pub narr_tx: mpsc::UnboundedSender<Narration>,
    /// Send a question to the live turn loop.
    pub(crate) ask_tx: mpsc::UnboundedSender<TurnRequest>,
    /// Serializes HTTP acceptance/persistence of human-authored turns. The
    /// visible `busy` transition and transcript projection happen later under
    /// `transcript_replay_lock`, so this lock reserves a submission without a
    /// reconnect observing a phantom thinking state.
    pub turn_accept_lock: tokio::sync::Mutex<()>,
    /// Serializes provider switches and credential/settings transactions so
    /// runtime configuration cannot drift behind the last durable write.
    pub provider_control_lock: tokio::sync::Mutex<()>,
    /// Send a directed "mission" turn — a kickoff prompt outside the normal
    /// ask flow (teaching-canvas: the learner just set their goal, framed by
    /// `Prompts::mission` into the turn's user message). Named for parity
    /// with the pre-lift `AppState::mission_tx`; a differently-shaped Surface
    /// may not use it at all (the channel is just plumbing — nothing reads
    /// its contents but `Prompts::mission`).
    pub(crate) mission_tx: mpsc::UnboundedSender<TurnRequest>,
    /// Request a provider switch (the supervisor tears down + respawns).
    pub switch_tx: mpsc::UnboundedSender<String>,
    /// Barge-in: a tick here cancels whatever turn is currently in flight —
    /// see `turn_loop::acp`/`turn_loop::openai`'s `run_turn`, both of which
    /// subscribe for the life of a turn and race it against this broadcast.
    pub interrupt_tx: broadcast::Sender<()>,
    /// The currently selected provider id.
    pub provider: Mutex<String>,
    /// Monotonic acknowledgement from the supervisor. HTTP switch/re-prime
    /// requests wait for this to advance instead of claiming success merely
    /// because a command was queued.
    pub provider_generation: AtomicU64,
    /// The full provider registry (managed agents, `models.json` presets, and
    /// user-created OpenAI-compatible connections). Connections can be added
    /// or removed at runtime, so readers take a short snapshot/lookup lock.
    pub providers: RwLock<Vec<Provider>>,
    /// Pi-style per-provider credential store (`auth.json`).
    pub auth: Arc<AuthStore>,
    /// The resolved, active OpenAI-compatible endpoint the OpenAI backend
    /// drives. Recomputed from the selected provider + [`auth`](Self::auth)
    /// on every switch or credential change. Never returned to the browser.
    pub openai: Mutex<OpenAiConfig>,
    /// Bring-your-own base URL + model for the manual `openai` slot (its key
    /// lives in [`auth`](Self::auth)).
    pub byok: Mutex<ByokConfig>,
    /// UI model overrides, keyed by provider id. Lets a `models.json` preset
    /// (OpenRouter, Groq, …) have its model swapped from the ⚙ Settings form
    /// without editing the file — the row's shipped `model` is the default, an
    /// entry here wins. The `openai` BYOK slot uses [`byok`](Self::byok)
    /// instead (it owns base_url too); this map is model-only.
    pub model_overrides: Mutex<std::collections::HashMap<String, String>>,
    /// The agent finished priming and accepts questions.
    pub ready: AtomicBool,
    /// True while the session is priming (suppresses narration).
    pub warming: AtomicBool,
    /// Last terminal provider failure. A failed backend stays stopped until an
    /// explicit switch/retry instead of entering an unbounded respawn loop.
    pub provider_error: Mutex<Option<String>>,
    /// A question is being answered (rejects overlap at the transport layer).
    pub busy: AtomicBool,
    /// The agent posed a recall question and is waiting on a reply. Tracked
    /// here (not just client-side) so a reloaded / late-joining page
    /// restores the "your turn" state.
    pub awaiting: Arc<AtomicBool>,
    /// Accumulates streamed agent text into whole spoken sentences (see
    /// [`crate::narration::narrate_chunk`]).
    pub narration_buf: Mutex<String>,
    /// The port this runtime is bound to (handed to ACP subprocesses that
    /// connect to the runtime-owned Streamable HTTP MCP endpoint).
    pub port: u16,
    /// Bearer token for the loopback `/mcp` endpoint. It is passed only in the
    /// ACP session's HTTP MCP descriptor and is never returned by
    /// browser-facing provider/config routes.
    ///
    /// Random per process by default, which is right when the only client is
    /// the ACP subprocess the runtime launches itself — it is handed the token
    /// directly and nobody else needs it. Set `AGUI_MCP_TOKEN` to pin it
    /// instead, which is what an *outside* MCP client needs: the surface's
    /// actions are already a complete MCP server, and the only reason another
    /// assistant could not connect to it was that the token existed nowhere it
    /// could be read. Pinning it is the whole bridge.
    ///
    /// Still loopback- and origin-checked either way; this makes the token
    /// knowable, not optional.
    pub mcp_token: String,
    /// Every agent currently attached over `/mcp`, keyed by the session id this
    /// host minted for it at `initialize`.
    ///
    /// This is a map rather than a single slot on purpose. The runtime used to
    /// assume one model at a time, so every MCP caller shared one anonymous
    /// byline and the only agent identity in the codebase was a hardcoded
    /// literal. A shared surface has to be able to say *which* agent wrote
    /// something, which starts with being able to hold more than one.
    pub mcp_agents: Mutex<HashMap<String, AttachedAgent>>,
    /// Everyone here who is a person rather than a model, keyed by the resume
    /// token their browser holds.
    ///
    /// Agents got identity first because they attach with a handshake that had
    /// somewhere to put it. People never handshake — they just load a page — so
    /// a surface could say which of four agents wrote something while still
    /// calling every human "you". That asymmetry is what made a room usable by
    /// any number of agents and exactly one person.
    pub people: crate::identity::People,
    /// MCP version selected by the most recent authenticated `initialize`
    /// request. ACP runs one provider session at a time; switching providers
    /// clears this slot before the replacement client receives `/mcp`.
    pub mcp_protocol_version: Mutex<Option<String>>,
    /// Deixis grounding: what the learner is currently pointing at, resolved
    /// to a human-readable phrase via `SurfaceState::resolve(id)` when the
    /// browser reports a click/dwell over `POST /semantic`. Generic across
    /// any Surface that implements `resolve()` — teaching-canvas's old
    /// `CanvasState::current_focus` held the same thing plus canvas-specific
    /// fields (kind/coords/color) that only `resolve()`'s wording needed;
    /// this slot only needs the already-worded phrase plus a timestamp for
    /// the [`FOCUS_TTL`] freshness check `/ask` performs before splicing it
    /// into the model's turn as a preamble.
    pub current_focus: Mutex<Option<(String, Instant)>>,
    /// Suspended [`Effect::EmitAndAwait`](crate::Effect::EmitAndAwait)
    /// decisions, keyed by decision id, awaiting a `POST /decision` reply.
    /// **Keyed (a `HashMap`, not a single `Option`)** so concurrent decisions
    /// — Colab's batch mode races 2–3 at once — each resolve to the right
    /// suspended tool call instead of clobbering one shared slot (Colab-spec
    /// §8.1, the one M5-blocking correction over Leak 4's single-slot design).
    /// Vellum only ever holds one key; the map costs it nothing.
    pub decision: Mutex<HashMap<String, PendingDecision>>,
}

/// The receiver halves `RuntimeState::new` keeps internal senders for —
/// handed back so the caller can spawn the turn-loop supervisor and the
/// narration worker against them.
pub struct RuntimeChannels {
    pub(crate) ask_rx: mpsc::UnboundedReceiver<TurnRequest>,
    pub(crate) mission_rx: mpsc::UnboundedReceiver<TurnRequest>,
    pub switch_rx: mpsc::UnboundedReceiver<String>,
    pub narr_rx: mpsc::UnboundedReceiver<Narration>,
}

impl RuntimeState {
    /// Build a fresh `RuntimeState` plus the channel receivers its caller
    /// needs to spawn `turn_loop::run_supervisor` and
    /// `narration::narration_worker`. `ws_tx`/`sse_tx`/`history` are taken as
    /// parameters (not constructed here) because a Surface typically needs a
    /// second handle onto the SAME channels/buffer for its own chrome events
    /// (see this struct's field docs) — the caller owns their construction
    /// so it can hand out clones both ways.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ws_tx: broadcast::Sender<Vec<u8>>,
        sse_tx: broadcast::Sender<String>,
        history: Arc<Mutex<VecDeque<JsonValue>>>,
        providers: Vec<Provider>,
        auth: Arc<AuthStore>,
        provider_id: String,
        byok: ByokConfig,
        audio_on: bool,
        port: u16,
    ) -> (Arc<Self>, RuntimeChannels) {
        Self::new_with_shared_tutor(
            ws_tx,
            sse_tx,
            history,
            providers,
            auth,
            provider_id,
            byok,
            audio_on,
            port,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(())),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_shared_tutor(
        ws_tx: broadcast::Sender<Vec<u8>>,
        sse_tx: broadcast::Sender<String>,
        history: Arc<Mutex<VecDeque<JsonValue>>>,
        providers: Vec<Provider>,
        auth: Arc<AuthStore>,
        provider_id: String,
        byok: ByokConfig,
        audio_on: bool,
        port: u16,
        awaiting: Arc<AtomicBool>,
        transcript_replay_lock: Arc<Mutex<()>>,
    ) -> (Arc<Self>, RuntimeChannels) {
        Self::new_with_shared_tutor_and_activity(
            ws_tx,
            sse_tx,
            history,
            providers,
            auth,
            provider_id,
            byok,
            audio_on,
            port,
            awaiting,
            transcript_replay_lock,
            ActivityFeed::memory(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_shared_tutor_and_activity(
        ws_tx: broadcast::Sender<Vec<u8>>,
        sse_tx: broadcast::Sender<String>,
        history: Arc<Mutex<VecDeque<JsonValue>>>,
        providers: Vec<Provider>,
        auth: Arc<AuthStore>,
        provider_id: String,
        byok: ByokConfig,
        audio_on: bool,
        port: u16,
        awaiting: Arc<AtomicBool>,
        transcript_replay_lock: Arc<Mutex<()>>,
        activity: Arc<ActivityFeed>,
    ) -> (Arc<Self>, RuntimeChannels) {
        let (ask_tx, ask_rx) = mpsc::unbounded_channel();
        let (mission_tx, mission_rx) = mpsc::unbounded_channel();
        let (switch_tx, switch_rx) = mpsc::unbounded_channel();
        let (interrupt_tx, _) = broadcast::channel(8);
        let (narr_tx, narr_rx) = mpsc::unbounded_channel();
        let model_overrides = providers
            .iter()
            .filter(|provider| provider.id != "openai")
            .filter_map(|provider| {
                auth.api_key_env(&provider.id)
                    .get(crate::turn_loop::openai::MODEL_SETTING)
                    .cloned()
                    .filter(|model| !model.is_empty())
                    .map(|model| (provider.id.clone(), model))
            })
            .collect();

        let state = Arc::new(Self {
            action_schemas: OnceLock::new(),
            activity,
            current_activity_run: Mutex::new(None),
            ws_tx,
            sse_tx,
            active_message: Mutex::new(None),
            stream_active_session: Mutex::new(None),
            transcript_replay_lock,
            history,
            audio_on: AtomicBool::new(audio_on),
            narr_tx,
            ask_tx,
            turn_accept_lock: tokio::sync::Mutex::new(()),
            provider_control_lock: tokio::sync::Mutex::new(()),
            mission_tx,
            switch_tx,
            interrupt_tx,
            provider: Mutex::new(provider_id),
            provider_generation: AtomicU64::new(0),
            providers: RwLock::new(providers),
            auth,
            openai: Mutex::new(OpenAiConfig::default()),
            byok: Mutex::new(byok),
            model_overrides: Mutex::new(model_overrides),
            ready: AtomicBool::new(false),
            warming: AtomicBool::new(true),
            provider_error: Mutex::new(None),
            busy: AtomicBool::new(false),
            awaiting,
            narration_buf: Mutex::new(String::new()),
            port,
            mcp_token: std::env::var("AGUI_MCP_TOKEN")
                .ok()
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty())
                .unwrap_or_else(|| MessageId::random().to_string()),
            mcp_agents: Mutex::new(HashMap::new()),
            people: crate::identity::People::new(),
            mcp_protocol_version: Mutex::new(None),
            current_focus: Mutex::new(None),
            decision: Mutex::new(HashMap::new()),
        });

        (
            state,
            RuntimeChannels {
                ask_rx,
                mission_rx,
                switch_rx,
                narr_rx,
            },
        )
    }

    /// Validate, compile, and install the effective action catalog exactly
    /// once. [`App::serve`](crate::App::serve) performs the same gate
    /// immediately after constructing its Surface; a low-level runtime user
    /// must call this before [`dispatch_tool`](crate::turn_loop::dispatch_tool).
    /// Keeping it separate from `new` lets narration/process-only runtimes omit
    /// an action vocabulary entirely.
    pub fn install_action_schemas(&self, actions: &[crate::ActionDef]) -> Result<(), String> {
        if self.action_schemas.get().is_some() {
            return Err("action schemas were already installed".to_string());
        }
        let schemas = ActionSchemas::compile(actions)?;
        self.install_compiled_action_schemas(schemas)
    }

    pub(crate) fn install_compiled_action_schemas(
        &self,
        schemas: ActionSchemas,
    ) -> Result<(), String> {
        self.action_schemas
            .set(schemas)
            .map_err(|_| "action schemas were already installed".to_string())
    }

    pub(crate) fn validate_action_arguments(
        &self,
        name: &str,
        arguments: &JsonValue,
    ) -> Result<(), String> {
        self.action_schemas
            .get()
            .ok_or_else(|| "action schemas were not compiled at startup".to_string())?
            .validate(name, arguments)
    }

    /// Register and broadcast one decision under the SSE replay boundary.
    /// A reconnect therefore receives the pending event from either its
    /// snapshot or its subscribed queue, never both and never neither.
    pub(crate) fn register_pending_decision(
        &self,
        id: String,
        pending: PendingDecision,
    ) -> Result<(), Box<PendingDecision>> {
        let _replay_guard = self.transcript_replay_lock.lock();
        let event_json = pending.event_json.clone();
        {
            let mut decisions = self.decision.lock();
            if decisions.contains_key(&id) {
                return Err(Box::new(pending));
            }
            decisions.insert(id, pending);
        }
        let _ = self.sse_tx.send(event_json);
        Ok(())
    }

    /// Generation-safe close used by an individual suspended waiter. An old
    /// future must never remove a newer decision that reused the same id.
    pub(crate) fn close_pending_decision_if_token(
        &self,
        id: &str,
        token: &str,
        reason: &str,
    ) -> Option<PendingDecision> {
        let _replay_guard = self.transcript_replay_lock.lock();
        let mut decisions = self.decision.lock();
        if decisions.get(id).map(|pending| pending.token.as_str()) != Some(token)
            || decisions
                .get(id)
                .is_some_and(|pending| pending.on_reply.is_none())
        {
            return None;
        }
        let pending = decisions.remove(id);
        drop(decisions);
        if pending.is_some() {
            self.broadcast_decision_closed(id, reason);
        }
        pending
    }

    /// Revoke every outstanding wait when its owning provider session exits.
    /// Dropping each reply sender wakes the suspended HTTP/tool task, while the
    /// keyed closed events make every connected/reconnecting client discard
    /// stale controls immediately.
    pub(crate) fn close_all_pending_decisions(&self, reason: &str) -> usize {
        let _replay_guard = self.transcript_replay_lock.lock();
        let mut decisions = self.decision.lock();
        let ids: Vec<String> = decisions
            .iter()
            .filter(|(_, pending)| pending.on_reply.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        let pending: Vec<PendingDecision> =
            ids.iter().filter_map(|id| decisions.remove(id)).collect();
        drop(decisions);
        let count = pending.len();
        for id in &ids {
            self.broadcast_decision_closed(id, reason);
        }
        count
    }

    pub(crate) fn broadcast_decision_closed(&self, id: &str, reason: &str) {
        let event = AgUiEvent::<JsonValue>::Custom(CustomEvent {
            base: BaseEvent::default(),
            name: "surface.decision.closed".to_string(),
            value: serde_json::json!({ "id": id, "reason": reason }),
        });
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = self.sse_tx.send(json);
        }
    }
}
