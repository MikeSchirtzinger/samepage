//! The AG-UI Surface runtime.
//!
//! **One line:** the reusable shell for "put an agent in your browser and
//! talk to it," where the agent can *see and touch the same live surface
//! you're looking at* — over any provider. AG-UI (`ag-ui-core`) is the
//! protocol; this crate is the application layer above it that everyone
//! currently rebuilds by hand.
//!
//! Source of truth: `docs/ag-ui-surface-spec.md` (§4 the `Surface` seam, §5
//! the `App` builder). This file is that spec made real.
//!
//! The runtime owns transport, provider selection, authentication, action
//! validation, MCP projection, and the provider-independent turn loop. Apps
//! contribute a [`Surface`] plus static client assets.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use ag_ui_core::event::{BaseEvent, Event as AgUiEvent, StateSnapshotEvent};
use ag_ui_core::JsonValue;

// Re-exported from ag-ui-core: `CustomEvent` (name/value payload on
// `BaseEvent`) is exactly the type `Effect::EmitAndAwait` needs — no reason
// to redefine it here.
pub use ag_ui_core::event::CustomEvent;

/// M1 Phase 1: the provider registry (ACP + OpenAI-compatible BYOK/subscription)
/// extracted verbatim from teaching-canvas's `providers.rs`. See its module docs
/// for the full shape.
pub mod providers;

/// M1 Phase 1: the Pi-style per-provider credential store (`auth.json`)
/// extracted verbatim from teaching-canvas's `auth.rs`. See its module docs for
/// the full shape.
pub mod auth;
pub mod config;

/// M2 Phase C: the generic-runtime half of teaching-canvas's old `AppState` —
/// transport channels, turn-protocol control channels, and provider/auth
/// config a turn loop drives. See its module docs.
pub mod runtime_state;

/// M2 Phase C: narration + barge-in chrome, lifted (and unified — see its
/// module docs) from teaching-canvas's `main.rs`.
pub mod narration;

/// M2 Phase C: the provider-agnostic turn loop itself (ACP subprocess +
/// OpenAI-compatible backends), lifted from teaching-canvas's
/// `acp.rs`/`openai.rs`, generic over [`Surface`]/[`SurfaceState`]. See its
/// module docs.
pub mod turn_loop;

/// Structure-driven diagram layout: the agent sends nodes + edges, the host
/// computes the placement. Shared by every surface that draws a graph, so
/// there is one answer to "what is a diagram" rather than one per example.
/// See its module docs.
pub mod diagram;

/// Host-owned, persistent operational activity with explicit coverage.
///
/// Records are stamped from runtime identity and survive with no browser open;
/// an Operational Trace is a view over this service, not another event owner.
pub mod activity;

/// Namespaced semantic targets plus host-owned, ephemeral reciprocal attention.
///
/// This is intentionally separate from CRDT awareness and from authorization:
/// presence and attention are context, never authority.
pub mod semantic_targets;

/// Who someone is, as distinct from what they may do.
///
/// Separate from [`Caller`] on purpose, and separate from `semantic_targets`
/// for a subtler reason: presence answers "who is here right now" and is
/// allowed to lapse, while identity answers "who wrote this" and must outlive
/// every connection that carried it.
pub mod identity;

/// Startup-compiled ActionDef schemas shared by every invocation origin.
mod action_schema;

/// Runtime-owned Model Context Protocol projection and request handling. The
/// public application API stays [`ActionDef`]/[`Surface::tools`]; this module
/// is wire-format plumbing used by the in-process `/mcp` endpoint.
mod mcp;

/// Typed, compile-time server extension composition plus the pinned
/// `agui.app.toml` recipe contract. The runtime still executes one `Surface`;
/// [`CompositeSurface`] is the adapter that makes several independently-owned
/// extension states/actions/routes look like that one surface without adding a
/// second dispatcher.
mod composition;
pub mod html;

/// Declared extension dependencies, host service requirements, and shared
/// document ownership.
///
/// [`Extension`] made one capability's parts travel together. This module makes
/// the parts an application used to supply by hand — a shared `Arc` from
/// `main`, a document a second extension writes, a capability string with no
/// provider — declarable, checkable against compiled code, and fatal at startup
/// when unmet.
pub mod services;

pub use activity::{
    ActivityActor, ActivityActorKind, ActivityCategory, ActivityCorrelation, ActivityCoverage,
    ActivityDurability, ActivityDurabilityStatus, ActivityEvent, ActivityFeed, ActivityOutcome,
    ActivitySnapshot, ActivityStateRevision, ActivitySubscription, InstrumentationCoverage,
    ObservationCoverage,
};
pub use composition::{
    AppRecipe, CompositeSurface, CompositionError, Extension, RecipeApp, RecipeExtension,
};
pub use runtime_state::AttachedAgent;
pub use semantic_targets::{
    Attention, AttentionMode, AttentionSnapshot, ComposerAnchor, Participant, ParticipantAwareness,
    ParticipantKind, SemanticTarget, SemanticTargetRef, SemanticTargetService,
};
pub use services::{
    DocumentClaim, DocumentRegistry, ExtensionDependency, ServiceRegistry, ServiceRequirement,
    SharedDocument, Transaction,
};

/// Owns the background workers attached to one [`App::serve`] invocation.
///
/// Dropping the server future (for example, when a test or embedding runtime
/// cancels it) must also cancel its narration/provider tasks. Those tasks own
/// subprocess RAII guards, so aborting them is what closes the complete
/// process tree instead of leaking an adapter after the listener disappears.
struct RuntimeTaskGuard {
    handles: Vec<tokio::task::JoinHandle<()>>,
}

impl RuntimeTaskGuard {
    fn new(handles: Vec<tokio::task::JoinHandle<()>>) -> Self {
        Self { handles }
    }

    async fn shutdown(mut self) {
        for handle in &self.handles {
            handle.abort();
        }
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }
}

impl Drop for RuntimeTaskGuard {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

/// Tiny UI-agnostic browser protocol client + manifest loader. Embedded so
/// every application gets the same core without copying a generated asset into
/// its own static directory.
const AGUI_CLIENT_JS: &str = include_str!("../client/ag-ui-client.js");
const PROVIDER_SETTINGS_JS: &str = include_str!("../client/provider-settings.js");
const PROVIDER_SETTINGS_CSS: &str = include_str!("../client/provider-settings.css");
const CONVERSATION_JS: &str = include_str!("../client/conversation.js");
const CONVERSATION_CSS: &str = include_str!("../client/conversation.css");

/// Declares how a Surface implementation owns its state. This is descriptive
/// snapshot/composition metadata for clients and diagnostics; the runtime does
/// not branch on it. Each Surface owns its backing-specific synchronization,
/// mutation, and persistence behind the generic state/effect hooks below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateBacking {
    /// True multi-writer convergence. yrs `Doc`. Canvas, co-editing.
    ///
    /// **Corrected in trait-review round 3 (M2, 2026-07-08), leak 2** — this
    /// variant's doc used to say "runtime runs CRDT sync + awareness," which
    /// M2's real implementation contradicts: the runtime's `/ws` route is a
    /// dumb subscribe-and-relay (see `ws_connection` in this file) with zero
    /// yrs dependency (`ag-ui-surface`'s `Cargo.toml` has none). The actual
    /// sync protocol — encode/decode sync frames, apply updates, produce
    /// replies — lives entirely behind [`SurfaceState::ws_hello`]/
    /// [`SurfaceState::ws_receive`] as opaque `Vec<u8>` frames a `Crdt`-backed
    /// Surface fills with real yrs logic (teaching-canvas's `CanvasState`)
    /// and every other backing rejects the transport as unsupported. This is the
    /// permanent design, not a stopgap: a `&dyn SurfaceState` trait object
    /// can't expose a generic "give me your yrs Doc" method without either
    /// leaking yrs into the trait for `LastWriterWins`/`RevisionHistory`/
    /// `Ephemeral` too, or maintaining a parallel per-backing sub-trait the
    /// runtime would still have to downcast into — both worse than the
    /// opaque-bytes seam already in place. "Awareness" (multi-cursor
    /// presence) is unimplemented by either layer today — aspirational, not
    /// a broken promise, since nothing claims otherwise elsewhere in the
    /// code; flagged here so a future skim of this doc doesn't assume it
    /// exists.
    Crdt,
    /// Single-writer authority. A form, a dashboard. The Surface decides how
    /// to persist and project replacements/reconnect state. A *persisted*
    /// single-value scope/focus pointer that survives reloads (Colab's
    /// `.colab/focus.json` — replace-not-merge, read back on every connect) is
    /// this, **not** [`Ephemeral`](Self::Ephemeral) — corrected per Colab-spec
    /// §1, which caught the trait doc's own "focus state → Ephemeral" example
    /// conflating it with teaching-canvas's different, TTL'd deixis pointer.
    LastWriterWins,
    /// Append-with-revision-history (jj/git). ADRs, decision logs.
    /// "Revise an early decision" = oplog rewind, NOT an edit. The Surface's
    /// store/effect implementation owns commit and reload; there is no generic
    /// runtime merge path.
    RevisionHistory,
    /// In-memory only; nothing durable survives a reload. Live choice-set
    /// panels; teaching-canvas's TTL'd deixis pointer (a genuinely
    /// in-memory-only "what am I pointing at" slot, cleared after `FOCUS_TTL`).
    /// The Surface owns any live projection. NB: a focus/scope pointer that
    /// must *survive* a reload is [`LastWriterWins`](Self::LastWriterWins), not this
    /// — the two share the English word "focus", not a backing (Colab-spec §1).
    Ephemeral,
    /// An application assembled from independently-backed extensions. Each
    /// child retains its own backing and reconnect payload; the composite
    /// snapshot namespaces those payloads by extension id instead of lying
    /// that the whole application is CRDT, LWW, revisioned, or ephemeral.
    Composite,
}

pub(crate) const BINARY_TRANSPORT_NOT_IMPLEMENTED: &str =
    "binary websocket transport is not implemented for this surface";

/// Fallible asynchronous PNG projection returned by [`SurfaceState`].
pub type SnapshotPngFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, String>> + Send + 'a>>;

/// What the state IS — the live view both human and agent look at. `describe`
/// and `snapshot_png` live here (they're properties of the state, not the
/// surface). A Surface picks its `backing`; the runtime does NOT assume yrs.
pub trait SurfaceState: Send + Sync + 'static {
    /// Declarative ownership/backing metadata; execution remains driven by the
    /// Surface's `Effect`, snapshot, reconnect-event, and opaque WS hooks.
    fn backing(&self) -> StateBacking;

    /// Text read-back: the agent's *structured* perception of the surface, in
    /// the agent's own coordinate space. Cheap and explicitly fallible so a
    /// contended or unavailable authoritative store cannot masquerade as an
    /// empty surface.
    /// (teaching-canvas: `describe_scene` → world-coord summary incl. drags.)
    fn describe(&self) -> Result<String, String>;

    /// Optional *pixel* perception: render the surface exactly as the human
    /// sees it — browser-free, deterministic, PNG bytes. Enables the visual
    /// feedback gate and image read-back to vision models.
    ///
    /// Boxed future, not `async fn`: `&dyn SurfaceState` is used as a trait
    /// object (`Surface::state()`), and native async-fn-in-trait isn't
    /// dyn-compatible without boxing. Spelled out explicitly here rather
    /// than pulling in `async-trait`, which would box every method on this
    /// trait (including the hot-path `describe()`/`snapshot()`) for the
    /// sake of this one. See trait-review round 2 (M1, 2026-07-08), Gap 4.
    /// (teaching-canvas: `render_scene_png` hops to a blocking thread for
    /// the wgpu render, hence async at all. Default `Box::pin(async {
    /// Ok(None) })` = text-only.)
    fn snapshot_png(&self) -> SnapshotPngFuture<'_> {
        Box::pin(async { Ok(None) })
    }

    /// Serialize for `STATE_SNAPSHOT` + reconnects. Crdt → yrs update;
    /// LastWriterWins → JSON; RevisionHistory → revision id + working-copy
    /// state; Ephemeral → JSON.
    fn snapshot(&self) -> Result<StateSnapshot, String>;

    /// Host-readable revision sampled immediately around an activity event.
    ///
    /// Revisions are opaque ordered labels owned by the state implementation.
    /// A composite namespaces each extension's entries. An empty vector is an
    /// explicit "this state exposes no revision", never a fabricated counter.
    fn activity_state_revision(&self) -> Result<Vec<ActivityStateRevision>, String> {
        Ok(Vec::new())
    }

    /// Runtime-installed host activity service. Application state returns
    /// `None`; the wrapper installed by [`App::serve`] supplies the feed.
    fn activity_feed(&self) -> Option<&ActivityFeed> {
        None
    }

    /// Resolve an object id to a short human-readable description, for
    /// deixis ("point at something, then say 'this'"). Distinct from the
    /// full-scene `describe()` — this is one object, already worded for
    /// splicing into a preamble. Deliberately `Option<String>`, not
    /// structured fields: kind/coords/color (teaching-canvas's `Focus`)
    /// don't generalize past canvas — see trait-review round 2, Gap 2.
    /// (teaching-canvas: `resolve_focus` + the wording half of
    /// `focus_preamble`, folded together.) Default `Ok(None)` = no focus support.
    fn resolve(&self, id: &str) -> Result<Option<String>, String> {
        let _ = id;
        Ok(None)
    }

    /// Resolve one stable namespaced target to typed semantics.
    ///
    /// The default upgrades the legacy [`Self::resolve`] text contract. A
    /// state with a better short label or a genuinely spatial view overrides
    /// this while still returning semantics rather than DOM or coordinates.
    fn semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        self.resolve(&target.target_id).map(|resolved| {
            resolved.map(|description| {
                SemanticTarget::new(target.clone(), description.clone(), description)
            })
        })
    }

    /// Access to the runtime-installed ephemeral attention service from an
    /// action effect. Ordinary application state returns `None`; the host
    /// wrapper installed by [`App::serve`] supplies it. This is never consulted
    /// by caller authorization.
    fn semantic_targets(&self) -> Option<&SemanticTargetService> {
        None
    }

    /// Opaque binary frames to send a freshly-connected client on the
    /// runtime's `/ws` transport, before live broadcast frames start
    /// flowing — a Surface's own wire protocol for resuming a live binary
    /// session (teaching-canvas: the yrs sync `greeting` frame + any live
    /// cloud-blob frames). Added in M2 Phase D, wiring `App::serve()`: `/ws`
    /// itself is fully generic (subscribe to the runtime's own `ws_tx`,
    /// forward verbatim — see [`crate::runtime_state::RuntimeState::ws_tx`]),
    /// but the CONTENT of those bytes is a CRDT-specific wire format
    /// (`ag_ui_canvas`'s y-sync framing) this crate deliberately does not
    /// depend on — same reasoning as `snapshot_png()`: opaque bytes handed
    /// to the runtime, never inspected. The default rejects `/ws` before the
    /// HTTP upgrade; a Surface with no binary transport uses `snapshot()`/SSE
    /// instead and must not appear to accept binary peers.
    fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
        Err(BINARY_TRANSPORT_NOT_IMPLEMENTED.to_string())
    }

    /// Apply one inbound binary frame from a `/ws` client (teaching-canvas: a
    /// yrs sync-step payload), returning any reply frames to broadcast back
    /// to every connected client (teaching-canvas: the yrs sync-step-2
    /// reply). See [`ws_hello`](Self::ws_hello)'s docs for why this is
    /// opaque bytes rather than a typed CRDT method. The default rejects the
    /// frame so no accepted work can disappear.
    fn ws_receive(&self, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let _ = data;
        Err(BINARY_TRANSPORT_NOT_IMPLEMENTED.to_string())
    }

    /// Named SSE custom-event replay entries for a freshly-connected/
    /// reloading client, derived from this state's own UI *chrome* — "chrome"
    /// in the interface-design sense (the panels/toolbars/widgets that frame
    /// the content), NOT the Chrome browser; these events are browser-agnostic.
    /// teaching-canvas: `[("canvas.chat", chat_config), ("canvas.widget",
    /// widget)]` (only when `widget` is set). Distinct from `snapshot()`'s
    /// `chrome` field: that's the wire-payload byte shape carried in one
    /// `STATE_SNAPSHOT`; this is the same data reprojected as the individual
    /// NAMED events a live session emits, since a reconnecting client needs
    /// them delivered under their own event names for the browser client to
    /// handle identically to a live update, not unpacked from one blob.
    /// Added in trait-review round 3 (M2, 2026-07-08) as leak 3; wired into
    /// `sse_handler` in the M5 follow-up (2026-07-10, renamed from
    /// `chrome_events`) so the runtime iterates this method instead of reading
    /// `chrome.get("chat_config")`/`"widget"` by name — the runtime no longer
    /// knows any `canvas.*` literal. Default empty: a Surface with nothing to
    /// replay returns nothing.
    ///
    /// This is reconnect UI-scaffold replay (panels/widgets), **not** a
    /// focus/deixis hook — unrelated to [`Surface::focus_events`]. A Surface
    /// whose reconnect state is one bundled `STATE_SNAPSHOT` (Colab) leaves
    /// this at the default and lets [`snapshot()`](Self::snapshot)'s `chrome`
    /// carry it instead (Colab-spec §6).
    fn reconnect_events(&self) -> Vec<(String, serde_json::Value)> {
        Vec::new()
    }
}

/// Fallible result returned by an action effect. The error text is safe to
/// return to a human, model, or MCP client as a retryable action failure.
pub type EffectResult<T> = Result<T, String>;

/// Synchronous read-only action body.
pub type QueryEffect = Box<dyn FnOnce(&dyn SurfaceState) -> EffectResult<String> + Send>;

/// Asynchronous read-only action body for bounded external work such as a real
/// browser capture.
pub type AsyncQueryEffect = Pin<Box<dyn Future<Output = EffectResult<String>> + Send>>;

/// State-mutating action body. `None` asks the runtime to emit its generic
/// acknowledgement after the mutation succeeds.
pub type MutateEffect = Box<dyn FnOnce(&dyn SurfaceState) -> EffectResult<Option<String>> + Send>;

/// Asynchronous state-mutating action body. The future is awaited by the
/// authoritative dispatcher, so external work and delayed transitions cannot
/// report success before their mutation finishes or hide a later failure.
pub type AsyncMutateEffect = Pin<Box<dyn Future<Output = EffectResult<Option<String>>> + Send>>;

/// Durable-store action body.
pub type CommitEffect = Box<dyn FnOnce(&dyn SurfaceStore) -> EffectResult<Option<String>> + Send>;

/// Human-decision reply interpreter.
pub type DecisionReply = Box<dyn FnOnce(serde_json::Value) -> EffectResult<String> + Send>;

/// What a tool does to state. NOT assumed to be a CRDT edit. The runtime
/// dispatches on `Effect` so Vellum's `accept_adr` (a jj commit) and
/// `render_choice_set` (a turn handoff) are first-class, not forced through a
/// CRDT-shaped hole.
///
/// Every variant except `EmitAndAwait` carries (or can carry) a **reply
/// string for the agent** — added in M2 Phase A
/// (`ag-ui-surface-m2-handoff.md` §2, the `apply_tool` row) once the generic
/// turn loop's contract was traced through teaching-canvas's real
/// `apply_tool -> Option<String>`, which turned out to have three shapes a
/// bare `Mutate(FnOnce(&dyn SurfaceState))` couldn't express:
///   1. `read_canvas` — a pure query, no mutation at all, always replies.
///   2. `diagram` — mutates AND replies with a summary that may depend on
///      the mutation having already happened (e.g. resolved layout/ids).
///   3. `plot_points`/`clear`/`axes`/etc — mutate, no specific reply (the
///      runtime supplies a generic ack, mirroring `openai.rs`'s existing
///      `.unwrap_or_else(|| format!("applied {name}"))` fallback).
///
/// `Query` is a separate variant rather than folding case 1 into `Mutate`
/// with an always-`Some` reply and a no-op body: keeping "touches state" vs
/// "doesn't" a real type-level distinction lets a later runtime phase treat
/// them differently (e.g. skip the post-effect CRDT broadcast/sync a
/// `Mutate` implies) without inspecting closure behavior to find out. Case 2
/// is handled by `Mutate`'s closure computing the reply itself, AFTER
/// mutating, inside the same `FnOnce` — no two-phase apply-then-effect split
/// needed, since a tool's `apply` closure already captures its own concrete
/// state (see `ToolDef::apply`'s doc comment), so "mutate, then read back
/// what changed" is just sequential code in one closure body, not two
/// separate calls that could observe different state.
pub enum Effect {
    /// Read state without touching it; always produces a reply for the
    /// agent (teaching-canvas: `read_canvas` → `describe_scene`).
    Query(QueryEffect),
    /// Read state while awaiting bounded external work. This keeps blocking
    /// browser/process I/O off the async runtime's worker threads.
    AsyncQuery(AsyncQueryEffect),
    /// Mutate the live state (CRDT apply / LWW set / ephemeral update), then
    /// optionally reply. `Some(text)` when the mutation has something
    /// specific to report back (teaching-canvas: `diagram`'s post-layout
    /// summary); `None` for a plain draw with no special reply
    /// (`plot_points`/`clear`/`axes`/…) — the runtime supplies a generic ack.
    Mutate(MutateEffect),
    /// Await bounded asynchronous work that mutates state, then optionally
    /// reply. Unlike [`Effect::AsyncQuery`], dispatch does not classify this as
    /// a read and will not let a background task outlive a successful action
    /// acknowledgement.
    AsyncMutate(AsyncMutateEffect),
    /// Commit a revision to the durable ledger (jj). A `reload` then re-syncs
    /// `state()` from the working copy. This is how Vellum's `accept_adr`
    /// time-travels via `jj op restore` — an op CRDT/LWW cannot express.
    /// Reply-carrying for the same reason as `Mutate` (a generic turn loop
    /// needs *some* tool-result text regardless of which effect ran).
    Commit {
        message: String,
        apply: CommitEffect,
    },
    /// Emit a panel/event and suspend the turn until the human posts a
    /// semantic reply for THIS decision. The turn-protocol primitive Vellum's
    /// loop is built on (`render_choice_set` → `OptionChosen` / `BlankFilled`)
    /// and Colab's approve/reject wait.
    ///
    /// **Keyed, not single-slot** (Colab-spec §8.1, the one M5-blocking
    /// correction over Leak 4's original design): `id` keys the pending
    /// decision in [`crate::runtime_state::RuntimeState::decision`] so several
    /// decisions can be outstanding at once — Colab's batch pacing mode races
    /// 2–3 named variants concurrently, each awaiting its own reply, and a
    /// single `Option<PendingDecision>` slot would silently clobber all but
    /// the last. Vellum only ever has one live, but the keyed API costs it
    /// nothing.
    ///
    /// Unlike the other reply-carrying variants the reply isn't produced
    /// synchronously: the runtime stashes the decision, emits `event`, then
    /// races the human's reply (delivered to `POST /decision` as `{id, …}`)
    /// against barge-in. `on_reply` translates that raw reply JSON into the
    /// tool-result text fed back to the agent — the same "closure interprets
    /// its own raw JSON" contract the other variants use, just deferred.
    EmitAndAwait {
        /// Decision key. The emitted `event` must carry this so the browser
        /// can echo it back on `POST /decision`, and so concurrent decisions
        /// stay distinct in the runtime's keyed decision map.
        id: String,
        event: CustomEvent,
        reply_kind: ReplyKind,
        /// Maps the human's posted reply JSON to the agent-facing tool result.
        on_reply: DecisionReply,
    },
    /// The tool rejecting its own call — invalid arguments, a failed
    /// precondition — so the agent retries rather than proceeding. The runtime
    /// feeds `.0` back to the agent as the tool result AND signals the HTTP
    /// action route (`POST /surface/action`; legacy `/canvas-tool`) to answer
    /// **422**, so an MCP/ACP bridge
    /// surfaces it as a retryable tool error, not a success. Vellum's
    /// `render_choice_set` validation is what needs this; teaching-canvas never
    /// produces it.
    Reject(String),
}

/// The one thing a use case implements. Everything else — providers,
/// transport, chat, HITL, feedback — the runtime provides for free.
pub trait Surface: Send + Sync + 'static {
    /// The live view. For teaching-canvas this IS the state (Crdt). For Vellum
    /// this is the ephemeral projection; `store()` is the durable ledger.
    fn state(&self) -> &dyn SurfaceState;

    /// The vocabulary the agent can call. Each `ToolDef`'s `apply` returns an
    /// [`Effect`] the runtime dispatches — NOT a raw CRDT edit. The runtime
    /// dispatches these AND auto-bridges them to ACP agents over MCP —
    /// you never write an MCP server again.
    fn tools(&self) -> &[ToolDef];

    /// Browser-side extensions that render or interact with `state()`, mounted
    /// inside the runtime's client shell. Returning a vector is intentional:
    /// one server Surface may compose a board, uploads, diagrams, and a WebGPU
    /// view while the protocol core remains unchanged.
    fn client_modules(&self) -> Vec<ClientModule>;

    /// The durable projection. For teaching-canvas: peripheral learner model
    /// (None or small). For Vellum: the PRIMARY state — the ADR log — and
    /// `state()` is its live projection. Optional by capability, NOT by
    /// priority: an entire class of decision/ledger apps inverts the default.
    fn store(&self) -> Option<&dyn SurfaceStore> {
        None
    }

    /// Extra HTTP routes this Surface needs beyond the tool-call vocabulary —
    /// browser-side hydrate/read endpoints with no agent turn involved
    /// (teaching-canvas: `GET/POST /mission`; Vellum: `GET /artifacts`).
    /// Framework-neutral by design: the runtime mounts these as real axum
    /// routes internally, but the trait never names axum, so a Surface's
    /// `Cargo.toml` never has to either. See trait-review round 2 (M1,
    /// 2026-07-08), Gap 1. Default empty: most Surfaces need nothing here,
    /// everything goes through `tools()`.
    fn routes(&self) -> Vec<RouteDef> {
        Vec::new()
    }

    /// Form/JSON mutation routes backed by an existing [`ActionDef`].
    ///
    /// Unlike [`Self::routes`], these routes do not call an extension handler
    /// to mutate state. The runtime establishes a [`Caller`], enforces the
    /// named action's [`ActionAudience`], validates its schema, and invokes it
    /// through [`turn_loop::dispatch_tool`] before calling the route's
    /// success renderer. A missing caller remains [`Caller::Unknown`] and is
    /// refused before dispatch.
    fn action_routes(&self) -> Vec<ActionRouteDef> {
        Vec::new()
    }

    /// The `POST /semantic` event names this Surface treats as a **deixis /
    /// pointer gesture** worth resolving into a live referent via
    /// [`SurfaceState::resolve(id)`](SurfaceState::resolve) — teaching-canvas:
    /// `["canvas.focus", "canvas.selected", "canvas.dragged"]`, emitted by
    /// `ag-ui-canvas-web`'s WASM renderer, so "this"/"that" in the next `/ask`
    /// binds to the clicked object.
    ///
    /// **Narrowly the deixis-pointer vocabulary, NOT a generic "this Surface's
    /// notion of focus" hook** (Colab-spec §6/§8.2): a Surface whose "focus"
    /// is a *persisted scope pointer* (Colab's `/focus`, a named tree path that
    /// is [`StateBacking::LastWriterWins`] state behind its own [`RouteDef`],
    /// not an opaque id needing `resolve()`) declares nothing here — that
    /// feature is unrelated to this hook despite sharing the word "focus".
    ///
    /// Added in trait-review round 3 (M2, 2026-07-08) as leak 3, to get these
    /// literal strings out of `semantic_handler`; that handler now reads this
    /// method (`st.surface.focus_events().contains(&name)`) instead of matching
    /// the literals — wired in the M5 follow-up (2026-07-10). Default empty: a
    /// Surface with no pointing vocabulary declares nothing here.
    fn focus_events(&self) -> &[&str] {
        &[]
    }

    /// Told who is about to call an action, immediately before dispatch.
    ///
    /// [`ActionDef::apply`] receives arguments and nothing else, deliberately —
    /// threading a caller through every surface's every tool to serve the one
    /// or two that care would be a poor trade. This hook is the narrow way to
    /// care: a surface that records authorship (the room stamps every pane
    /// with who wrote it) stashes the value and reads it while applying.
    ///
    /// Default is a no-op, so a surface that does not track authorship is
    /// unaffected.
    ///
    /// The value is last-writer-wins across concurrent dispatches. That is
    /// sound for authorship because a surface recording it also serialises its
    /// writes — the room rejects any edit carrying a stale `expected_revision`
    /// — so two callers cannot both be mid-write. Do not use this hook for
    /// anything where a torn read would be a security decision; the enforced
    /// permission check is [`Caller::may_call`] in
    /// [`turn_loop::dispatch_tool`], and it does not consult this.
    fn note_caller(&self, _actor: &Actor) {}

    /// Resolve one declared semantic focus event. The default preserves the
    /// original single-Surface behavior. [`CompositeSurface`] overrides this
    /// to route the id only to the extension that owns `event`, avoiding id
    /// collisions between independently composed states.
    fn resolve_focus(&self, event: &str, id: &str) -> Result<Option<String>, String> {
        if self.focus_events().contains(&event) {
            self.state().resolve(id)
        } else {
            Ok(None)
        }
    }

    /// Extension ids whose stable semantic targets this surface registers.
    ///
    /// Empty by default, so a surface never advertises host attention actions
    /// until it explicitly provides a namespace.
    fn semantic_target_namespaces(&self) -> Vec<String> {
        Vec::new()
    }

    /// Resolve an exact namespaced target. Composite surfaces override this to
    /// route only to the extension owning `extension_id`.
    fn resolve_semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        if self
            .semantic_target_namespaces()
            .iter()
            .any(|namespace| namespace == &target.extension_id)
        {
            self.state().semantic_target(target)
        } else {
            Ok(None)
        }
    }
}

/// Wire payload for `STATE_SNAPSHOT` + reconnects — what a late-joining or
/// reloading client is handed to reconstruct the surface. The runtime emits
/// this concrete payload on every SSE connection before live events. `body`
/// carries the backing-specific state; `chrome` carries optional ancillary UI
/// state that must replay at the same connection boundary.
#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    /// Mirrors the originating [`SurfaceState::backing()`] for client-side
    /// interpretation, diagnostics, and composite namespacing. The runtime
    /// serializes this metadata but does not branch on it or inspect `body`.
    pub backing: StateBacking,
    /// Surface-defined state payload. It is intentionally opaque `Value`: the
    /// runtime only transports it, while a Surface's `snapshot()` and browser
    /// client agree on its concrete shape.
    pub body: serde_json::Value,
    /// Optional Surface-specific UI-chrome, replayed to late-joining /
    /// reloaded clients alongside `body` — teaching-canvas's `chat_config` +
    /// `widget`. `None` when a Surface has none (most won't). Opaque JSON
    /// for the same reason as `body`: the runtime only ships it, it never
    /// inspects it.
    pub chrome: Option<serde_json::Value>,
}

/// The browser-side half of one enabled extension.
///
/// This is deliberately runtime metadata, not a remote plugin descriptor. The
/// Rust host decides which extensions exist, validates their same-origin entry
/// modules at startup, and exposes only that allowlist through `GET /extensions`.
/// A marketplace can distribute source packages and configuration recipes, but
/// installing/enabling one is a separate trusted host operation; opening a page
/// never downloads native code or imports an arbitrary third-party origin.
///
/// `module` is optional only while older, host-bundled examples migrate. New
/// extensions should provide one ES-module entrypoint exporting `activate(ctx)`
/// (or a default function). The shared `/_agui/client.js` loader imports it
/// lazily, after the manifest arrives and before opening the AG-UI event stream,
/// so reconnect state cannot race ahead of extension registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientModule {
    /// Stable, marketplace-safe extension id (`board`, `canvas.webgpu`, ...).
    pub id: String,
    /// Extension contract version. Kept as a string so packages can use semver
    /// without making the core runtime a package-manager implementation.
    pub version: String,
    /// Same-origin ES-module URL, normally `/extensions/<id>/index.js`.
    /// `None` means a legacy host-bundled view; the manifest reports it but the
    /// lazy loader does not pretend it loaded anything.
    pub module: Option<String>,
    /// A compiled wasm renderer bundle to mount, if the Surface's view is
    /// *drawn* (not DOM-manipulated). `None` for a DOM-native Surface.
    pub wasm: Option<WasmMount>,
    /// The DOM id the shell reserves for this Surface to mount/draw into —
    /// teaching-canvas: the `<canvas>` element id; a DOM Surface: the container
    /// wrapping its own view.
    pub mount_id: String,
    /// Custom event names this browser module consumes. Informational today;
    /// useful to installers, diagnostics, and future permission tooling.
    pub events: Vec<String>,
    /// Names from [`Surface::tools`] this extension owns. `None` means a sole
    /// module owns every Surface action. Composed Surfaces must use `.action()`
    /// or `.no_actions()` for every module so ownership is never ambiguous.
    pub actions: Option<Vec<String>>,
    /// Open capability vocabulary (`dom`, `wasm`, `webgpu`, `filesystem`, ...).
    /// Strings are intentional: a shared marketplace must be able to introduce
    /// capabilities without requiring a core release merely to parse metadata.
    pub capabilities: Vec<String>,
    /// Required extensions surface a visible load failure; optional ones may
    /// fail while the protocol/chat core remains usable.
    pub required: bool,
}

impl ClientModule {
    /// A manifest-driven, lazily imported browser extension.
    pub fn lazy(
        id: impl Into<String>,
        version: impl Into<String>,
        module: impl Into<String>,
        mount_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            module: Some(module.into()),
            wasm: None,
            mount_id: mount_id.into(),
            events: Vec::new(),
            actions: None,
            capabilities: Vec::new(),
            required: true,
        }
    }

    /// A legacy page-integrated extension. It is still discoverable, but the
    /// shared loader reports `loading: "host"` and does not claim a lazy import.
    pub fn host(
        id: impl Into<String>,
        version: impl Into<String>,
        mount_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            module: None,
            wasm: None,
            mount_id: mount_id.into(),
            events: Vec::new(),
            actions: None,
            capabilities: Vec::new(),
            required: true,
        }
    }

    pub fn wasm(mut self, wasm: WasmMount) -> Self {
        self.wasm = Some(wasm);
        self
    }

    pub fn event(mut self, name: impl Into<String>) -> Self {
        self.events.push(name.into());
        self
    }

    pub fn action(mut self, name: impl Into<String>) -> Self {
        self.actions.get_or_insert_with(Vec::new).push(name.into());
        self
    }

    pub fn no_actions(mut self) -> Self {
        self.actions = Some(Vec::new());
        self
    }

    pub fn capability(mut self, name: impl Into<String>) -> Self {
        self.capabilities.push(name.into());
        self
    }

    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || !self.id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
        {
            return Err(format!(
                "extension id {:?} must contain only lowercase ASCII letters, digits, '.', '_' or '-'",
                self.id
            ));
        }
        if self.version.trim().is_empty() {
            return Err(format!("extension {} has an empty version", self.id));
        }
        if self.mount_id.trim().is_empty() {
            return Err(format!("extension {} has an empty mount id", self.id));
        }
        for (kind, values) in [
            ("event", self.events.as_slice()),
            ("capability", self.capabilities.as_slice()),
            ("action", self.actions.as_deref().unwrap_or(&[])),
        ] {
            let mut seen = std::collections::HashSet::new();
            for value in values {
                if value.trim().is_empty() || !seen.insert(value) {
                    return Err(format!(
                        "extension {} has an empty or duplicate {kind}: {:?}",
                        self.id, value
                    ));
                }
            }
        }
        if let Some(module) = &self.module {
            let valid = module.starts_with('/')
                && !module.starts_with("//")
                && !module.contains("..")
                && !module.contains('\\')
                && !module.contains("://");
            if !valid {
                return Err(format!(
                    "extension {} module {:?} must be a same-origin absolute path",
                    self.id, module
                ));
            }
        }
        if let Some(wasm) = &self.wasm {
            for (kind, path) in [("js", &wasm.js), ("wasm", &wasm.wasm)] {
                let valid = path.starts_with('/')
                    && !path.starts_with("//")
                    && !path.contains("..")
                    && !path.contains('\\')
                    && !path.contains("://");
                if !valid {
                    return Err(format!(
                        "extension {} wasm {kind} path {:?} must be a same-origin absolute path",
                        self.id, path
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A compiled wasm renderer bundle (see [`ClientModule::wasm`]). Asset URLs
/// are same-origin paths, normally under the runtime's `/pkg` mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmMount {
    /// The wasm-pack package / crate name (teaching-canvas: `ag_ui_canvas_web`).
    pub pkg_name: String,
    /// The JS loader URL (e.g. `/pkg/ag_ui_canvas_web.js`).
    pub js: String,
    /// The wasm binary URL (e.g. `/pkg/ag_ui_canvas_web_bg.wasm`).
    pub wasm: String,
}

/// Wire format returned by `GET /extensions`. This is the active runtime
/// allowlist, not a marketplace catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionManifest {
    pub schema_version: u32,
    pub protocol: String,
    pub extensions: Vec<ExtensionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDescriptor {
    pub id: String,
    pub version: String,
    pub loading: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub mount: String,
    pub actions: Vec<String>,
    pub events: Vec<String>,
    pub capabilities: Vec<String>,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wasm: Option<WasmMount>,
}

impl ExtensionManifest {
    fn from_surface(surface: &dyn Surface) -> Result<Self, String> {
        let modules = surface.client_modules();
        let human_actions: Vec<String> = surface
            .tools()
            .iter()
            .filter(|action| action.audience.human())
            .map(|action| action.name.clone())
            .collect();
        let mut ids = std::collections::HashSet::new();
        let mut mounts = std::collections::HashSet::new();
        let mut extensions = Vec::with_capacity(modules.len());

        let composed = modules.len() > 1;
        for module in modules {
            module.validate()?;
            if !ids.insert(module.id.clone()) {
                return Err(format!("duplicate extension id: {}", module.id));
            }
            if !mounts.insert(module.mount_id.clone()) {
                return Err(format!("duplicate extension mount: {}", module.mount_id));
            }
            let actions = match module.actions {
                None if composed => {
                    return Err(format!(
                        "extension {} must declare action ownership in a composed Surface",
                        module.id
                    ));
                }
                None => human_actions.clone(),
                Some(actions) => {
                    for action in &actions {
                        if !human_actions.contains(action) {
                            return Err(format!(
                                "extension {} declares unknown or non-human action {action}",
                                module.id
                            ));
                        }
                    }
                    actions
                }
            };
            let loading = if module.module.is_some() {
                "lazy"
            } else {
                "host"
            };
            extensions.push(ExtensionDescriptor {
                id: module.id,
                version: module.version,
                loading: loading.to_string(),
                module: module.module,
                mount: module.mount_id,
                actions,
                events: module.events,
                capabilities: module.capabilities,
                required: module.required,
                wasm: module.wasm,
            });
        }

        Ok(Self {
            schema_version: 1,
            protocol: "ag-ui".to_string(),
            extensions,
        })
    }
}

fn validate_extension_assets(
    manifest: &ExtensionManifest,
    static_dir: &Path,
    pkg_dir: Option<&Path>,
    mounts: &[(String, PathBuf)],
) -> Result<(), String> {
    for extension in &manifest.extensions {
        let mut assets = Vec::new();
        if let Some(module) = &extension.module {
            assets.push(("module", module.as_str()));
        }
        if let Some(wasm) = &extension.wasm {
            assets.push(("wasm JS", wasm.js.as_str()));
            assets.push(("wasm binary", wasm.wasm.as_str()));
        }
        for (kind, url) in assets {
            let Some(path) = extension_asset_path(url, static_dir, pkg_dir, mounts) else {
                return Err(format!(
                    "extension {} {kind} {url:?} has no matching static mount",
                    extension.id
                ));
            };
            if !path.is_file() {
                return Err(format!(
                    "extension {} {kind} {url:?} is missing at {}",
                    extension.id,
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn extension_asset_path(
    url: &str,
    static_dir: &Path,
    pkg_dir: Option<&Path>,
    mounts: &[(String, PathBuf)],
) -> Option<PathBuf> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(relative) = path.strip_prefix("/pkg/") {
        return pkg_dir.map(|dir| dir.join(relative));
    }
    for (prefix, dir) in mounts {
        let prefix = format!("/{}", prefix.trim_matches('/'));
        if let Some(relative) = path.strip_prefix(&format!("{prefix}/")) {
            return Some(dir.join(relative));
        }
    }
    Some(static_dir.join(path.trim_start_matches('/')))
}

/// How a [`Effect::EmitAndAwait`] reply should be interpreted — carried on the
/// stashed [`crate::runtime_state::PendingDecision`] so the live typed
/// `POST /decision` validator can reject malformed replies without consuming
/// the pending wait. The `on_reply` closure performs the Surface-specific
/// conversion only after this shared validation succeeds.
pub enum ReplyKind {
    /// Any decision object after the shared route has validated its matching
    /// `id`; the `on_reply` closure owns all remaining bespoke fields.
    Any,
    /// A typed approve/reject verdict. Rejections can require the redirect
    /// comment the agent must act on.
    Approval { require_reject_comment: bool },
    /// A choice-set pick whose id must belong to the panel that was emitted.
    /// Options listed in `text_required_for` additionally require a non-empty
    /// human-authored `text` field (for example, an "other" choice).
    Choice {
        allowed: Vec<String>,
        text_required_for: Vec<String>,
    },
}

/// Durable context seam implemented by teaching-canvas's filesystem workspace
/// and Vellum's jj-backed ADR store.
pub trait SurfaceStore: Send + Sync {
    /// Whatever standing context this store contributes to the agent's
    /// system/prime prompt, formatted as ready-to-append prose — teaching-
    /// canvas's mission + recent learning records (`WorkspaceStore` in
    /// `examples/teaching-canvas/src/canvas_surface.rs`) or Vellum's recent
    /// ADRs. The live generic loop reads this on every prime. Added in M2 Phase C
    /// (`ag-ui-surface-m2-handoff.md` §4 step 5's `TUTOR_METHOD`/
    /// `build_context`/`build_prime` row) as the one generic hook the lifted
    /// turn loop's context assembly needs — see [`crate::turn_loop::build_context`].
    /// Deliberately a single opaque-prose method, not `read_mission()`/
    /// `recent_records(n)`-shaped: those are teaching-canvas's own data
    /// model and don't generalize to a jj ADR log, whereas "some prose to
    /// append to the prompt" does. Default empty — most stores contribute
    /// nothing here (a store's *primary* content is read through
    /// `Surface::state()`, not this — this is only for standing context
    /// repeated on every (re)prime).
    fn context(&self) -> String {
        String::new()
    }
}

/// Who may discover an action. This is visibility metadata, not a second
/// execution path: both actors still reach the same [`ActionDef::apply`]
/// closure through the runtime dispatcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionAudience {
    Human,
    Agent,
    Both,
}

impl ActionAudience {
    pub fn human(self) -> bool {
        matches!(self, Self::Human | Self::Both)
    }

    pub fn agent(self) -> bool {
        matches!(self, Self::Agent | Self::Both)
    }
}

/// Who is asking to run an action.
///
/// [`ActionAudience`] declares who an action is *for*; this says who is
/// actually calling. [`turn_loop::dispatch_tool`] compares the two, and it is
/// deliberately the **only** place that comparison is guaranteed to happen.
///
/// Enforcing per transport does not work. Every transport that reaches an
/// agent — the `/mcp` catalog, its executable re-check, the OpenAI adapter —
/// filtered on `audience.agent()`, while `POST /surface/action` filtered on
/// nothing, so `audience` was a boundary in three places and a listing
/// convention in the fourth. Requiring the caller here means a new transport
/// cannot be written without answering the question.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Caller {
    /// The person, through the browser: an action-backed form route with a
    /// host-issued browser-session credential, or `POST /surface/action`
    /// (legacy alias `/canvas-tool`).
    Human,
    /// A model: the runtime-owned `/mcp` endpoint, or an in-process provider
    /// adapter dispatching a tool call it just streamed.
    Agent,
    /// A model that is *not* this surface's own agent — an outside assistant
    /// reaching in over `POST /surface/action` with `"as": "companion"`.
    ///
    /// This variant exists because two of them were being conflated. The
    /// audience split assumes browser = human and `/mcp` = agent, so an
    /// assistant driving the surface over HTTP arrived as [`Caller::Human`]:
    /// it could not call agent-only actions, and anything it did write was
    /// attributed to the person sitting there. Neither is true, and the
    /// second is worse than an inconvenience — a surface whose whole purpose
    /// is establishing who said what should not put the human's name on a
    /// machine's writing.
    ///
    /// For *authorisation* a companion is an agent: same catalogue, same
    /// permissions, so no action has to be re-declared to admit one. What it
    /// buys is an honest byline, which surfaces read through
    /// [`Surface::note_caller`].
    Companion,
    /// An explicitly supplied caller identity the runtime does not recognize.
    ///
    /// This is deliberately distinct from [`Caller::Human`]. Treating a
    /// misspelled, malformed, or future identity as a person forges both
    /// authorization and attribution. Unknown callers have no action
    /// audience and are rejected before dispatch.
    Unknown,
}

impl Caller {
    /// Whether this caller may run an action declared for `audience`.
    pub fn may_call(self, audience: ActionAudience) -> bool {
        match self {
            Self::Human => audience.human(),
            // A companion is a model; it gets the model's permissions.
            Self::Agent | Self::Companion => audience.agent(),
            Self::Unknown => false,
        }
    }

    /// How to name this caller in a refusal the other side will read.
    pub fn label(self) -> &'static str {
        match self {
            Self::Human => "the human",
            Self::Agent => "this agent",
            Self::Companion => "the companion agent",
            Self::Unknown => "an unknown caller",
        }
    }

    /// Parse the `as` field of a `POST /surface/action` body.
    ///
    /// An omitted field is the browser's established human transport. An
    /// explicitly supplied value must be recognized; otherwise it remains
    /// [`Caller::Unknown`] and cannot pass any audience gate.
    pub fn parse(value: Option<&str>) -> Caller {
        match value {
            None => Caller::Human,
            Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                "human" => Caller::Human,
                "companion" => Caller::Companion,
                _ => Caller::Unknown,
            },
        }
    }
}

/// Who is acting, for **attribution** — as distinct from [`Caller`], which
/// decides what they are allowed to do.
///
/// These were the same value until identity existed, and that is precisely why
/// only one agent byline was possible: there are four `Caller` variants, so
/// there were four expressible authors, and every model that ever wrote to a
/// surface signed the same one. Splitting them lets a surface record *which*
/// agent wrote something without changing who may write.
///
/// `label` is a display name the host disambiguated, never an authorization
/// input. Nothing may branch on it for permission — that remains
/// [`Caller::may_call`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    pub caller: Caller,
    /// Room-unique display name, when this actor announced itself.
    pub label: Option<String>,
    /// Host-minted participant id, stable across every connection this actor
    /// makes, not just the current one.
    ///
    /// The distinction matters: a session id would change when a laptop sleeps
    /// or a model reconnects, and a surface that stored one on a byline would
    /// show two authors where there was one person who reloaded.
    pub participant_id: Option<String>,
    /// Display colour, assigned at join from the principal. Present for a
    /// person, and for an agent it is the colour of the person responsible for
    /// it, so the chain of responsibility is visible without being read.
    pub hue: Option<u16>,
    /// The [`Principal`](identity::Principal) key behind this actor: the person
    /// themselves, or for an agent the person who brought it.
    pub responsible: Option<String>,
}

tokio::task_local! {
    /// The actor whose request is being served on this task.
    ///
    /// A request is one task, so a value scoped here cannot be observed by any
    /// other request. That is the property `note_caller` alone cannot give: it
    /// records the most recent caller on shared state, and between recording
    /// and the write the handler makes there are awaits during which another
    /// request can record itself. A byline read from the shared slot after such
    /// an interleaving signs one participant's write with another's name.
    static CURRENT_ACTOR: Actor;
}

/// Run `f` with `actor` as the actor of the current request.
///
/// Every HTTP entry that dispatches an action wraps the dispatch in this, so a
/// surface reading [`current_actor`] inside an action handler sees the caller
/// of that request and never a concurrent one.
pub async fn with_actor<F: std::future::Future>(actor: Actor, f: F) -> F::Output {
    CURRENT_ACTOR.scope(actor, f).await
}

/// The actor of the request being served, when dispatch ran under
/// [`with_actor`]. `None` outside a request, such as an in-page turn loop or a
/// unit test, where the surface falls back to whatever it last noted.
pub fn current_actor() -> Option<Actor> {
    CURRENT_ACTOR.try_with(|actor| actor.clone()).ok()
}

impl Actor {
    /// An actor that never said who it is — today's behaviour for every
    /// caller that has not attached with an identity.
    pub fn anonymous(caller: Caller) -> Self {
        Self {
            caller,
            label: None,
            participant_id: None,
            hue: None,
            responsible: None,
        }
    }

    /// An agent that attached over `/mcp` and announced itself.
    pub fn attached(caller: Caller, agent: &AttachedAgent) -> Self {
        Self {
            caller,
            label: Some(agent.label.clone()),
            participant_id: Some(agent.participant_id.clone()),
            hue: None,
            responsible: agent.responsible.clone(),
        }
    }

    /// A person who joined and holds a resume token.
    ///
    /// Deliberately not reachable from a request body. A person's identity is
    /// resolved from a token the host minted and the browser presents, never
    /// from a field a caller could fill in, because a byline anyone can choose
    /// is not a byline.
    pub fn person(caller: Caller, person: &identity::Person) -> Self {
        Self {
            caller,
            label: Some(person.name.clone()),
            participant_id: Some(person.participant_id.clone()),
            hue: Some(person.hue),
            responsible: Some(person.principal.key()),
        }
    }

    /// The name to sign a write with: the announced label when there is one,
    /// otherwise the generic role. Never empty.
    pub fn byline(&self) -> &str {
        match self.label.as_deref() {
            Some(label) => label,
            None => match self.caller {
                Caller::Human => "human",
                Caller::Agent => "agent",
                Caller::Companion => "companion",
                Caller::Unknown => "unknown",
            },
        }
    }
}

/// Pairs an action's wire schema (name/description/JSON-Schema parameters — the
/// shape teaching-canvas's `tools::ToolSpec` already has, see
/// `examples/teaching-canvas/src/tools.rs::specs()`) with an `apply` closure
/// the runtime calls on every tool invocation. `apply` takes the call's JSON
/// arguments and returns an [`Effect`] for the runtime to dispatch — it does
/// NOT mutate state directly (see `Effect`'s docs: `Mutate`/`Commit`/
/// `EmitAndAwait` cover CRDT edits, jj commits, and turn-protocol handoffs
/// uniformly, so no one tool vocabulary is forced through a CRDT-shaped
/// hole). Fleshed out in M2 Phase A (`ag-ui-surface-m2-handoff.md` §4 step
/// 1); the runtime auto-bridges these to ACP agents over MCP (see
/// [`Surface::tools()`]).
#[derive(Clone)]
pub struct ActionDef {
    pub name: String,
    pub description: String,
    /// JSON-Schema object for the tool's arguments — an MCP `inputSchema` /
    /// OpenAI function `parameters` object. It must declare root `type=object`.
    /// At startup the runtime meta-validates and compiles every effective
    /// action schema (including human-only actions), using JSON Schema 2020-12
    /// when `$schema` is absent and honoring supported explicit dialects. The
    /// shared dispatcher validates each call against that compiled contract
    /// before `apply` for every origin. OpenAI and MCP project this exact value
    /// directly; applications do not maintain backend renderers or validators.
    pub parameters: serde_json::Value,
    /// Whether humans, agents, or both discover this action. Browser extension
    /// manifests include human-visible actions; OpenAI and MCP schemas include
    /// agent-visible actions. Defaults to [`ActionAudience::Both`].
    pub audience: ActionAudience,
    /// Ask vision-capable backends to append the current
    /// [`SurfaceState::snapshot_png`] after this action succeeds. Explicit
    /// metadata avoids treating every query as visual: `read_board` and
    /// Colab's status queries remain text-only while `read_canvas` opts in.
    pub include_state_snapshot: bool,
    /// Called once per tool invocation with the call's JSON arguments;
    /// returns the [`Effect`] the runtime should dispatch. `Fn`, not
    /// `FnOnce` — a tool is called many times over a session.
    ///
    /// Typically captures its own concrete state (e.g. `Arc<CanvasState>`)
    /// from the `Surface` that built it, rather than relying on the `&dyn
    /// SurfaceState` a `Query`/`Mutate` closure is handed at dispatch time:
    /// `SurfaceState` deliberately exposes no mutation methods (it only
    /// describes the *read* side of state — `describe`/`snapshot_png`/
    /// `snapshot`/`resolve`), so a `Mutate` closure's real mutation path is
    /// its own captured interior-mutability handle (a `Mutex`/CRDT doc),
    /// same as teaching-canvas's `apply_*` fns close over `&Arc<AppState>`
    /// today (`main.rs:417`). Composes cleanly with `Effect`: `Effect` grew
    /// a `Query` variant and reply-carrying `Mutate`/`Commit` (see `Effect`'s
    /// docs) to express `apply_tool`'s three real reply shapes — a pure
    /// query (`read_canvas`), a mutate-then-summarize (`diagram`), and a
    /// plain mutate with a generic ack (`plot_points`/`clear`/…) — but
    /// `ToolDef.apply`'s own signature didn't need to change to carry that;
    /// see the tests below for all three patterns proven end to end.
    pub apply: Arc<dyn Fn(&serde_json::Value) -> Effect + Send + Sync>,
}

impl ActionDef {
    /// Ergonomic constructor so a `Surface` impl can write its tool
    /// vocabulary as a flat list of these, mirroring
    /// `examples/teaching-canvas/src/tools.rs::specs()`'s shape.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        apply: impl Fn(&serde_json::Value) -> Effect + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            audience: ActionAudience::Both,
            include_state_snapshot: false,
            apply: Arc::new(apply),
        }
    }

    pub fn audience(mut self, audience: ActionAudience) -> Self {
        self.audience = audience;
        self
    }

    pub fn human_only(self) -> Self {
        self.audience(ActionAudience::Human)
    }

    pub fn agent_only(self) -> Self {
        self.audience(ActionAudience::Agent)
    }

    /// Include the live surface PNG in this action's model-facing result when
    /// one exists. Text remains the authoritative result and still succeeds
    /// when the state has no renderer.
    pub fn with_state_snapshot(mut self) -> Self {
        self.include_state_snapshot = true;
        self
    }
}

/// Compatibility name retained while applications migrate to the more honest
/// `ActionDef` terminology.
pub type ToolDef = ActionDef;

/// Future returned by a Surface-contributed JSON route.
pub type RouteFuture = Pin<Box<dyn Future<Output = RouteResponse> + Send>>;

/// Handler for a Surface-contributed JSON route.
pub type RouteHandler = Box<dyn Fn(RouteRequest) -> RouteFuture + Send + Sync>;

/// Handler that renders the successful result of an [`ActionRouteDef`].
///
/// It runs only after the shared action dispatcher has accepted and applied
/// the action. Refusals and action failures are runtime-owned HTTP errors, so
/// an extension cannot turn either into a healthy-looking rendered response.
pub type ActionRouteHandler = Box<dyn Fn(ActionRouteRequest) -> RouteFuture + Send + Sync>;

/// One Surface-contributed HTTP route (see [`Surface::routes()`]). Plain
/// JSON in, JSON out — the only shape teaching-canvas's `GET/POST /mission`
/// and Vellum's `GET /artifacts` actually need; no multipart, no SSE, no
/// upgrade. Added in trait-review round 2 (M1, 2026-07-08), Gap 1.
pub struct RouteDef {
    pub method: HttpMethod,
    pub path: &'static str,
    /// Boxed future for the same dyn-compatibility reason as
    /// [`SurfaceState::snapshot_png()`] — see that method's doc comment.
    pub handler: RouteHandler,
}

/// A Surface-contributed mutation route that reuses one existing
/// [`ActionDef`] instead of becoming a second mutation channel.
///
/// The runtime accepts JSON and ordinary
/// `application/x-www-form-urlencoded` bodies. Caller identity comes from
/// runtime-owned transport credentials, not a caller-controlled form field:
/// a valid agent bearer token establishes [`Caller::Agent`], while a
/// host-issued, HttpOnly browser-session cookie establishes
/// [`Caller::Human`]. Missing, invalid, or conflicting credentials establish
/// [`Caller::Unknown`] and fail closed.
///
/// The named action remains the single source of truth for audience, schema,
/// effect, and mutation. `on_success` only renders the state after that action
/// succeeds.
pub struct ActionRouteDef {
    pub method: HttpMethod,
    pub path: &'static str,
    pub action: &'static str,
    pub on_success: ActionRouteHandler,
}

impl ActionRouteDef {
    /// Declare a normal form-compatible `POST` route over `action`.
    pub fn post(
        path: &'static str,
        action: &'static str,
        on_success: impl Fn(ActionRouteRequest) -> RouteFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            method: HttpMethod::Post,
            path,
            action,
            on_success: Box::new(on_success),
        }
    }
}

/// HTTP method for a [`RouteDef`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
}

/// The request half of a [`RouteDef`] handler's signature.
pub struct RouteRequest {
    pub query: std::collections::HashMap<String, String>,
    pub body: Option<serde_json::Value>,
}

/// The request handed to an [`ActionRouteDef`]'s success renderer.
///
/// `body` is the exact argument object already validated and applied by the
/// shared dispatcher. `result` is the action's truthful result text. The
/// caller is host-established and included so a renderer can describe the
/// completed operation without re-parsing identity from untrusted input.
pub struct ActionRouteRequest {
    pub query: std::collections::HashMap<String, String>,
    pub body: serde_json::Value,
    pub caller: Caller,
    pub result: String,
}

/// The response half of a [`RouteDef`] handler's signature.
pub struct RouteResponse {
    pub status: u16,
    pub body: RouteBody,
}

/// What a surface route sends back.
///
/// [`Json`](RouteBody::Json) is the default and serves the browser-module tier:
/// the page is JavaScript, it fetches state, and it renders. That tier is not
/// the only one worth supporting. Below it is an app whose author writes *only
/// Rust* — no build step, no toolchain, no bundle — where the server is the
/// only thing that makes markup. Nothing third-party runs on such a page, so
/// there is nothing on it to sandbox.
///
/// The runtime could not express that tier at all until this existed, which is
/// why the one app in this repository that wanted it had to be written as its
/// own separate server and could only be driven by hand. Both tiers now compose
/// into the same host, over the same actions, under the same audiences.
pub enum RouteBody {
    Json(serde_json::Value),
    /// Served verbatim as `text/html; charset=utf-8`.
    ///
    /// Anything interpolated from outside — a model's text, a filename, a form
    /// field — must go through [`html::escape`] on the way in. The runtime
    /// cannot tell your markup from your data, and will not guess.
    Html(String),
    /// Served verbatim as `text/plain; charset=utf-8`.
    Text(String),
}

impl RouteBody {
    /// The JSON body, if this route returned JSON.
    ///
    /// Deliberately `Option` rather than a coercion: a caller asking a
    /// server-rendered page for its fields wants to know it asked the wrong
    /// question, not to receive an empty object that reads like an answer.
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match self {
            RouteBody::Json(value) => Some(value),
            _ => None,
        }
    }

    /// The rendered markup or text, if this route returned either.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RouteBody::Html(text) | RouteBody::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// So a failed assertion can print what the route actually said, whichever
/// shape it chose.
impl std::fmt::Display for RouteBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteBody::Json(value) => write!(f, "{value}"),
            RouteBody::Html(text) | RouteBody::Text(text) => write!(f, "{text}"),
        }
    }
}

/// Read a JSON body's field directly — `response.body["goal"]` — the way a
/// `serde_json::Value` behaves. A non-JSON body yields `Null` under indexing,
/// exactly as indexing a `Value` that lacks the key does.
impl std::ops::Index<&str> for RouteBody {
    type Output = serde_json::Value;

    fn index(&self, key: &str) -> &Self::Output {
        const NULL: serde_json::Value = serde_json::Value::Null;
        self.as_json().map_or(&NULL, |value| &value[key])
    }
}

impl RouteResponse {
    pub fn json(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            body: RouteBody::Json(body),
        }
    }

    pub fn html(status: u16, markup: impl Into<String>) -> Self {
        Self {
            status,
            body: RouteBody::Html(markup.into()),
        }
    }

    pub fn text(status: u16, text: impl Into<String>) -> Self {
        Self {
            status,
            body: RouteBody::Text(text.into()),
        }
    }
}

/// The resolved provider registry the [`App`] builder takes: built-in ACP +
/// BYOK providers plus any `models.json` presets. M1 Phase 1: real, extracted
/// from teaching-canvas's `providers.rs` (the [`providers::build`] loader) —
/// this struct itself is new glue reconciling that free function with the
/// `App::providers()` seam the A0 scaffold already committed to.
pub struct Providers(pub Vec<providers::Provider>);

impl Providers {
    /// Build the registry: built-ins plus any presets in the `models.json` at
    /// `path`. A missing optional file yields available built-ins; a present
    /// unreadable or malformed file is a startup error so the advertised
    /// provider set cannot silently change — see [`providers::build`].
    pub fn from_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, providers::ProviderConfigError> {
        providers::build(path.as_ref()).map(Self)
    }

    /// Build the registry from a provider config that must exist. Use this for
    /// an explicit configuration override; unlike [`Self::from_file`], a
    /// missing path is a startup error.
    pub fn from_required_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, providers::ProviderConfigError> {
        providers::build_required(path.as_ref()).map(Self)
    }

    /// Keep only ACP coding-agent backends. Visual-editing applications use
    /// this when the agent must modify project files out of band; an in-process
    /// chat-completions model only sees Surface actions and cannot truthfully
    /// perform that filesystem step.
    pub fn acp_only(mut self) -> Self {
        self.0
            .retain(|provider| matches!(provider.backend, providers::Backend::Acp { .. }));
        self
    }

    /// Keep every managed coding-agent backend, regardless of wire protocol.
    /// This is the protocol-neutral choice for applications that need ordinary
    /// repository tools as well as Surface actions.
    pub fn managed_agents_only(mut self) -> Self {
        self.0
            .retain(|provider| provider.backend.is_managed_agent());
        self
    }
}

/// Runtime-owned human-in-the-loop routes to expose for an application.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hitl {
    pub barge_in: bool,
    pub decisions: bool,
    pub focus: bool,
}

/// Errors returned while validating or serving an [`App`].
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A required builder input was not supplied.
    #[error("{0}")]
    MissingConfiguration(&'static str),
    /// A supplied action, extension, mount, or provider setting is invalid.
    #[error("invalid application configuration: {0}")]
    InvalidConfiguration(String),
    /// The listen address could not be parsed.
    #[error("invalid listen address: {0}")]
    InvalidAddress(#[from] std::net::AddrParseError),
    /// The HTTP listener could not be bound.
    #[error("failed to bind HTTP listener: {0}")]
    Bind(#[source] std::io::Error),
    /// The HTTP server stopped with an error.
    #[error("HTTP server failed: {0}")]
    Serve(#[source] std::io::Error),
}

/// Second handles onto the runtime's own WS binary broadcast, SSE custom-
/// event broadcast, and replayable transcript ring buffer, for a Surface's
/// state constructor to hold onto directly — not new channels; the runtime
/// and the Surface deliberately share these, so a Surface's own chrome (draw
/// broadcasts, `canvas.*` events, `describe()`'s recent-narration read)
/// rides the exact same channels `App::serve()` binds `/ws`/`/events` to.
///
/// **Replaces the old `App::transport()` two-step (M2 Phase D → trait-review
/// round 3, M2, 2026-07-08, leak 1).** Before this type existed, a caller had
/// to call `App::transport()` on a bound `App::new()` and build its `Surface`
/// from the result BEFORE calling `.surface(...)` — an ordering invariant
/// enforced only by a doc comment, easy to get backwards with no compiler
/// error (get the order wrong and the Surface holds channels from a
/// different `App` entirely, or none). [`App::surface`] now takes a
/// *factory* closure instead of a built `Surface`, and hands it a `Transport`
/// — the runtime clones its channels, the closure builds the Surface with
/// them, and the ordering constraint is structural (there is no longer a
/// separate call to get wrong) rather than a comment to read.
#[derive(Clone)]
pub struct Transport {
    pub ws_tx: broadcast::Sender<Vec<u8>>,
    pub sse_tx: broadcast::Sender<String>,
    pub history: Arc<Mutex<VecDeque<JsonValue>>>,
    /// Replayable human-turn flag shared with [`runtime_state::RuntimeState`].
    /// Use [`Self::tutor_state`] rather than mutating it directly.
    pub awaiting: Arc<AtomicBool>,
    /// Shared reconnect boundary for tutor-state flag changes plus their SSE
    /// projection. Surface actions must not publish tutor lifecycle outside it.
    pub transcript_replay_lock: Arc<Mutex<()>>,
}

impl Transport {
    /// Broadcast a named SSE custom event to every connected client — the
    /// public seam a **DOM-native** Surface uses to push a live view update
    /// when a tool mutates its state (the `create-ag-ui-app` starter's
    /// `add_note` tool → a `surface.board` event its `web/index.html` renders).
    /// This is the same custom-event wire shape the runtime uses for
    /// `surface.tutor`, exposed so a Surface can speak the same protocol
    /// without reaching into `ag-ui-core`. Assistant text itself uses AG-UI's
    /// standard `TEXT_MESSAGE_*` lifecycle. A
    /// Surface with a wasm renderer syncs pixels through `/ws` instead and
    /// won't need this; a Surface that carries reload state also lists it in
    /// [`SurfaceState::reconnect_events`] so a fresh client gets it on connect.
    pub fn emit(&self, name: impl Into<String>, value: JsonValue) {
        let event = AgUiEvent::<JsonValue>::Custom(CustomEvent {
            base: BaseEvent::default(),
            name: name.into(),
            value,
        });
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = self.sse_tx.send(json);
        }
    }

    /// Emit the authoritative tutor lifecycle event from a Surface-owned
    /// action while keeping reconnect state in sync. This is the supported
    /// seam for `record(await_recall=true)` and similar application effects.
    pub fn tutor_state(&self, state: &str, question: Option<&str>) {
        let _replay_guard = self.transcript_replay_lock.lock();
        match state {
            "awaiting" => self.awaiting.store(true, Ordering::Relaxed),
            "thinking" | "warming" | "failed" => self.awaiting.store(false, Ordering::Relaxed),
            _ => {}
        }
        self.emit(
            "surface.tutor",
            json!({ "state": state, "question": question }),
        );
    }
}

/// The runtime entry point. See spec §5 for the full usage example:
///
/// ```ignore
/// App::new()
///     .surface(|transport| {
///         let state = CanvasState::new(transport, workspace, render);
///         CanvasSurface::new(Arc::new(state))
///     })
///     .providers(Providers::from_file("models.json")?)
///     .auth(auth_store)
///     .prompt(Prompts::new(
///         include_str!("../prompt.md").trim_end(),
///         |text| format!("The learner just set their mission: \"{text}\"..."),
///     ))
///     .static_dir(static_dir)
///     .pkg_dir(pkg_dir)
///     .voice(true)
///     .hitl(Hitl { barge_in: true, decisions: true, focus: true })
///     .serve("127.0.0.1:8090")
///     .await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct App {
    ws_tx: broadcast::Sender<Vec<u8>>,
    sse_tx: broadcast::Sender<String>,
    history: Arc<Mutex<VecDeque<JsonValue>>>,
    awaiting: Arc<AtomicBool>,
    transcript_replay_lock: Arc<Mutex<()>>,
    surface: Option<Box<dyn Surface>>,
    providers: Option<Providers>,
    default_provider: Option<String>,
    auth: Option<Arc<auth::AuthStore>>,
    activity_path: Option<PathBuf>,
    prompt: Option<turn_loop::Prompts>,
    voice: bool,
    hitl: Option<Hitl>,
    mcp_bridge: Option<turn_loop::McpBridge>,
    static_dir: Option<PathBuf>,
    pkg_dir: Option<PathBuf>,
    /// Working directory handed to ACP subprocesses and `session/new`.
    agent_cwd: Option<PathBuf>,
    /// Additional `(prefix, dir)` mounts for trusted public assets beyond
    /// `static_dir`/`pkg_dir` (see [`Self::mount`]).
    mounts: Vec<(String, PathBuf)>,
}

impl App {
    pub fn new() -> Self {
        // Constructed here, not lazily, because `.surface(...)`'s factory
        // closure needs a `Transport` clone of these — see `Transport`'s docs.
        let (ws_tx, _) = broadcast::channel(512);
        let (sse_tx, _) = broadcast::channel(256);
        Self {
            ws_tx,
            sse_tx,
            history: Arc::new(Mutex::new(VecDeque::new())),
            awaiting: Arc::new(AtomicBool::new(false)),
            transcript_replay_lock: Arc::new(Mutex::new(())),
            surface: None,
            providers: None,
            default_provider: None,
            auth: None,
            activity_path: None,
            prompt: None,
            voice: false,
            hitl: None,
            mcp_bridge: None,
            static_dir: None,
            pkg_dir: None,
            agent_cwd: None,
            mounts: Vec::new(),
        }
    }

    /// Your 20%: the use-case-specific `Surface` implementation. Takes a
    /// *factory* — `FnOnce(Transport) -> S` — rather than an already-built
    /// `Surface`, because a Surface's state constructor typically needs a
    /// second handle onto the runtime's own transport channels (see
    /// [`Transport`]'s docs for why, and what this replaced).
    pub fn surface<S: Surface + 'static>(mut self, build: impl FnOnce(Transport) -> S) -> Self {
        let transport = Transport {
            ws_tx: self.ws_tx.clone(),
            sse_tx: self.sse_tx.clone(),
            history: self.history.clone(),
            awaiting: self.awaiting.clone(),
            transcript_replay_lock: self.transcript_replay_lock.clone(),
        };
        self.surface = Some(Box::new(build(transport)));
        self
    }

    /// Override the durable activity journal path. Without this call the host
    /// stores `activity.json` beside its required auth store, so every
    /// application gets restart-safe telemetry rather than process memory.
    pub fn activity_store(mut self, path: impl Into<PathBuf>) -> Self {
        self.activity_path = Some(path.into());
        self
    }

    /// Fallible form of [`Self::surface`], for surfaces whose durable state or
    /// recipe composition must be validated before the server can start.
    pub fn try_surface<S, E>(
        mut self,
        build: impl FnOnce(Transport) -> Result<S, E>,
    ) -> Result<Self, E>
    where
        S: Surface + 'static,
    {
        let transport = Transport {
            ws_tx: self.ws_tx.clone(),
            sse_tx: self.sse_tx.clone(),
            history: self.history.clone(),
            awaiting: self.awaiting.clone(),
            transcript_replay_lock: self.transcript_replay_lock.clone(),
        };
        self.surface = Some(Box::new(build(transport)?));
        Ok(self)
    }

    /// ACP + BYOK + subscription providers, from `models.json` or built up
    /// directly.
    pub fn providers(mut self, providers: Providers) -> Self {
        self.providers = Some(providers);
        self
    }

    /// Provider to select on process start when `AGUI_PROVIDER` (or the legacy
    /// `CANVAS_PROVIDER`) is not set. This is an application policy: a generic
    /// BYOK starter can choose `openai`, while a richer installation may
    /// choose a managed ACP or subscription provider.
    pub fn default_provider(mut self, id: impl Into<String>) -> Self {
        self.default_provider = Some(id.into());
        self
    }

    /// Pi-style per-provider credential store (`auth.json`). Required: opening
    /// it is a filesystem/path decision (`auth.json`'s location) only the
    /// application knows, so it's constructed by the caller and handed in
    /// rather than defaulted here.
    pub fn auth(mut self, auth: Arc<auth::AuthStore>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Your agent's [`turn_loop::Prompts`] — the fixed persona/method prose
    /// prepended to `SurfaceStore::context()` on every (re)prime (see
    /// [`turn_loop::build_context`]), plus (optionally) how to frame a
    /// just-set "mission" as a turn kickoff.
    ///
    /// **Folded `.mission()` into this method in trait-review round 3 (M2,
    /// 2026-07-08), leak 1.** Before this, `.prompt(&str)` and a separate
    /// `.mission(closure)` were two builder calls that had to be paired by
    /// convention — nothing tied them together, and `App::serve()` merged
    /// them back into one `Prompts` internally regardless. Accepting `impl
    /// Into<Prompts>` here means a plain `&str`/`String` still works (via
    /// `From`, mission defaults to passing text through unchanged — see
    /// [`turn_loop::Prompts`]'s `From` impls) for a Surface with no
    /// mission-framing needs (Vellum, Colab today), while a Surface that DOES
    /// need one (teaching-canvas) passes `Prompts::new(system, mission_fn)`
    /// in the same single call, matching `Prompts`'s own field shape instead
    /// of two builder methods a caller had to remember to pair.
    pub fn prompt(mut self, prompt: impl Into<turn_loop::Prompts>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Server-side speak (voice) — the initial on/off; `POST /control`
    /// toggles it live thereafter.
    pub fn voice(mut self, enabled: bool) -> Self {
        self.voice = enabled;
        self
    }

    /// Human-in-the-loop: barge-in, decisions, focus pointer.
    pub fn hitl(mut self, hitl: Hitl) -> Self {
        self.hitl = Some(hitl);
        self
    }

    /// Optional custom stdio MCP fallback for an ACP adapter that does not
    /// advertise HTTP MCP. Current built-in adapters use the runtime-owned
    /// `/mcp` endpoint and require no application bridge code.
    pub fn mcp_bridge(mut self, bridge: turn_loop::McpBridge) -> Self {
        self.mcp_bridge = Some(bridge);
        self
    }

    /// Directory the browser client's static assets (HTML/JS/CSS) are served
    /// from. A path, not a crate-relative default, because `env!(...)` at
    /// THIS crate's compile time would resolve to `ag-ui-surface`'s own
    /// directory, not the application's — the caller must resolve its own
    /// `CARGO_MANIFEST_DIR`-relative path.
    pub fn static_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.static_dir = Some(dir.into());
        self
    }

    /// Directory for extension package assets served at `/pkg`. The active
    /// extension manifest declares the actual JavaScript and WebAssembly file
    /// names and startup validates those exact assets. See
    /// [`Self::static_dir`]'s docs for why this is a caller-resolved path.
    pub fn pkg_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.pkg_dir = Some(dir.into());
        self
    }

    /// Root ACP coding agents in the project they are meant to inspect/edit.
    /// Defaults to the server process's current directory. The path is
    /// canonicalized and required to be a directory before the server binds.
    pub fn agent_cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.agent_cwd = Some(dir.into());
        self
    }

    /// Serve an additional trusted static directory at `prefix`, generalizing
    /// `static_dir`/`pkg_dir` to any number of mounts. Every mount shares the
    /// runtime's privileged control-plane origin, so it must contain only
    /// application-owned public assets. Never mount a project root, user home,
    /// or other user-controlled tree here. Additive and repeatable;
    /// `static_dir`/`pkg_dir` are unaffected.
    pub fn mount(mut self, prefix: &str, dir: PathBuf) -> Self {
        self.mounts.push((prefix.to_string(), dir));
        self
    }

    /// Bind and run. Everything in the builder above this call is provided
    /// by the runtime and shared across every use case (see spec §5):
    /// transport (`/ws`, `/events`), the turn-protocol chrome (`/ask`,
    /// `/mission` glue, `/interrupt`, `/provider`, `/auth`, `/control`,
    /// `/debug/stats`), extension discovery (`/extensions` + the embedded
    /// `/_agui/client.js` loader), shared action dispatch (`/surface/action`, with
    /// `/canvas-tool` retained as a compatibility alias), state read-back
    /// (`/canvas-state`, `/canvas.png`, `/canvas-image`), deixis (`/semantic`),
    /// whatever extra routes the Surface contributes (see
    /// [`Surface::routes`]), and static file serving.
    pub async fn serve(self, addr: &str) -> Result<(), AppError> {
        let surface: Arc<dyn Surface> = Arc::from(self.surface.ok_or(
            AppError::MissingConfiguration("App::surface(...) is required"),
        )?);
        let attention_tx = self.sse_tx.clone();
        let (surface, semantic_targets) = semantic_targets::install(surface, move |snapshot| {
            let event = AgUiEvent::<JsonValue>::Custom(CustomEvent {
                base: BaseEvent::default(),
                name: semantic_targets::EVENT_NAME.to_string(),
                value: serde_json::to_value(snapshot).unwrap_or(JsonValue::Null),
            });
            if let Ok(encoded) = serde_json::to_string(&event) {
                let _ = attention_tx.send(encoded);
            }
        })
        .map_err(AppError::InvalidConfiguration)?;
        let mut providers = self
            .providers
            .ok_or(AppError::MissingConfiguration(
                "App::providers(...) is required",
            ))?
            .0;
        let default_provider = self.default_provider;
        let auth = self
            .auth
            .ok_or(AppError::MissingConfiguration("App::auth(...) is required"))?;
        let activity_path = self
            .activity_path
            .unwrap_or_else(|| auth.default_activity_path());
        let activity = ActivityFeed::open(&activity_path)
            .map_err(|error| AppError::InvalidConfiguration(error.to_string()))?;
        let surface = activity::install(surface, activity.clone());
        let action_schemas = action_schema::ActionSchemas::compile(surface.tools())
            .map_err(|error| AppError::InvalidConfiguration(error.to_string()))?;
        tracing::info!(
            "action schemas: {} validated and compiled",
            surface.tools().len()
        );
        let extensions = Arc::new(ExtensionManifest::from_surface(surface.as_ref()).map_err(
            |error| AppError::InvalidConfiguration(format!("invalid client module: {error}")),
        )?);
        let saved_connections =
            providers::saved_connections(auth.as_ref()).map_err(AppError::InvalidConfiguration)?;
        for connection in saved_connections {
            if providers
                .iter()
                .any(|provider| provider.id == connection.id)
            {
                return Err(AppError::InvalidConfiguration(format!(
                    "saved connection id `{}` collides with a configured provider",
                    connection.id
                )));
            }
            providers.push(connection);
        }
        let prompts = self.prompt.ok_or(AppError::MissingConfiguration(
            "App::prompt(...) is required: your agent's persona/method",
        ))?;
        // Current ACP adapters connect to the runtime-owned HTTP MCP endpoint.
        // A caller-supplied stdio bridge remains only as a compatibility
        // fallback for an adapter that does not advertise HTTP MCP support.
        let bridge = self.mcp_bridge;
        if providers.is_empty() {
            return Err(AppError::InvalidConfiguration(
                "no providers configured".to_string(),
            ));
        }
        let static_dir = self.static_dir.ok_or(AppError::MissingConfiguration(
            "App::static_dir(...) is required",
        ))?;
        // Optional: only a Surface whose `client_modules()` mounts a wasm bundle
        // needs `/pkg`. A DOM-native starter leaves it unset and the mount is
        // simply not added.
        let pkg_dir = self.pkg_dir;
        let mounts = self.mounts;
        let surface_routes = surface.routes();
        let surface_action_routes = surface.action_routes();
        let human_route_cookie_enabled = needs_human_route_cookie(surface.as_ref());
        validate_http_layout(
            &surface_routes,
            &surface_action_routes,
            surface.tools(),
            &mounts,
        )
        .map_err(AppError::InvalidConfiguration)?;
        validate_static_directories(&static_dir, pkg_dir.as_deref(), &mounts)
            .map_err(AppError::InvalidConfiguration)?;
        let configured_agent_cwd = match self.agent_cwd {
            Some(path) => path,
            None => std::env::current_dir().map_err(|error| {
                AppError::InvalidConfiguration(format!(
                    "failed to resolve default agent working directory: {error}"
                ))
            })?,
        };
        if configured_agent_cwd.as_os_str().is_empty() {
            return Err(AppError::InvalidConfiguration(
                "agent working directory must not be empty".into(),
            ));
        }
        let agent_cwd = std::fs::canonicalize(&configured_agent_cwd).map_err(|error| {
            AppError::InvalidConfiguration(format!(
                "agent working directory {} cannot be resolved: {error}",
                configured_agent_cwd.display()
            ))
        })?;
        if !agent_cwd.is_dir() {
            return Err(AppError::InvalidConfiguration(format!(
                "agent working directory {} is not a directory",
                agent_cwd.display()
            )));
        }
        validate_extension_assets(
            extensions.as_ref(),
            &static_dir,
            pkg_dir.as_deref(),
            &mounts,
        )
        .map_err(|error| AppError::InvalidConfiguration(error.to_string()))?;

        let socket_addr = loopback_socket_addr(addr)?;
        let mcp_catalog = Arc::new(mcp::Catalog::from_actions(surface.tools()).map_err(
            |error| AppError::InvalidConfiguration(format!("invalid MCP action catalog: {error}")),
        )?);

        let byok = turn_loop::openai::ByokConfig::from_env_and_auth(auth.as_ref())
            .map_err(|error| AppError::InvalidConfiguration(error.to_string()))?;
        let provider_id = providers::default_id_with(&providers, default_provider.as_deref())
            .map_err(AppError::InvalidConfiguration)?
            .ok_or_else(|| AppError::InvalidConfiguration("no provider can be selected".into()))?;
        let audio_on = self.voice;

        // Bind before constructing RuntimeState so an ephemeral `:0` address
        // records the real kernel-assigned port in the ACP MCP descriptor.
        let listener = tokio::net::TcpListener::bind(socket_addr)
            .await
            .map_err(AppError::Bind)?;
        let bound_addr = listener.local_addr().map_err(AppError::Bind)?;
        let port = bound_addr.port();

        let (rt, channels) = runtime_state::RuntimeState::new_with_shared_tutor_and_activity(
            self.ws_tx,
            self.sse_tx,
            self.history,
            providers,
            auth,
            provider_id.clone(),
            byok,
            audio_on,
            port,
            self.awaiting,
            self.transcript_replay_lock,
            activity,
        );
        rt.activity.record_lifecycle(
            "runtime.started",
            ActivityOutcome::Started,
            None,
            Some(&format!("listening on {bound_addr}")),
        );
        rt.activity.record_provider(
            "provider.selected",
            ActivityOutcome::Info,
            None,
            &provider_id,
        );
        rt.install_compiled_action_schemas(action_schemas)
            .map_err(|error| AppError::InvalidConfiguration(error.to_string()))?;

        // Resolve the initial active endpoint for the starting provider
        // (OpenAI-backed only; ACP providers manage their own credentials).
        let initial_provider = providers::find(&rt.providers.read(), &provider_id).cloned();
        if let Some(p) = initial_provider {
            if matches!(p.backend, providers::Backend::OpenAi) {
                let byok = rt.byok.lock().clone();
                let ov = rt.model_overrides.lock().get(&p.id).cloned();
                let cfg =
                    turn_loop::openai::resolve_openai_config(&rt.auth, &byok, &p, ov.as_deref());
                tracing::info!(
                    "initial endpoint: base_url={}, model={}, auth={}",
                    cfg.base_url,
                    cfg.model,
                    if cfg.auth == turn_loop::openai::AuthMode::Oauth {
                        "oauth"
                    } else {
                        "api_key"
                    }
                );
                *rt.openai.lock() = cfg;
            }
        }

        let prompts = Arc::new(prompts);
        let human_route_token: Arc<str> = Arc::from(uuid::Uuid::new_v4().simple().to_string());

        tracing::info!(
            "extensions: {}",
            extensions
                .extensions
                .iter()
                .map(|extension| format!(
                    "{}@{} ({})",
                    extension.id, extension.version, extension.loading
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let router_state = RouterState {
            rt: rt.clone(),
            surface: surface.clone(),
            semantic_targets,
            extensions,
            mcp_catalog,
            human_route_token: human_route_token.clone(),
            human_route_cookie_enabled,
        };

        let hitl = self.hitl.unwrap_or_default();
        let mut router: Router<RouterState> = Router::new()
            .route("/_agui/client.js", get(agui_client_handler))
            .route(
                "/_agui/provider-settings.js",
                get(provider_settings_handler),
            )
            .route(
                "/_agui/provider-settings.css",
                get(provider_settings_style_handler),
            )
            .route("/_agui/conversation.js", get(conversation_handler))
            .route("/_agui/conversation.css", get(conversation_style_handler))
            .route(
                "/_agui/semantic-targets.js",
                get(semantic_targets_script_handler),
            )
            .route(
                "/_agui/semantic-targets.css",
                get(semantic_targets_style_handler),
            )
            .route("/extensions", get(extensions_handler))
            .route("/activity", get(activity_handler))
            .route("/activity/events", get(activity_events_handler))
            .route("/mcp", get(mcp_get_handler).post(mcp_post_handler))
            .route("/ws", get(ws_handler))
            .route("/events", get(sse_handler))
            .route("/debug/stats", get(stats_handler))
            .route("/control", post(control_handler))
            .route("/ask", post(ask_handler))
            .route("/surface/me", get(whoami_handler).post(join_handler))
            .route("/surface/action", post(tool_handler))
            .route("/canvas-tool", post(tool_handler))
            .route("/provider", get(provider_get).post(provider_post))
            .route("/auth", get(auth_get).post(auth_post))
            .route("/connections", post(connections_post))
            .route("/models/openrouter", get(openrouter_models_get))
            .route("/surface/state", get(state_describe_handler))
            .route("/canvas-state", get(state_describe_handler))
            .route("/canvas.png", get(state_png_handler))
            .route("/canvas-image", get(state_png_b64_handler))
            .route("/semantic-targets", get(semantic_targets_get_handler))
            .route(
                "/semantic-targets/presence",
                post(semantic_targets_presence_handler),
            )
            .route(
                "/semantic-targets/focus",
                post(semantic_targets_focus_handler),
            )
            .route(
                "/semantic-targets/clear",
                post(semantic_targets_clear_handler),
            );

        if hitl.focus {
            router = router.route("/semantic", post(semantic_handler));
        }
        if hitl.barge_in {
            router = router.route("/interrupt", post(interrupt_handler));
        }
        if hitl.decisions {
            router = router.route("/decision", post(decision_handler));
        }

        router = mount_surface_routes(router, surface_routes);
        router = mount_action_routes(router, surface_action_routes);

        // Additional trusted public-asset mounts (`App::mount`), alongside the
        // fixed `/pkg` + fallback `static_dir` below.
        for (prefix, dir) in mounts {
            router = router.nest_service(&prefix, ServeDir::new(dir));
        }

        if let Some(pkg_dir) = pkg_dir {
            router = router.nest_service("/pkg", ServeDir::new(pkg_dir));
        } else {
            // A DOM-only recipe must not accidentally expose a stale generated
            // bundle living under its fallback static directory. `.pkg_dir()`
            // is the capability gate for `/pkg`, not merely a preferred source.
            router = router
                .route("/pkg", get(unavailable_pkg_handler))
                .route("/pkg/{*path}", get(unavailable_pkg_handler));
        }
        let router = router
            .fallback_service(ServeDir::new(static_dir))
            // Dev/demo: never cache static assets — see teaching-canvas's
            // original `main.rs` for the full rationale (a rebuilt WASM
            // bundle or an edited `index.html`/`lesson.js` always lands on
            // the next reload instead of being served stale).
            .layer(SetResponseHeaderLayer::overriding(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store"),
            ))
            .with_state(router_state)
            // Reject DNS-rebinding and cross-origin browser requests before
            // they can reach either runtime routes, Surface routes, or static
            // assets. Native HTTP clients may omit Origin, but every request
            // still has to address the exact loopback listener authority.
            .layer(middleware::from_fn_with_state(
                LoopbackRequestGuard {
                    port,
                    human_route_token,
                    human_route_cookie_enabled,
                },
                loopback_request_guard,
            ));

        tracing::info!("listening on http://{bound_addr}");
        // Bind before starting ACP: `session/new` may connect to `/mcp`
        // immediately after its initialize handshake. The bound listener can
        // queue that connection until `axum::serve` begins polling below.
        let runtime_tasks = RuntimeTaskGuard::new(vec![
            tokio::spawn(narration::narration_worker(channels.narr_rx)),
            tokio::spawn(turn_loop::run_supervisor(
                rt.clone(),
                surface,
                prompts,
                bridge,
                agent_cwd,
                channels.ask_rx,
                channels.mission_rx,
                channels.switch_rx,
            )),
        ]);
        let result = axum::serve(listener, router).await.map_err(AppError::Serve);
        runtime_tasks.shutdown().await;
        result
    }
}

async fn unavailable_pkg_handler() -> StatusCode {
    StatusCode::NOT_FOUND
}

fn loopback_socket_addr(addr: &str) -> Result<SocketAddr, AppError> {
    let socket_addr: SocketAddr = addr.parse()?;
    if socket_addr.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
        return Err(AppError::InvalidConfiguration(format!(
            "listen address {socket_addr} is not 127.0.0.1; browser-facing control and credential routes are loopback-only"
        )));
    }
    Ok(socket_addr)
}

const HUMAN_ROUTE_COOKIE: &str = "agui_human_route_session";

#[derive(Clone)]
struct LoopbackRequestGuard {
    port: u16,
    human_route_token: Arc<str>,
    human_route_cookie_enabled: bool,
}

fn loopback_request_headers_allowed(headers: &HeaderMap, port: u16) -> bool {
    let ipv4_authority = format!("127.0.0.1:{port}");
    let localhost_authority = format!("localhost:{port}");
    let host_allowed = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value == ipv4_authority || value.eq_ignore_ascii_case(&localhost_authority)
        });
    if !host_allowed {
        return false;
    }

    headers
        .get(header::ORIGIN)
        .map(|value| {
            value.to_str().is_ok_and(|value| {
                value == format!("http://{ipv4_authority}")
                    || value.eq_ignore_ascii_case(&format!("http://{localhost_authority}"))
            })
        })
        .unwrap_or(true)
}

async fn loopback_request_guard(
    State(guard): State<LoopbackRequestGuard>,
    request: Request,
    next: Next,
) -> Response {
    if !loopback_request_headers_allowed(request.headers(), guard.port) {
        return (
            StatusCode::FORBIDDEN,
            "untrusted request authority or Origin",
        )
            .into_response();
    }
    let issue_human_route_cookie = human_navigation_request(&request);
    let mut response = next.run(request).await;
    if guard.human_route_cookie_enabled
        && issue_human_route_cookie
        && response.status().is_success()
    {
        let value = format!(
            "{HUMAN_ROUTE_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict",
            guard.human_route_token
        );
        match HeaderValue::from_str(&value) {
            Ok(value) => {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            Err(error) => {
                tracing::error!("failed to encode human route session cookie: {error}");
            }
        }
    }
    response
}

/// A browser session becomes human-capable only after a successful,
/// user-activated document navigation. Fetch Metadata is browser-owned, so
/// ordinary links, address-bar navigation, and reloads qualify without
/// JavaScript or a caller-controlled identity field.
///
/// This is a loopback browser/session boundary, not cryptographic personhood:
/// a malicious native client can forge Fetch Metadata. The credential is
/// nevertheless never given to the provider, is HttpOnly, and prevents an
/// unauthenticated POST from becoming human merely because it reached a path.
fn human_navigation_request(request: &Request) -> bool {
    request.method() == axum::http::Method::GET
        && request
            .headers()
            .get("sec-fetch-mode")
            .and_then(|value| value.to_str().ok())
            == Some("navigate")
        && request
            .headers()
            .get("sec-fetch-dest")
            .and_then(|value| value.to_str().ok())
            == Some("document")
        && request
            .headers()
            .get("sec-fetch-user")
            .and_then(|value| value.to_str().ok())
            == Some("?1")
        && request
            .headers()
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(',').any(|kind| kind.trim() == "text/html"))
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared axum state for every generic runtime route.
#[derive(Clone)]
struct RouterState {
    rt: Arc<runtime_state::RuntimeState>,
    surface: Arc<dyn Surface>,
    semantic_targets: Option<Arc<SemanticTargetService>>,
    extensions: Arc<ExtensionManifest>,
    mcp_catalog: Arc<mcp::Catalog>,
    human_route_token: Arc<str>,
    human_route_cookie_enabled: bool,
}

const RUNTIME_ROUTE_PATHS: &[&str] = &[
    "/_agui/client.js",
    "/_agui/provider-settings.js",
    "/_agui/provider-settings.css",
    "/_agui/conversation.js",
    "/_agui/conversation.css",
    "/_agui/semantic-targets.js",
    "/_agui/semantic-targets.css",
    "/extensions",
    "/activity",
    "/activity/events",
    "/mcp",
    "/ws",
    "/events",
    "/debug/stats",
    "/control",
    "/ask",
    "/surface/action",
    "/canvas-tool",
    "/provider",
    "/auth",
    "/connections",
    "/models/openrouter",
    "/surface/state",
    "/canvas-state",
    "/canvas.png",
    "/canvas-image",
    "/semantic-targets",
    "/semantic-targets/presence",
    "/semantic-targets/focus",
    "/semantic-targets/clear",
    "/semantic",
    "/interrupt",
    "/decision",
    "/pkg",
];

const RUNTIME_ROUTE_PREFIXES: &[&str] = &["/_agui", "/pkg"];

fn valid_literal_http_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && !path.ends_with('/')
        && !path.contains("//")
        && !path.chars().any(|character| {
            character.is_whitespace() || matches!(character, '?' | '#' | '{' | '}' | '*')
        })
        && path.split('/').skip(1).all(|segment| {
            !segment.is_empty()
                    && segment != "."
                    && segment != ".."
                    // Axum 0.8 treats the legacy `:param` spelling as an
                    // invalid route and panics while building the router.
                    // Surface-owned paths are literal, so reject it during
                    // our fallible preflight just like `{param}` and `*rest`.
                    && !segment.starts_with(':')
        })
}

fn path_is_owned_by_mount(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

/// Validate every dynamic route/mount before handing strings to Axum. Its
/// builder APIs panic for malformed or conflicting paths; `App::serve` has a
/// fallible contract, so configuration errors must be returned first.
fn validate_http_layout(
    routes: &[RouteDef],
    action_routes: &[ActionRouteDef],
    actions: &[ToolDef],
    mounts: &[(String, PathBuf)],
) -> Result<(), String> {
    let mut methods = std::collections::HashSet::new();
    for route in routes {
        if !valid_literal_http_path(route.path) {
            return Err(format!("invalid surface route path {:?}", route.path));
        }
        if RUNTIME_ROUTE_PATHS.contains(&route.path)
            || RUNTIME_ROUTE_PREFIXES
                .iter()
                .any(|prefix| path_is_owned_by_mount(route.path, prefix))
        {
            return Err(format!(
                "surface route {:?} conflicts with a runtime-owned path",
                route.path
            ));
        }
        if !methods.insert((route.path, route.method)) {
            return Err(format!(
                "duplicate surface route {:?} {:?}",
                route.method, route.path
            ));
        }
    }
    for route in action_routes {
        if route.method == HttpMethod::Get {
            return Err(format!(
                "action-backed surface route {:?} must use a mutating HTTP method",
                route.path
            ));
        }
        if !actions.iter().any(|action| action.name == route.action) {
            return Err(format!(
                "action-backed surface route {:?} names unknown action {:?}",
                route.path, route.action
            ));
        }
        if !valid_literal_http_path(route.path) {
            return Err(format!(
                "invalid action-backed surface route path {:?}",
                route.path
            ));
        }
        if RUNTIME_ROUTE_PATHS.contains(&route.path)
            || RUNTIME_ROUTE_PREFIXES
                .iter()
                .any(|prefix| path_is_owned_by_mount(route.path, prefix))
        {
            return Err(format!(
                "action-backed surface route {:?} conflicts with a runtime-owned path",
                route.path
            ));
        }
        if !methods.insert((route.path, route.method)) {
            return Err(format!(
                "duplicate surface route {:?} {:?}",
                route.method, route.path
            ));
        }
    }

    let mut prefixes: Vec<&str> = Vec::with_capacity(mounts.len());
    for (prefix, _) in mounts {
        if !valid_literal_http_path(prefix) {
            return Err(format!("invalid static mount prefix {prefix:?}"));
        }
        if RUNTIME_ROUTE_PATHS
            .iter()
            .any(|path| path_is_owned_by_mount(path, prefix))
            || RUNTIME_ROUTE_PREFIXES
                .iter()
                .any(|runtime_prefix| path_is_owned_by_mount(prefix, runtime_prefix))
        {
            return Err(format!(
                "static mount prefix {prefix:?} conflicts with a runtime-owned path"
            ));
        }
        if prefixes.iter().any(|existing| {
            path_is_owned_by_mount(prefix, existing) || path_is_owned_by_mount(existing, prefix)
        }) {
            return Err(format!(
                "static mount prefix {prefix:?} overlaps another static mount"
            ));
        }
        if routes
            .iter()
            .any(|route| path_is_owned_by_mount(route.path, prefix))
            || action_routes
                .iter()
                .any(|route| path_is_owned_by_mount(route.path, prefix))
        {
            return Err(format!(
                "static mount prefix {prefix:?} overlaps a surface route"
            ));
        }
        prefixes.push(prefix.as_str());
    }
    Ok(())
}

/// Fail before binding if a configured static root cannot actually be served.
/// `ServeDir` otherwise turns a typo or non-directory into a healthy-looking
/// server that answers every asset request with 404.
fn validate_static_directories(
    static_dir: &Path,
    pkg_dir: Option<&Path>,
    mounts: &[(String, PathBuf)],
) -> Result<(), String> {
    let validate = |label: &str, path: &Path| {
        if path.as_os_str().is_empty() {
            return Err(format!("{label} must not be empty"));
        }
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("{label} {} cannot be read: {error}", path.display()))?;
        if !metadata.is_dir() {
            return Err(format!("{label} {} is not a directory", path.display()));
        }
        Ok(())
    };
    let validate_file = |label: &str, path: &Path| {
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("{label} {} cannot be read: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{label} {} is not a regular file", path.display()));
        }
        std::fs::File::open(path)
            .map_err(|error| format!("{label} {} cannot be opened: {error}", path.display()))?;
        Ok(())
    };

    validate("static directory", static_dir)?;
    validate_file("static entry", &static_dir.join("index.html"))?;
    if let Some(pkg_dir) = pkg_dir {
        validate("package directory", pkg_dir)?;
    }
    for (prefix, path) in mounts {
        validate(&format!("static mount {prefix:?}"), path)?;
    }
    Ok(())
}

/// Mount whatever extra HTTP routes a Surface contributes beyond the
/// tool-call vocabulary (see [`Surface::routes`]; teaching-canvas: `GET`/
/// `POST /mission`). Grouped by path first — axum panics if `.route()` is
/// called twice for the same literal path, so every method for a given path
/// must be folded into ONE `MethodRouter` before mounting.
///
/// `POST /mission` gets one small piece of extra router glue ON TOP of the
/// Surface's own persistence handler — ratified (M2 Phase D): a `RouteDef`
/// handler has no channel access (deliberately — see its doc comment), so it
/// can persist a mission but can't itself kick the turn that begins lesson
/// one. The runtime calls the Surface's handler for persistence, THEN sends
/// to `rt.mission_tx`. Deliberately special-cased to this one path, not a
/// generic "routes can trigger turns" mechanism — only teaching-canvas needs
/// it today. Vellum and Colab use ordinary Surface routes without turn
/// kickoff, so this remains an intentionally narrow mission contract.
fn mount_surface_routes(router: Router<RouterState>, routes: Vec<RouteDef>) -> Router<RouterState> {
    let mut by_path: HashMap<&'static str, axum::routing::MethodRouter<RouterState>> =
        HashMap::new();
    for route in routes {
        let path = route.path;
        let method = route.method;
        let is_mission_post = path == "/mission" && method == HttpMethod::Post;
        let route = Arc::new(route);
        let mr = by_path.remove(path).unwrap_or_default();
        let mr = if is_mission_post {
            mr.post(
                move |State(st): State<RouterState>, Json(body): Json<JsonValue>| {
                    let route = route.clone();
                    async move { mission_post_glue(st, route, body).await }
                },
            )
        } else {
            let handler = move |Query(query): Query<HashMap<String, String>>,
                                headers: HeaderMap,
                                body: axum::body::Bytes| {
                let route = route.clone();
                async move { passthrough_route(route, query, headers, body).await }
            };
            match method {
                HttpMethod::Get => mr.get(handler),
                HttpMethod::Post => mr.post(handler),
                HttpMethod::Put => mr.put(handler),
                HttpMethod::Delete => mr.delete(handler),
            }
        };
        by_path.insert(path, mr);
    }
    let mut router = router;
    for (path, mr) in by_path {
        router = router.route(path, mr);
    }
    router
}

/// Mount HTML/form mutation adapters over the same typed actions used by MCP,
/// in-process providers, and `/surface/action`. The route contributes only a
/// success renderer; caller, audience, schema, and mutation remain
/// runtime-owned.
fn mount_action_routes(
    router: Router<RouterState>,
    routes: Vec<ActionRouteDef>,
) -> Router<RouterState> {
    let mut by_path: HashMap<&'static str, axum::routing::MethodRouter<RouterState>> =
        HashMap::new();
    for route in routes {
        let path = route.path;
        let method = route.method;
        let route = Arc::new(route);
        let handler = move |State(st): State<RouterState>,
                            Query(query): Query<HashMap<String, String>>,
                            headers: HeaderMap,
                            body: axum::body::Bytes| {
            let route = route.clone();
            async move { action_route_request(st, route, query, headers, body).await }
        };
        let mr = by_path.remove(path).unwrap_or_default();
        let mr = match method {
            HttpMethod::Get => mr.get(handler),
            HttpMethod::Post => mr.post(handler),
            HttpMethod::Put => mr.put(handler),
            HttpMethod::Delete => mr.delete(handler),
        };
        by_path.insert(path, mr);
    }
    let mut router = router;
    for (path, mr) in by_path {
        router = router.route(path, mr);
    }
    router
}

fn action_route_caller(headers: &HeaderMap, st: &RouterState) -> Caller {
    let authorization_present = headers.contains_key(header::AUTHORIZATION);
    let agent = mcp::authorized(headers, &st.rt.mcp_token);
    let human = cookie_matches(headers, HUMAN_ROUTE_COOKIE, st.human_route_token.as_ref());

    match (authorization_present, agent, human) {
        (true, true, false) => Caller::Agent,
        (false, false, true) => Caller::Human,
        // Invalid bearer credentials and two simultaneous identities are both
        // ambiguous. Neither may silently fall through to the human audience.
        _ => Caller::Unknown,
    }
}

/// Apply the request body's optional role only after the transport has
/// established the caller. A human browser may deliberately use the lower
/// agent authority, but an agent credential can never claim the person.
fn narrow_action_route_caller(established: Caller, requested: Option<&JsonValue>) -> Caller {
    let Some(requested) = requested else {
        return established;
    };
    let Some(requested) = requested.as_str() else {
        return Caller::Unknown;
    };
    let requested = requested.trim().to_ascii_lowercase();

    match (established, requested.as_str()) {
        (Caller::Human, "human") => Caller::Human,
        (Caller::Human | Caller::Agent, "agent") => Caller::Agent,
        (Caller::Human | Caller::Agent | Caller::Companion, "companion") => Caller::Companion,
        (Caller::Companion, "agent") => Caller::Agent,
        _ => Caller::Unknown,
    }
}

/// A browser needs its host-issued human credential whenever it can discover
/// a human action. This includes DOM extensions that call `/surface/action`
/// directly and have no server-rendered [`ActionRouteDef`] adapters.
fn needs_human_route_cookie(surface: &dyn Surface) -> bool {
    surface.tools().iter().any(|action| action.audience.human())
}

fn cookie_matches(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|field| field.trim().split_once('='))
        .any(|(candidate, value)| candidate == name && value == expected)
}

async fn action_route_request(
    st: RouterState,
    route: Arc<ActionRouteDef>,
    query: HashMap<String, String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Establish identity and audience before body decoding. A caller that may
    // not use this action must not have its arguments inspected.
    let caller = action_route_caller(&headers, &st);
    if caller == Caller::Unknown {
        st.rt.activity.record_action(
            Caller::Unknown,
            route.action,
            ActivityOutcome::Failed,
            st.rt.current_activity_run.lock().as_deref(),
            Vec::new(),
            Vec::new(),
            Some("caller identity was not established; no action was dispatched"),
        );
        st.rt.activity.record_failure(
            "action.identity_unestablished",
            Some(Caller::Unknown),
            st.rt.current_activity_run.lock().as_deref(),
            "caller identity was not established; no action was dispatched",
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "ok": false,
                "result": "caller identity was not established; no action was dispatched"
            })),
        )
            .into_response();
    }
    let Some(action) = st
        .surface
        .tools()
        .iter()
        .find(|action| action.name == route.action)
    else {
        tracing::error!(
            path = route.path,
            action = route.action,
            "validated action-backed route lost its action"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "result": "action-backed route is not available"
            })),
        )
            .into_response();
    };
    if !caller.may_call(action.audience) {
        let (result, _, _) = turn_loop::dispatch_tool(
            &st.rt,
            st.surface.as_ref(),
            caller,
            route.action,
            &JsonValue::Null,
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "ok": false, "result": result })),
        )
            .into_response();
    }

    let body = match decode_route_body(&headers, &body) {
        Ok(body) => body.unwrap_or_else(|| json!({})),
        Err(response) => return response,
    };
    let actor = Actor::anonymous(caller);
    st.surface.note_caller(&actor);
    let (result, _, ok) = with_actor(
        actor,
        turn_loop::dispatch_tool(&st.rt, st.surface.as_ref(), caller, route.action, &body),
    )
    .await;
    if !ok {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "result": result })),
        )
            .into_response();
    }

    let response = (route.on_success)(ActionRouteRequest {
        query,
        body,
        caller,
        result,
    })
    .await;
    surface_route_response(route.path, response)
}

/// A pure pass-through to `route.handler`: the request's query pairs and a
/// JSON body when the request has one.
///
/// The query string used to be dropped here — `RouteRequest::query` was
/// documented as a real field but always arrived empty, so the first `GET`
/// route that needed a parameter (same-page-atlas's `/atlas/source?path=…`)
/// silently read `""`. Repeated keys keep the last value, matching the
/// `HashMap` the field has always been.
async fn passthrough_route(
    route: Arc<RouteDef>,
    query: HashMap<String, String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let body_json = match decode_route_body(&headers, &body) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let resp = (route.handler)(RouteRequest {
        query,
        body: body_json,
    })
    .await;
    surface_route_response(route.path, resp)
}

// Axum early-return helper; the `Err` is the response, and boxing it would obscure that.
#[allow(clippy::result_large_err)]
fn decode_route_body(
    headers: &HeaderMap,
    body: &axum::body::Bytes,
) -> Result<Option<JsonValue>, Response> {
    let form_encoded = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(';').next().is_some_and(|kind| {
                kind.trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")
            })
        });

    if body.is_empty() {
        return Ok(None);
    }
    if form_encoded {
        // A server-rendered surface puts real <form> elements on the page, and
        // a real form posts form-encoded. Decoding it here keeps both route
        // request shapes independent of the wire encoding. Repeated keys keep
        // the last value, matching `query`.
        let fields: serde_json::Map<String, JsonValue> = form_urlencoded::parse(body)
            .map(|(key, value)| (key.into_owned(), JsonValue::String(value.into_owned())))
            .collect();
        Ok(Some(JsonValue::Object(fields)))
    } else {
        serde_json::from_slice(body).map(Some).map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": format!("invalid JSON body: {error}") })),
            )
                .into_response()
        })
    }
}

/// The one place a surface's chosen body shape becomes an HTTP response.
fn surface_route_response(path: &str, resp: RouteResponse) -> Response {
    let Some(status) = surface_status(path, resp.status) else {
        return invalid_surface_status_response();
    };
    match resp.body {
        RouteBody::Json(value) => (status, Json(value)).into_response(),
        RouteBody::Html(markup) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            markup,
        )
            .into_response(),
        RouteBody::Text(text) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            text,
        )
            .into_response(),
    }
}

fn surface_status(path: &str, status: u16) -> Option<StatusCode> {
    if (100..=599).contains(&status) {
        if let Ok(status) = StatusCode::from_u16(status) {
            return Some(status);
        }
    }
    tracing::error!(
        path,
        status,
        "surface route returned an invalid HTTP status"
    );
    None
}

fn invalid_surface_status_response() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "ok": false, "error": "surface route returned an invalid HTTP status" })),
    )
        .into_response()
}

/// `POST /mission`'s router glue: call the Surface's own handler to persist
/// the mission (same busy/ready guarding `main.rs`'s original `mission_post`
/// had), then — only on success — hand the text to `rt.mission_tx` so the
/// live turn loop acknowledges it and begins lesson one.
async fn mission_post_glue(st: RouterState, route: Arc<RouteDef>, body: JsonValue) -> Response {
    let rt = &st.rt;
    let _turn_guard = rt.turn_accept_lock.lock().await;
    if !rt.ready.load(Ordering::Relaxed) {
        return (
            StatusCode::OK,
            Json(json!({ "ok": false, "error": "tutor still warming up" })),
        )
            .into_response();
    }
    if rt.busy.load(Ordering::SeqCst) {
        return (StatusCode::OK, Json(json!({ "ok": false, "busy": true }))).into_response();
    }
    let resp = (route.handler)(RouteRequest {
        query: HashMap::new(),
        body: Some(body.clone()),
    })
    .await;
    if resp.status != 200 {
        return surface_route_response(route.path, resp);
    }
    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let turn = runtime_state::TurnRequest {
        text: text.clone(),
        cancel: rt.interrupt_tx.subscribe(),
    };
    narration::begin_human_turn(
        rt,
        "you",
        &text,
        Some(("surface.mission", json!({ "text": text }))),
        None,
    );
    if rt.mission_tx.send(turn).is_err() {
        narration::fail_turn(rt, "tutor unavailable");
        return (
            StatusCode::OK,
            Json(json!({ "ok": false, "error": "tutor unavailable" })),
        )
            .into_response();
    }
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

// ── generic runtime routes ─────────────────────────────────────────────────
// Everything below is turn-protocol/transport chrome any Surface gets "for
// free" (spec §5) — lifted, behavior-preserving, from teaching-canvas's
// `main.rs` (M2 Phase D), reading `Surface::state()`/`Surface::tools()`
// instead of a canvas-specific `AppState`.

/// The shared, embedded browser protocol client and extension loader. Keeping
/// this in the runtime means app pages contain layout/chrome only; they do not
/// fork the transport, action, or manifest security logic.
async fn agui_client_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        AGUI_CLIENT_JS,
    )
}

async fn provider_settings_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        PROVIDER_SETTINGS_JS,
    )
}

async fn provider_settings_style_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        PROVIDER_SETTINGS_CSS,
    )
}

/// The conversation chrome, served next to `<agui-provider-settings>`. An app
/// that wants a transcript and composer writes one tag instead of a fifth copy
/// of the same protocol client.
async fn conversation_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        CONVERSATION_JS,
    )
}

async fn conversation_style_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        CONVERSATION_CSS,
    )
}

async fn semantic_targets_script_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        semantic_targets::BROWSER_JS,
    )
}

async fn semantic_targets_style_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        semantic_targets::BROWSER_CSS,
    )
}

/// Active extension allowlist generated from the Surface's validated
/// [`ClientModule`] plus its executable action vocabulary.
async fn extensions_handler(State(st): State<RouterState>) -> Json<ExtensionManifest> {
    Json(st.extensions.as_ref().clone())
}

/// This runtime does not initiate server-to-client MCP messages, so it uses
/// the stateless Streamable HTTP form and declines a standalone SSE stream.
async fn mcp_get_handler(State(st): State<RouterState>, headers: HeaderMap) -> Response {
    if !mcp::origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "untrusted MCP Origin").into_response();
    }
    if !mcp::authorized(&headers, &st.rt.mcp_token) {
        return (StatusCode::UNAUTHORIZED, "invalid MCP bearer token").into_response();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn mcp_post_handler(
    State(st): State<RouterState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !mcp::origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "untrusted MCP Origin").into_response();
    }
    if !mcp::authorized(&headers, &st.rt.mcp_token) {
        return (StatusCode::UNAUTHORIZED, "invalid MCP bearer token").into_response();
    }
    let message = match serde_json::from_slice::<JsonValue>(&body) {
        Ok(message) => message,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": JsonValue::Null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") }
                })),
            )
                .into_response();
        }
    };
    if let Err(error) = mcp::protocol_version_allowed(&headers, &message, st.rt.as_ref()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "jsonrpc": "2.0",
                "id": message.get("id").cloned().unwrap_or(JsonValue::Null),
                "error": { "code": -32600, "message": error }
            })),
        )
            .into_response();
    }
    // An `initialize` carrying `clientInfo` proposes an attachment. Admission
    // happens only after the handler returns a successful initialize result,
    // so malformed initialize requests cannot leave a participant behind.
    let attaching = mcp::client_identity(&message);
    // Any authenticated call from an already-attached session is evidence it
    // is still here, which is what keeps its presence lease alive.
    let acting = match mcp::session_id(&headers) {
        Some(session) if attaching.is_none() => {
            refresh_mcp_presence(&st, &session);
            st.rt.mcp_agents.lock().get(&session).cloned()
        }
        _ => None,
    };
    // Attribution for anything this call writes. Nothing on the MCP path noted
    // a caller before, which is why every model's writes signed the same
    // generic byline no matter which agent made them. Authorization is
    // unchanged — it is still `Caller::may_call` inside dispatch.
    let actor = match &acting {
        Some(agent) => Actor::attached(Caller::Agent, agent),
        None => Actor::anonymous(Caller::Agent),
    };
    st.surface.note_caller(&actor);
    match with_actor(
        actor,
        mcp::handle(
            st.mcp_catalog.as_ref(),
            st.rt.as_ref(),
            st.surface.as_ref(),
            message,
        ),
    )
    .await
    {
        mcp::Reply::Accepted => StatusCode::ACCEPTED.into_response(),
        mcp::Reply::Json(mut value) => {
            let Some(identity) = attaching else {
                return Json(value).into_response();
            };
            let Some(result) = value.get_mut("result").and_then(JsonValue::as_object_mut) else {
                return Json(value).into_response();
            };
            let agent = attach_mcp_agent(&st, identity);
            // Hand the identity back. An agent that cannot learn the name it
            // was given cannot refer to itself, and cannot tell whether it was
            // renamed to avoid a collision with someone already in the room.
            result.insert(
                "_meta".to_string(),
                json!({
                    "ag-ui-surface/participant": {
                        "id": agent.participant_id,
                        "label": agent.label,
                        "session": agent.session,
                    }
                }),
            );
            let mut response = Json(value).into_response();
            if let Ok(header_value) = HeaderValue::from_str(&agent.session) {
                response.headers_mut().insert(
                    axum::http::HeaderName::from_static(mcp::SESSION_ID_HEADER),
                    header_value,
                );
            }
            response
        }
    }
}

/// Admit an attaching MCP client as a named participant in the room.
///
/// The participant id is minted **here**, never taken from the client: a
/// caller that could choose its own id could take over one already present.
/// The label is only a display name, and it is disambiguated against everyone
/// already in the room so that two clients both calling themselves `claude`
/// cannot collapse into one byline.
fn attach_mcp_agent(st: &RouterState, identity: mcp::ClientIdentity) -> AttachedAgent {
    let session = uuid::Uuid::new_v4().simple().to_string();
    let participant_id = format!("agent-{session}");
    let proposed = identity
        .title
        .clone()
        .unwrap_or_else(|| identity.name.clone());
    // Presence must be read before taking the registry lock because publishing
    // presence may call back into host state. The registry lock then spans
    // name selection and insertion, so two concurrent attaches cannot both
    // claim the same label.
    let presence_labels: Vec<String> = st
        .semantic_targets
        .as_deref()
        .map(|service| {
            service
                .snapshot()
                .participants
                .into_iter()
                .map(|awareness| awareness.participant.label)
                .collect()
        })
        .unwrap_or_default();
    let mut attached_agents = st.rt.mcp_agents.lock();
    let label = unique_participant_label(&proposed, &presence_labels, &attached_agents);
    let agent = AttachedAgent {
        session: session.clone(),
        participant_id: participant_id.clone(),
        label: label.clone(),
        client_name: identity.name,
        client_version: identity.version,
        // Nothing on the MCP handshake carries this yet, so an agent attaching
        // to an open room is nobody's. The meetings tier is where this becomes
        // required rather than known, and where an attach without it is refused.
        responsible: None,
    };
    attached_agents.insert(session, agent.clone());
    drop(attached_agents);
    if let Some(service) = st.semantic_targets.as_deref() {
        // Presence is descriptive context, not authority, so a refused lease
        // must not fail the attach — the agent is admitted either way.
        if let Err(error) = service.present(Participant::agent(&participant_id, &label)) {
            tracing::warn!(%error, "could not register agent presence");
        }
    }
    tracing::info!(
        participant = %participant_id,
        label = %label,
        client_name = %agent.client_name,
        client_version = ?agent.client_version,
        "agent attached over /mcp"
    );
    agent
}

/// A display name nobody else in the room is already using.
fn unique_participant_label(
    proposed: &str,
    presence_labels: &[String],
    attached_agents: &HashMap<String, AttachedAgent>,
) -> String {
    // Both sources matter: presence covers whoever is live in the room, and the
    // attached-agent registry covers a session whose lease has lapsed but which
    // is still holding its name.
    let mut taken = presence_labels.to_vec();
    taken.extend(attached_agents.values().map(|agent| agent.label.clone()));
    unique_label(proposed, &taken)
}

/// The resume token a person's browser holds. `HttpOnly`: nothing in the page
/// needs to read it, and a token JavaScript can read is a token an injected
/// script can steal and then write under someone else's name.
const PERSON_COOKIE: &str = "agui_person";

/// Every display name currently spoken for, by a person or by an agent.
///
/// Both sources matter for the same reason `unique_participant_label` reads
/// both: a room where a person and an agent can both be called "sam" cannot
/// answer who said something, which is the one thing it is for.
fn names_in_use(st: &RouterState) -> Vec<String> {
    let mut taken: Vec<String> = st
        .rt
        .mcp_agents
        .lock()
        .values()
        .map(|agent| agent.label.clone())
        .collect();
    taken.extend(
        st.rt
            .people
            .everyone()
            .into_iter()
            .map(|person| person.name),
    );
    taken
}

/// The person behind this request, resolved from the resume token their browser
/// presents.
///
/// Returns nothing when the token is absent or unrecognized rather than minting
/// on the spot. Admission happens at exactly one endpoint, so an action can
/// never quietly create an identity as a side effect of writing something.
fn person_from(st: &RouterState, headers: &HeaderMap) -> Option<identity::Person> {
    let token = cookie_value(headers, PERSON_COOKIE)?;
    st.rt.people.resolve(&token)
}

/// A credential the host has minted for a human browser. The navigation
/// credential proves entry through the local page, while the person token
/// preserves access for any participant the host has already admitted.
fn human_replica_credential_is_valid(st: &RouterState, headers: &HeaderMap) -> bool {
    cookie_matches(headers, HUMAN_ROUTE_COOKIE, st.human_route_token.as_ref())
        || person_from(st, headers).is_some()
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_string())
}

/// `GET /surface/me` — who the browser asking is, if it has been here before.
///
/// Read-only, and deliberately so. Minting here instead would mean every
/// unauthenticated GET created a participant: a health check, a crawler, or a
/// readiness probe would each leave a person in the room who never arrived and
/// will never leave. Joining is a POST because joining is a change.
async fn whoami_handler(State(st): State<RouterState>, headers: HeaderMap) -> impl IntoResponse {
    match person_from(&st, &headers) {
        Some(person) => {
            // Still here, so renew the presence lease the same way an acting
            // agent does. A returning person who is not re-presented would be
            // attributed correctly but shown as absent.
            present_person(&st, &person);
            Json(person_json(&person)).into_response()
        }
        None => Json(json!({ "ok": true, "joined": false })).into_response(),
    }
}

/// `POST /surface/me` — join the room, and optionally choose a display name.
///
/// This is the human counterpart of an agent's `initialize` handshake, and it
/// exists because people never had one. An agent announces itself and gets an
/// identity; a person only ever loaded a page, so the surface had nothing to
/// attribute their writes to and fell back to calling them "you" — which reads
/// correctly to exactly one reader and silently mislabels everyone else.
///
/// Presenting an existing token returns the same participant, so a reload is
/// the same person coming back rather than a second person arriving. That is
/// the whole reason the token exists, and the reason a byline stores a
/// participant id rather than anything connection-shaped.
async fn join_handler(
    State(st): State<RouterState>,
    headers: HeaderMap,
    body: Option<Json<JsonValue>>,
) -> impl IntoResponse {
    let proposed = body
        .as_ref()
        .and_then(|Json(body)| body.get("name"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);

    if let Some(person) = person_from(&st, &headers) {
        let Some(proposed) = proposed else {
            present_person(&st, &person);
            return Json(person_json(&person)).into_response();
        };
        // Renaming reads the room's names first and disambiguates against
        // them, so asking for a name someone already has takes a suffix rather
        // than their identity.
        let taken = names_in_use(&st);
        let token = cookie_value(&headers, PERSON_COOKIE).unwrap_or_default();
        let person = match st.rt.people.rename(&token, &proposed, &taken) {
            Some(_) => st.rt.people.resolve(&token).unwrap_or(person),
            None => person,
        };
        present_person(&st, &person);
        return Json(person_json(&person)).into_response();
    }

    // A local principal, because the open tiers have nothing better and should
    // not pretend to. The meetings tier admits `Principal::Invited` here
    // instead, from the address the invitation was sent to — same record, same
    // byline, same colour, only a stronger claim behind it.
    let principal = identity::Principal::Local {
        id: uuid::Uuid::new_v4().simple().to_string(),
    };
    let (person, token) = st.rt.people.admit(
        principal,
        proposed.as_deref().unwrap_or("someone"),
        &names_in_use(&st),
    );
    present_person(&st, &person);
    tracing::info!(
        participant = %person.participant_id,
        name = %person.name,
        "person joined"
    );
    let mut response = Json(person_json(&person)).into_response();
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{PERSON_COOKIE}={token}; Path=/; Max-Age=86400; SameSite=Lax; HttpOnly"
    )) {
        response
            .headers_mut()
            .insert(axum::http::header::SET_COOKIE, value);
    }
    response
}

/// Presence is context, not authority, so a refused lease never fails a join —
/// the person is admitted either way and simply shows as absent.
fn present_person(st: &RouterState, person: &identity::Person) {
    let Some(service) = st.semantic_targets.as_deref() else {
        return;
    };
    if let Err(error) = service.present(Participant::human(&person.participant_id, &person.name)) {
        tracing::warn!(%error, "could not register person presence");
    }
}

/// What a person is told about themselves. The resume token is deliberately
/// absent: the browser proves who it is with the cookie, and a page that also
/// held the token in a variable would be one XSS away from handing it over.
fn person_json(person: &identity::Person) -> JsonValue {
    json!({
        "ok": true,
        "joined": true,
        "id": person.participant_id,
        "name": person.name,
        "hue": person.hue,
        "principal": person.principal.key(),
        "invitable": person.principal.address().is_some(),
    })
}

/// Disambiguate a proposed display name against the names already in use.
fn unique_label(proposed: &str, taken: &[String]) -> String {
    let proposed = proposed.trim();
    let base: String = if proposed.is_empty() {
        "agent".to_string()
    } else {
        proposed.chars().take(100).collect()
    };
    if !taken.iter().any(|label| label == &base) {
        return base;
    }
    for suffix in 2..=99u32 {
        let candidate = format!("{base}-{suffix}");
        if !taken.iter().any(|label| label == &candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", uuid::Uuid::new_v4().simple())
}

/// Renew an attached agent's presence lease because it just acted.
fn refresh_mcp_presence(st: &RouterState, session: &str) {
    let agent = st.rt.mcp_agents.lock().get(session).cloned();
    let (Some(agent), Some(service)) = (agent, st.semantic_targets.as_deref()) else {
        return;
    };
    if let Err(error) = service.present(Participant::agent(&agent.participant_id, &agent.label)) {
        tracing::warn!(
            %error,
            participant = %agent.participant_id,
            "could not refresh agent presence"
        );
    }
}

async fn ask_handler(
    State(st): State<RouterState>,
    headers: HeaderMap,
    Json(body): Json<JsonValue>,
) -> impl IntoResponse {
    let rt = &st.rt;
    // Who actually asked, resolved from their resume token. `by` below is a
    // field the caller fills in and defaults to "you", which every browser
    // sends and every browser then renders as its own message — so a second
    // person's question arrived on the first person's screen looking like
    // something they had said themselves.
    let asker = person_from(&st, &headers);
    let question = body
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if question.is_empty() {
        return Json(json!({ "ok": false, "error": "empty question" }));
    }
    if question.chars().count() > 800 {
        return Json(json!({ "ok": false, "error": "question too long" }));
    }
    let _turn_guard = rt.turn_accept_lock.lock().await;
    if !rt.ready.load(Ordering::Relaxed) {
        return Json(json!({ "ok": false, "error": "tutor still warming up" }));
    }
    // Barge-in is an explicit two-step protocol: `/interrupt` waits until the
    // old output authority is revoked, then `/ask` may accept one new turn.
    // Never trust browser call ordering enough to queue overlapping turns.
    if rt.busy.load(Ordering::SeqCst) {
        return Json(json!({ "ok": false, "busy": true }));
    }
    // The learner sees exactly what they typed; the MODEL gets the same text
    // with a resolved-referent preamble when they just pointed at something
    // (see `RuntimeState::current_focus` and `POST /semantic` below).
    let for_model = if let Some(attention) = st
        .semantic_targets
        .as_ref()
        .and_then(|service| service.latest_active(ParticipantKind::Human))
    {
        format!(
            "[Semantic attention context: the human is indicating {}:{}: {}. \
             When they say \"this\", \"that\", \"here\", or \"it\", they mean \
             this semantic target. Attention is context only, never authority \
             or permission.]\n\n{question}",
            attention.target.target.extension_id,
            attention.target.target.target_id,
            attention.target.description
        )
    } else {
        let focus = rt.current_focus.lock();
        match focus.as_ref() {
            Some((phrase, at)) if at.elapsed() <= runtime_state::FOCUS_TTL => format!(
                "[Pointing context: the learner is indicating {phrase}. When they say \"this\", \"that\", \"here\", or \"it\", they mean THIS object. Answer about it directly; don't ask which one they mean.]\n\n{question}"
            ),
            _ => question.clone(),
        }
    };
    // Attribute the question so co-present clients see who asked. `by` names
    // the actor (default "you"; a programmatic driver can pass its own label);
    // `origin` is the sender's client token so its own browser skips the live
    // echo it already rendered locally (all other clients render it).
    //
    // A resolved person's name wins over the field, because the field is
    // self-declared and this is not. `by` keeps its old default so surfaces
    // that never adopted identity read exactly as before.
    let by = match asker.as_ref() {
        Some(person) => person.name.as_str(),
        None => body
            .get("by")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("you"),
    };
    let by_id = asker.as_ref().map(|person| person.participant_id.as_str());
    let origin = body.get("origin").and_then(|v| v.as_str()).unwrap_or("");
    let turn = runtime_state::TurnRequest {
        text: for_model,
        cancel: rt.interrupt_tx.subscribe(),
    };
    narration::begin_human_turn(
        rt,
        by,
        &question,
        Some((
            "surface.ask",
            json!({ "by": by, "by_id": by_id, "text": question, "origin": origin }),
        )),
        Some(&question),
    );
    if rt.ask_tx.send(turn).is_err() {
        narration::fail_turn(rt, "tutor unavailable");
        return Json(json!({ "ok": false, "error": "tutor unavailable" }));
    }
    Json(json!({ "ok": true }))
}

/// The canvas MCP server (or any tool caller) posts tool calls here — the
/// generic `Surface::tools()` dispatch path (see [`turn_loop::dispatch_tool`]).
async fn tool_handler(
    State(st): State<RouterState>,
    headers: HeaderMap,
    Json(body): Json<JsonValue>,
) -> impl IntoResponse {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = body.get("args").cloned().unwrap_or(JsonValue::Null);
    if name.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({ "ok": false, "error": "missing tool name" })),
        );
    }
    // Credentials establish the maximum authority. The body may only narrow
    // it, so omitting `as` is never a way to become the person.
    let caller = narrow_action_route_caller(action_route_caller(&headers, &st), body.get("as"));
    if caller == Caller::Unknown {
        st.rt.activity.record_action(
            Caller::Unknown,
            name,
            ActivityOutcome::Failed,
            st.rt.current_activity_run.lock().as_deref(),
            Vec::new(),
            Vec::new(),
            Some("unrecognized caller identity; no action was dispatched"),
        );
        st.rt.activity.record_failure(
            "action.identity_unrecognized",
            Some(Caller::Unknown),
            st.rt.current_activity_run.lock().as_deref(),
            "unrecognized caller identity; no action was dispatched",
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "ok": false,
                "result": "unrecognized caller identity; no action was dispatched"
            })),
        );
    }
    // Attribution only. A recognized person changes whose name is on the write
    // and nothing about what the write may do. Authorization stays inside
    // `Caller::may_call` after transport credentials establish the caller.
    let actor = match person_from(&st, &headers) {
        Some(person) if caller == Caller::Human => Actor::person(caller, &person),
        _ => Actor::anonymous(caller),
    };
    st.surface.note_caller(&actor);
    let (result, _was_query, ok) = with_actor(
        actor,
        turn_loop::dispatch_tool(&st.rt, st.surface.as_ref(), caller, name, &args),
    )
    .await;
    // A tool that rejected its own call (`Effect::Reject` — e.g. Vellum's
    // `render_choice_set` validation) answers 422 so the MCP/ACP bridge
    // surfaces it as a retryable tool error instead of a silent success.
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    (status, Json(json!({ "ok": ok, "result": result })))
}

/// The browser posts a human's decision reply here — the resume half of
/// [`Effect::EmitAndAwait`]. Body: `{ id, ... }` where `id` is the decision
/// key the suspended tool call stashed (echoed back from the emitted event);
/// the rest of the object is the raw reply the tool's `on_reply` closure
/// interprets. Looks up + removes the keyed `PendingDecision` and delivers the
/// whole body on its oneshot. Distinct from `/semantic` (fire-and-forget
/// deixis) — this one unblocks a turn. Keyed so Colab's concurrent batch
/// decisions each resolve independently (see [`Effect::EmitAndAwait`]).
async fn decision_handler(State(st): State<RouterState>, Json(body): Json<JsonValue>) -> Response {
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing decision id" })),
        )
            .into_response();
    }
    let (token, on_reply, reply_tx) = {
        let _replay_guard = st.rt.transcript_replay_lock.lock();
        let mut decisions = st.rt.decision.lock();
        let Some(pending) = decisions.get_mut(&id) else {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "ok": false, "error": "no pending decision" })),
            )
                .into_response();
        };
        if let Err(error) = validate_decision_reply(&pending.reply_kind, &body) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "ok": false, "error": error })),
            )
                .into_response();
        }
        let Some(on_reply) = pending.on_reply.take() else {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "ok": false, "error": "decision is already being resolved" })),
            )
                .into_response();
        };
        let Some(reply_tx) = pending.reply_tx.take() else {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "ok": false, "error": "decision receiver is unavailable" })),
            )
                .into_response();
        };
        (pending.token.clone(), on_reply, reply_tx)
    };
    let mut claim_guard = DecisionClaimGuard {
        rt: st.rt.clone(),
        id: id.clone(),
        token: token.clone(),
        armed: true,
    };

    // The route owns the accepted callback after claiming it. Run arbitrary
    // Surface code outside the non-reentrant replay mutex: callbacks may do
    // filesystem work or emit tutor state through the same boundary.
    let result = on_reply(body);
    let ok = result.is_ok();
    let error = result.as_ref().err().cloned();
    let _ = reply_tx.send(result.clone());
    {
        let _replay_guard = st.rt.transcript_replay_lock.lock();
        let mut decisions = st.rt.decision.lock();
        if decisions
            .get(&id)
            .is_some_and(|pending| pending.token == token && pending.on_reply.is_none())
        {
            decisions.remove(&id);
        }
        drop(decisions);
        st.rt
            .broadcast_decision_closed(&id, if ok { "resolved" } else { "failed" });
        claim_guard.armed = false;
    }
    if ok {
        Json(json!({ "ok": true })).into_response()
    } else {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "ok": false,
                "error": error.unwrap_or_else(|| "decision handler failed".to_string())
            })),
        )
            .into_response()
    }
}

/// Fail-closed cleanup for a decision already claimed by its HTTP handler.
/// In particular, a panicking Surface callback cannot leave an unreplayable
/// claimed entry that provider cleanup intentionally skips.
struct DecisionClaimGuard {
    rt: Arc<runtime_state::RuntimeState>,
    id: String,
    token: String,
    armed: bool,
}

impl Drop for DecisionClaimGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _replay_guard = self.rt.transcript_replay_lock.lock();
        let mut decisions = self.rt.decision.lock();
        let claimed = decisions
            .get(&self.id)
            .is_some_and(|pending| pending.token == self.token && pending.on_reply.is_none());
        if claimed {
            decisions.remove(&self.id);
            drop(decisions);
            self.rt.broadcast_decision_closed(&self.id, "aborted");
        }
    }
}

fn validate_decision_reply(reply_kind: &ReplyKind, body: &JsonValue) -> Result<(), String> {
    if !body.is_object() {
        return Err("decision reply must be a JSON object".to_string());
    }
    match reply_kind {
        // Bespoke decisions deliberately delegate their shape to `on_reply`.
        // The route has already required an object carrying a matching id.
        ReplyKind::Any => Ok(()),
        ReplyKind::Approval {
            require_reject_comment,
        } => {
            let status = body
                .get("status")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .unwrap_or("");
            if !matches!(status, "approved" | "rejected") {
                return Err(
                    "approval decision `status` must be `approved` or `rejected`".to_string(),
                );
            }
            let comment = match body.get("comment") {
                Some(comment) => comment
                    .as_str()
                    .ok_or_else(|| "approval decision `comment` must be a string".to_string())?
                    .trim(),
                None => "",
            };
            if status == "rejected" && *require_reject_comment && comment.is_empty() {
                return Err("a rejected decision requires a non-empty `comment`".to_string());
            }
            Ok(())
        }
        ReplyKind::Choice {
            allowed,
            text_required_for,
        } => {
            let chosen = body
                .get("chosen")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .unwrap_or("");
            if chosen.is_empty() {
                return Err("choice decision requires a non-empty string `chosen`".to_string());
            }
            if !allowed.iter().any(|option| option == chosen) {
                return Err(format!(
                    "choice decision selected unknown option {chosen:?}"
                ));
            }
            let text = match body.get("text") {
                Some(text) => text
                    .as_str()
                    .ok_or_else(|| {
                        "choice decision `text` must be a string when present".to_string()
                    })?
                    .trim(),
                None => "",
            };
            if text_required_for.iter().any(|option| option == chosen) && text.is_empty() {
                return Err(format!(
                    "choice decision {chosen:?} requires non-empty human-authored `text`"
                ));
            }
            Ok(())
        }
    }
}

// Axum early-return helper; the `Err` is the response, and boxing it would obscure that.
#[allow(clippy::result_large_err)]
fn semantic_target_service(st: &RouterState) -> Result<&SemanticTargetService, Response> {
    st.semantic_targets.as_deref().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({
                "ok": false,
                "error": "this surface has no registered semantic target namespaces"
            })),
        )
            .into_response()
    })
}

// Axum early-return helper; the `Err` is the response, and boxing it would obscure that.
#[allow(clippy::result_large_err)]
fn browser_participant(body: &JsonValue) -> Result<Participant, Response> {
    let Some(id) = body
        .get("participantId")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing participantId" })),
        )
            .into_response());
    };
    let label = body
        .get("participantLabel")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Human");
    Ok(Participant::human(id, label))
}

async fn semantic_targets_get_handler(State(st): State<RouterState>) -> Response {
    let service = match semantic_target_service(&st) {
        Ok(service) => service,
        Err(response) => return response,
    };
    Json(json!({ "ok": true, "snapshot": service.snapshot() })).into_response()
}

async fn semantic_targets_presence_handler(
    State(st): State<RouterState>,
    Json(body): Json<JsonValue>,
) -> Response {
    let service = match semantic_target_service(&st) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let participant = match browser_participant(&body) {
        Ok(participant) => participant,
        Err(response) => return response,
    };
    match service.present(participant) {
        Ok(snapshot) => Json(json!({ "ok": true, "snapshot": snapshot })).into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "error": error })),
        )
            .into_response(),
    }
}

async fn semantic_targets_focus_handler(
    State(st): State<RouterState>,
    Json(body): Json<JsonValue>,
) -> Response {
    let service = match semantic_target_service(&st) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let participant = match browser_participant(&body) {
        Ok(participant) => participant,
        Err(response) => return response,
    };
    let mode =
        match body.get("mode").and_then(JsonValue::as_str) {
            Some("selection") => AttentionMode::Selection,
            Some("focus") | None => AttentionMode::Focus,
            Some(other) => return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "ok": false,
                    "error": format!("browser attention mode {other:?} must be focus or selection")
                })),
            )
                .into_response(),
        };
    let target = match body
        .get("target")
        .cloned()
        .ok_or_else(|| "missing semantic target".to_string())
        .and_then(|value| {
            serde_json::from_value::<SemanticTargetRef>(value)
                .map_err(|error| format!("invalid semantic target: {error}"))
        }) {
        Ok(target) => target,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": error })),
            )
                .into_response()
        }
    };
    let message = body
        .get("message")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    match service.attend(participant, mode, target, message) {
        Ok(target) => Json(json!({
            "ok": true,
            "target": target,
            "snapshot": service.snapshot()
        }))
        .into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "error": error })),
        )
            .into_response(),
    }
}

async fn semantic_targets_clear_handler(
    State(st): State<RouterState>,
    Json(body): Json<JsonValue>,
) -> Response {
    let service = match semantic_target_service(&st) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let participant = match browser_participant(&body) {
        Ok(participant) => participant,
        Err(response) => return response,
    };
    match service.clear(participant) {
        Ok(snapshot) => Json(json!({ "ok": true, "snapshot": snapshot })).into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "error": error })),
        )
            .into_response(),
    }
}

/// The browser posts pointing/selection events here. A click or a rested
/// hover on an object becomes the learner's live *focus* — what "this"/
/// "that" refers to — resolved via [`Surface::resolve_focus`] and stashed for
/// the next `/ask`. A single Surface's default implementation gates on its
/// [`Surface::focus_events`] vocabulary (teaching-canvas:
/// `canvas.focus`/`canvas.selected`/`canvas.dragged`, emitted by
/// `ag-ui-canvas-web`'s WASM renderer). The runtime no longer hardcodes those
/// literals — trait-review round 3 leak 3 closed in the M5 follow-up
/// (2026-07-10). A CompositeSurface routes the event to its owning extension,
/// so identical object ids in two extension states cannot resolve against the
/// wrong one. Requests a Surface cannot resolve are rejected, not acknowledged
/// and discarded.
async fn semantic_handler(State(st): State<RouterState>, Json(body): Json<JsonValue>) -> Response {
    let rt = &st.rt;
    let Some(name) = body
        .get("name")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing semantic event name" })),
        )
            .into_response();
    };
    let Some(id) = body
        .pointer("/value/id")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "missing semantic focus id" })),
        )
            .into_response();
    };
    match st.surface.resolve_focus(name, id) {
        Ok(Some(phrase)) => {
            tracing::info!("focus: {id} -> {phrase}");
            *rt.current_focus.lock() = Some((phrase, std::time::Instant::now()));
        }
        Ok(None) => {
            let error = format!("semantic event {name:?} did not resolve focus target {id:?}");
            tracing::warn!(%error, "semantic focus was not accepted");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "ok": false, "error": error })),
            )
                .into_response();
        }
        Err(error) => {
            tracing::warn!(%error, "semantic focus resolution failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false, "error": error })),
            )
                .into_response();
        }
    }
    Json(json!({ "ok": true })).into_response()
}

/// Read-back of the live state as plain text — `SurfaceState::describe()`.
async fn state_describe_handler(State(st): State<RouterState>) -> Response {
    match st.surface.state().describe() {
        Ok(description) => description.into_response(),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    }
}

/// The live state rendered to a PNG — `SurfaceState::snapshot_png()`.
async fn state_png_handler(State(st): State<RouterState>) -> impl IntoResponse {
    match st.surface.state().snapshot_png().await {
        Ok(Some(png)) => (
            [(header::CONTENT_TYPE, HeaderValue::from_static("image/png"))],
            png,
        )
            .into_response(),
        Ok(None) => (StatusCode::SERVICE_UNAVAILABLE, "renderer unavailable").into_response(),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    }
}

/// The live state rendered to a PNG, base64-encoded as text for compatibility
/// clients. Runtime-owned MCP reads `snapshot_png()` in process instead.
async fn state_png_b64_handler(State(st): State<RouterState>) -> impl IntoResponse {
    match st.surface.state().snapshot_png().await {
        Ok(Some(png)) => base64::engine::general_purpose::STANDARD
            .encode(&png)
            .into_response(),
        Ok(None) => (StatusCode::SERVICE_UNAVAILABLE, "renderer unavailable").into_response(),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    }
}

/// Barge-in: cancel the turn currently in flight. A no-op when nothing is
/// running (the broadcast just has zero live subscribers).
async fn interrupt_handler(State(st): State<RouterState>) -> Response {
    const INTERRUPT_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);
    let rt = &st.rt;
    let _turn_guard = rt.turn_accept_lock.lock().await;
    if !rt.busy.load(Ordering::SeqCst) {
        return Json(json!({ "ok": true, "interrupted": false, "idle": true })).into_response();
    }
    let woke = rt.interrupt_tx.send(()).unwrap_or(0);
    if woke == 0 {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "ok": false,
                "interrupted": false,
                "error": "no active provider turn accepted the interrupt"
            })),
        )
            .into_response();
    }
    let settled = tokio::time::timeout(INTERRUPT_ACK_TIMEOUT, async {
        while rt.busy.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    if !settled {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({ "ok": false, "interrupted": true, "error": "provider did not acknowledge interrupt" })),
        )
            .into_response();
    }
    if !rt.ready.load(Ordering::SeqCst) {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "ok": false,
                "interrupted": true,
                "error": "provider session failed while acknowledging interrupt"
            })),
        )
            .into_response();
    }
    Json(json!({ "ok": true, "interrupted": true, "idle": true })).into_response()
}

/// List providers + current selection/state (drives the dropdown). Never
/// returns any key — only id/label/auth-note.
async fn provider_get(State(st): State<RouterState>) -> impl IntoResponse {
    let rt = &st.rt;
    let current = rt.provider.lock().clone();
    let error = rt.provider_error.lock().clone();
    let list: Vec<JsonValue> = rt
        .providers
        .read()
        .iter()
        .map(|p| json!({ "id": p.id, "label": p.label, "auth": p.auth_note }))
        .collect();
    Json(json!({
        "current": current,
        "ready": rt.ready.load(Ordering::Relaxed),
        "warming": rt.warming.load(Ordering::Relaxed),
        "error": error,
        "providers": list
    }))
}

/// Switch the active provider (tears down the current agent, primes the new
/// one). For OpenAI-backed providers the active endpoint is resolved from
/// the provider + credential store on the way in.
async fn provider_post(State(st): State<RouterState>, Json(body): Json<JsonValue>) -> Response {
    let rt = &st.rt;
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
    let provider = match providers::find(&rt.providers.read(), id).cloned() {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "unknown provider" })),
            )
                .into_response();
        }
    };
    let _control_guard = rt.provider_control_lock.lock().await;
    if *rt.provider.lock() == provider.id
        && (rt.ready.load(Ordering::Relaxed) || rt.warming.load(Ordering::Relaxed))
    {
        return Json(json!({ "ok": true, "current": provider.id, "unchanged": true }))
            .into_response();
    }
    if let Err(error) = request_provider_switch(rt, &provider.id).await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false, "error": error })),
        )
            .into_response();
    }
    Json(json!({
        "ok": true,
        "current": provider.id,
        "adopted": true,
        "ready": rt.ready.load(Ordering::Relaxed)
    }))
    .into_response()
}

/// Cancel any active turn, queue a supervisor switch/re-prime, and wait until
/// the supervisor has actually torn down the old backend and adopted the
/// requested provider. Queueing alone is not a successful switch.
async fn request_provider_switch(
    rt: &Arc<runtime_state::RuntimeState>,
    provider_id: &str,
) -> Result<u64, String> {
    const ADOPTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
    let generation = rt.provider_generation.load(Ordering::SeqCst);
    rt.ready.store(false, Ordering::Relaxed);
    rt.warming.store(true, Ordering::Relaxed);
    *rt.provider_error.lock() = None;
    narration::tutor_event(rt, "warming", None);
    let _ = rt.interrupt_tx.send(());
    rt.switch_tx
        .send(provider_id.to_string())
        .map_err(|_| "provider supervisor is unavailable".to_string())?;

    tokio::time::timeout(ADOPTION_TIMEOUT, async {
        loop {
            let adopted = rt.provider_generation.load(Ordering::SeqCst) > generation
                && *rt.provider.lock() == provider_id;
            if adopted {
                return rt.provider_generation.load(Ordering::SeqCst);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| format!("provider {provider_id:?} was not adopted within 12 seconds"))
}

/// A snapshot of every provider's auth status for the settings UI. NEVER
/// returns a key.
fn auth_snapshot(rt: &runtime_state::RuntimeState) -> JsonValue {
    let current = rt.provider.lock().clone();
    let byok = rt.byok.lock().clone();
    let overrides = rt.model_overrides.lock().clone();
    let providers: Vec<JsonValue> = rt
        .providers
        .read()
        .iter()
        .map(|p| {
            let status = rt.auth.status(p);
            let editable = matches!(p.backend, providers::Backend::OpenAi)
                && p.auth == providers::AuthKind::ApiKey;
            let (eff_base, eff_model) = if p.id == "openai" {
                (byok.base_url.clone(), byok.model.clone())
            } else {
                let model = overrides
                    .get(&p.id)
                    .cloned()
                    .or_else(|| p.model.clone())
                    .unwrap_or_default();
                (p.base_url.clone().unwrap_or_default(), model)
            };
            let local = turn_loop::openai::OpenAiConfig {
                base_url: eff_base.clone(),
                ..turn_loop::openai::OpenAiConfig::default()
            }
            .is_local_host();
            let (configured, source, detail) = if !status.ready && editable && local {
                (
                    true,
                    "local",
                    Some("local server, no key needed".to_string()),
                )
            } else {
                (status.ready, status.source, status.detail)
            };
            let active = p.id == current;
            let runtime_state = if !active {
                "not-selected"
            } else if rt.provider_error.lock().is_some() {
                "failed"
            } else if rt.ready.load(Ordering::Relaxed) {
                "ready"
            } else if rt.warming.load(Ordering::Relaxed) {
                "warming"
            } else {
                "not-ready"
            };
            let builtin_tools = match &p.backend {
                providers::Backend::PiRpc { builtin_tools, .. } => builtin_tools.clone(),
                _ => Vec::new(),
            };
            json!({
                "id": p.id,
                "label": p.label,
                "kind": status.kind,
                "configured": configured,
                "source": source,
                "detail": detail,
                "note": p.auth_note,
                "adapter": p.backend.adapter(),
                "runtime_state": runtime_state,
                "editable": editable,
                "byok": p.id == "openai",
                "group": if p.backend.is_managed_agent() {
                    "managed"
                } else if providers::is_user_connection(p) {
                    "connection"
                } else {
                    "preset"
                },
                "deletable": providers::is_user_connection(p),
                "vision": p.vision,
                "model": eff_model,
                "base_url": eff_base,
                "builtin_tools": builtin_tools,
            })
        })
        .collect();
    json!({
        "current": current,
        "ready": rt.ready.load(Ordering::Relaxed),
        "warming": rt.warming.load(Ordering::Relaxed),
        "error": rt.provider_error.lock().clone(),
        "byok": { "base_url": byok.base_url, "model": byok.model },
        "providers": providers,
    })
}

/// Search OpenRouter's public model catalog without exposing a provider key.
/// The UI calls this only while editing an OpenRouter connection/preset.
async fn openrouter_models_get(Query(query): Query<HashMap<String, String>>) -> Response {
    let needle = query
        .get("q")
        .map(|value| value.trim().to_lowercase())
        .unwrap_or_default();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let response = match client
        .get("https://openrouter.ai/api/v1/models")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "ok": false, "error": format!("OpenRouter catalog unavailable: {error}") })),
            )
                .into_response();
        }
    };
    if !response.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("OpenRouter catalog returned {}", response.status()) })),
        )
            .into_response();
    }
    let body: JsonValue = match response.json().await {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(
                    json!({ "ok": false, "error": format!("invalid OpenRouter catalog: {error}") }),
                ),
            )
                .into_response();
        }
    };
    let models: Vec<JsonValue> = body
        .get("data")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?;
            let name = model.get("name").and_then(JsonValue::as_str).unwrap_or(id);
            let haystack = format!("{id} {name}").to_lowercase();
            (needle.is_empty() || haystack.contains(&needle)).then(|| {
                json!({
                    "id": id,
                    "name": name,
                    "context_length": model.get("context_length").cloned().unwrap_or(JsonValue::Null),
                    "supported_parameters": model.get("supported_parameters").cloned().unwrap_or_else(|| json!([])),
                })
            })
        })
        .take(80)
        .collect();
    Json(json!({ "ok": true, "models": models })).into_response()
}

/// Create, edit, or delete a persisted OpenAI-compatible connection. Secrets
/// remain in auth.json; the browser receives only the normal auth snapshot.
async fn connections_post(State(st): State<RouterState>, Json(body): Json<JsonValue>) -> Response {
    let rt = &st.rt;
    let action = body
        .get("action")
        .and_then(JsonValue::as_str)
        .unwrap_or("save");
    let _control_guard = rt.provider_control_lock.lock().await;

    if action == "delete" {
        let id = body
            .get("id")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .trim();
        if !providers::valid_connection_id(id) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "invalid connection id" })),
            )
                .into_response();
        }
        if *rt.provider.lock() == id {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "ok": false,
                    "error": "switch to another agent or connection before deleting this one"
                })),
            )
                .into_response();
        }
        if let Err(error) = rt.auth.delete_entry(id) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "ok": false, "error": error.to_string() })),
            )
                .into_response();
        }
        rt.providers.write().retain(|provider| provider.id != id);
        rt.model_overrides.lock().remove(id);
        let mut snapshot = auth_snapshot(rt);
        snapshot["ok"] = json!(true);
        return Json(snapshot).into_response();
    }

    if action != "save" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "unknown connection action" })),
        )
            .into_response();
    }

    let text = |field: &str| {
        body.get(field)
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let requested_id = text("id");
    let id = if requested_id.is_empty() {
        format!(
            "{}{}",
            providers::CONNECTION_ID_PREFIX,
            uuid::Uuid::new_v4().simple()
        )
    } else {
        requested_id
    };
    if !providers::valid_connection_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "invalid connection id" })),
        )
            .into_response();
    }
    let editing = rt
        .providers
        .read()
        .iter()
        .find(|provider| provider.id == id)
        .cloned();
    if editing
        .as_ref()
        .is_some_and(|provider| !providers::is_user_connection(provider))
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "ok": false, "error": "connection id collides with a configured provider" })),
        )
            .into_response();
    }
    let vision = body
        .get("vision")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let connection = match providers::Provider::connection(
        id.clone(),
        text("label"),
        text("base_url"),
        text("model"),
        vision,
    ) {
        Ok(connection) => connection,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": error })),
            )
                .into_response();
        }
    };
    let mut env = std::collections::HashMap::new();
    env.insert(providers::CONNECTION_MARKER.to_string(), "true".to_string());
    env.insert(
        providers::CONNECTION_LABEL.to_string(),
        connection.label.clone(),
    );
    env.insert(
        turn_loop::openai::BASE_URL_SETTING.to_string(),
        connection.base_url.clone().unwrap_or_default(),
    );
    env.insert(
        turn_loop::openai::MODEL_SETTING.to_string(),
        connection.model.clone().unwrap_or_default(),
    );
    env.insert(providers::CONNECTION_VISION.to_string(), vision.to_string());
    let key = text("api_key");
    let secret = if key.is_empty() {
        auth::ApiKeyUpdate::Keep
    } else {
        auth::ApiKeyUpdate::Set(key)
    };
    if let Err(error) = rt.auth.update_api_key_entry(&id, env, secret) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": error.to_string() })),
        )
            .into_response();
    }
    {
        let mut registry = rt.providers.write();
        if let Some(existing) = registry.iter_mut().find(|provider| provider.id == id) {
            *existing = connection.clone();
        } else {
            registry.push(connection.clone());
        }
    }
    rt.model_overrides
        .lock()
        .insert(id.clone(), connection.model.clone().unwrap_or_default());

    let live = *rt.provider.lock() == id;
    if live {
        if let Err(error) = request_provider_switch(rt, &id).await {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false, "persisted": true, "error": error })),
            )
                .into_response();
        }
    }
    let mut snapshot = auth_snapshot(rt);
    snapshot["ok"] = json!(true);
    snapshot["saved"] = json!(id);
    Json(snapshot).into_response()
}

async fn auth_get(State(st): State<RouterState>) -> impl IntoResponse {
    Json(auth_snapshot(&st.rt))
}

/// POST a credential change — paste a key / remove it, plus the BYOK
/// base/model form, unified. Body: `{ provider, api_key?, remove?, base_url?,
/// model? }`. If the changed provider is live, its endpoint is re-resolved
/// and re-primed.
async fn auth_post(State(st): State<RouterState>, Json(body): Json<JsonValue>) -> Response {
    let rt = &st.rt;
    let pid = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let provider = match providers::find(&rt.providers.read(), pid).cloned() {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "unknown provider" })),
            )
                .into_response();
        }
    };
    let field = |k: &str| {
        body.get(k)
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_string())
    };
    let mutates_api_key_settings = body.get("api_key").is_some()
        || body.get("remove").and_then(JsonValue::as_bool) == Some(true)
        || body.get("base_url").is_some()
        || body.get("model").is_some();
    if provider.auth != providers::AuthKind::ApiKey && mutates_api_key_settings {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "error": "this provider does not accept API-key endpoint/model updates"
            })),
        )
            .into_response();
    }
    let _control_guard = rt.provider_control_lock.lock().await;

    let persistence_error = |error: auth::AuthStoreError| {
        tracing::error!(provider = %provider.id, %error, "credential update rejected");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": error.to_string() })),
        )
            .into_response()
    };

    let mut byok_candidate = rt.byok.lock().clone();
    let mut overrides_candidate = rt.model_overrides.lock().clone();
    let mut env = rt.auth.api_key_env(&provider.id);
    if provider.id == "openai" {
        if let Some(v) = field("base_url").filter(|s| !s.is_empty()) {
            byok_candidate.base_url = match turn_loop::openai::canonical_base_url(&v) {
                Ok(base_url) => base_url,
                Err(error) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "ok": false, "error": error })),
                    )
                        .into_response();
                }
            };
        }
        if let Some(v) = field("model").filter(|s| !s.is_empty()) {
            byok_candidate.model = v;
        }
        env.insert(
            turn_loop::openai::BASE_URL_SETTING.to_string(),
            byok_candidate.base_url.clone(),
        );
        env.insert(
            turn_loop::openai::MODEL_SETTING.to_string(),
            byok_candidate.model.clone(),
        );
    } else if matches!(provider.backend, providers::Backend::OpenAi) {
        // A `models.json` preset (OpenRouter, Groq, …): let the ⚙ form swap its
        // model without editing the file. Empty string clears the override
        // (reverts to the preset's shipped model). Persist it with the provider
        // credential so the selected model also survives a restart.
        if let Some(v) = field("model") {
            if v.is_empty() {
                overrides_candidate.remove(&provider.id);
                env.remove(turn_loop::openai::MODEL_SETTING);
            } else {
                overrides_candidate.insert(provider.id.clone(), v.clone());
                env.insert(turn_loop::openai::MODEL_SETTING.to_string(), v);
            }
        }
    }

    let secret = if body
        .get("remove")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        auth::ApiKeyUpdate::Remove
    } else if let Some(k) = field("api_key").filter(|s| !s.is_empty()) {
        auth::ApiKeyUpdate::Set(k)
    } else {
        auth::ApiKeyUpdate::Keep
    };
    if let Err(error) = rt.auth.update_api_key_entry(&provider.id, env, secret) {
        return persistence_error(error);
    }
    *rt.byok.lock() = byok_candidate;
    *rt.model_overrides.lock() = overrides_candidate;

    let live = *rt.provider.lock() == provider.id;
    if live && matches!(provider.backend, providers::Backend::OpenAi) {
        let adopted_generation = match request_provider_switch(rt, &provider.id).await {
            Ok(generation) => generation,
            Err(error) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "ok": false,
                        "persisted": true,
                        "error": error
                    })),
                )
                    .into_response();
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if rt.provider_generation.load(Ordering::SeqCst) >= adopted_generation
                    && rt.ready.load(Ordering::Relaxed)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    let mut snap = auth_snapshot(rt);
    if let Some(obj) = snap.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        obj.insert(
            "reprimed".into(),
            json!(live && rt.ready.load(Ordering::Relaxed)),
        );
        obj.insert("restarted".into(), json!(live));
    }
    Json(snap).into_response()
}

async fn control_handler(State(st): State<RouterState>, Json(body): Json<JsonValue>) -> Response {
    let rt = &st.rt;
    match body.get("action").and_then(|v| v.as_str()) {
        Some("audio") => {
            let Some(on) = body.get("on").and_then(JsonValue::as_bool) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "ok": false, "error": "audio control requires boolean `on`" })),
                )
                    .into_response();
            };
            rt.audio_on.store(on, Ordering::Relaxed);
            if !on {
                narration::hush(rt);
            }
        }
        other => {
            tracing::warn!("unknown control action: {other:?}");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "unsupported control action" })),
            )
                .into_response();
        }
    }
    Json(json!({ "ok": true, "audio": rt.audio_on.load(Ordering::Relaxed) })).into_response()
}

async fn stats_handler(State(st): State<RouterState>) -> impl IntoResponse {
    let rt = &st.rt;
    Json(json!({
        "ws_subscribers": rt.ws_tx.receiver_count(),
        "sse_subscribers": rt.sse_tx.receiver_count(),
        "provider": *rt.provider.lock(),
        "ready": rt.ready.load(Ordering::Relaxed),
        "warming": rt.warming.load(Ordering::Relaxed),
        "provider_error": rt.provider_error.lock().clone(),
        "busy": rt.busy.load(Ordering::Relaxed),
        "audio": rt.audio_on.load(Ordering::Relaxed),
    }))
}

fn serialized_surface_snapshot(surface: &dyn Surface) -> Result<String, String> {
    let snapshot =
        serde_json::to_value(surface.state().snapshot()?).map_err(|error| error.to_string())?;
    serde_json::to_string(&AgUiEvent::<JsonValue>::StateSnapshot(StateSnapshotEvent {
        base: BaseEvent::default(),
        snapshot,
    }))
    .map_err(|error| error.to_string())
}

fn pending_decision_events(rt: &runtime_state::RuntimeState) -> Vec<String> {
    let decisions = rt.decision.lock();
    let mut events = decisions
        .iter()
        .map(|(id, pending)| (id.clone(), pending.event_json.clone()))
        .collect::<Vec<_>>();
    events.sort_by(|left, right| left.0.cmp(&right.0));
    events.into_iter().map(|(_, event)| event).collect()
}

async fn activity_handler(State(st): State<RouterState>) -> Json<ActivitySnapshot> {
    Json(st.rt.activity.snapshot())
}

/// Durable replay followed by live host-stamped activity. The journal is
/// written with no subscribers and is replayed after a browser or diagnostics
/// process reconnects, so an open tab is never the collection boundary.
async fn activity_events_handler(
    State(st): State<RouterState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let after = query
        .get("after")
        .and_then(|value| value.parse::<u64>().ok());
    let subscription = st.rt.activity.subscribe(after);
    let replay = futures_util::stream::iter(subscription.replay);
    let live = tokio_stream::wrappers::BroadcastStream::new(subscription.receiver).scan(
        (),
        |(), item| async move {
            match item {
                Ok(event) => Some(event),
                Err(error) => {
                    tracing::warn!(%error, "activity subscriber lagged; closing for durable replay");
                    None
                }
            }
        },
    );
    let stream = replay.chain(live).filter_map(|event| async move {
        match serde_json::to_string(&event) {
            Ok(encoded) => Some(Ok::<_, std::convert::Infallible>(
                SseEvent::default()
                    .event(activity::EVENT_NAME)
                    .data(encoded),
            )),
            Err(error) => {
                tracing::error!(%error, "failed to serialize activity event");
                None
            }
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

struct ActivityConnectionGuard {
    activity: Arc<ActivityFeed>,
    kind: &'static str,
}

impl Drop for ActivityConnectionGuard {
    fn drop(&mut self) {
        self.activity.record_connection(
            self.kind,
            ActivityOutcome::Disconnected,
            Some("transport subscriber disconnected"),
        );
    }
}

async fn sse_handler(State(st): State<RouterState>) -> Response {
    let rt = st.rt.clone();
    let surface = st.surface.clone();
    // Subscribe before capturing any reconnect projection. A mutation racing
    // this read is therefore present either in the snapshot, in the receiver
    // queue, or both—never lost. Reconnect projections are full idempotent
    // state views, so duplicate delivery at this boundary is intentional and
    // clients replace state rather than append it.
    let (rx, surface_snapshot) = match subscribe_surface_replay(&rt, surface.as_ref()) {
        Ok(capture) => capture,
        Err(error) => {
            tracing::warn!(%error, "failed to build authoritative SSE state snapshot");
            return (StatusCode::SERVICE_UNAVAILABLE, error).into_response();
        }
    };
    rt.activity.record_connection(
        "connection.sse",
        ActivityOutcome::Connected,
        Some("AG-UI event subscriber connected"),
    );
    let (history, active_message_events, initial_tutor, awaiting, pending_decisions) = {
        let _replay_guard = rt.transcript_replay_lock.lock();
        let history = rt.history.lock().iter().cloned().collect::<Vec<_>>();
        let active = narration::active_message_events(rt.as_ref());
        let tutor = initial_tutor_state(rt.as_ref());
        let awaiting = rt.awaiting.load(Ordering::Relaxed);
        let pending = pending_decision_events(rt.as_ref());
        (history, active, tutor, awaiting, pending)
    };
    let mut initial = Vec::new();
    let ready_evt = serde_json::to_string(&AgUiEvent::<JsonValue>::Custom(CustomEvent {
        base: BaseEvent::default(),
        name: "surface.tutor".to_string(),
        value: initial_tutor,
    }));
    match ready_evt {
        Ok(ready_evt) => initial.push(ready_evt),
        Err(error) => tracing::error!(%error, "failed to serialize SSE tutor state"),
    }

    initial.push(surface_snapshot);

    // Restore the "your turn" state if the tutor is mid-await. The recall
    // question itself is part of the completed transcript replay below.
    if awaiting {
        if let Ok(json) = serde_json::to_string(&AgUiEvent::<JsonValue>::Custom(CustomEvent {
            base: BaseEvent::default(),
            name: "surface.tutor".to_string(),
            value: json!({ "state": "awaiting" }),
        })) {
            initial.push(json);
        }
    }

    // UI-chrome replay: the runtime doesn't know any Surface's chrome shape or
    // event names — it iterates `SurfaceState::reconnect_events()`, which each
    // Surface reprojects from its own state under its own event names
    // (teaching-canvas: `canvas.chat`/`canvas.widget`). A Surface with nothing
    // to replay (Vellum, Colab) returns an empty vec — harmless no-op. Leak 3
    // (round-3 trait review) closed here in the M5 follow-up (2026-07-10):
    // no `canvas.*` literal survives in generic runtime code.
    for (name, value) in surface.state().reconnect_events() {
        if let Ok(json) = serde_json::to_string(&AgUiEvent::<JsonValue>::Custom(CustomEvent {
            base: BaseEvent::default(),
            name,
            value,
        })) {
            initial.push(json);
        }
    }

    // A human-decision panel is a live part of the suspended turn, not a
    // fire-and-forget notification. Replay every still-pending event after the
    // state snapshot so a reload can render and answer it.
    initial.extend(pending_decisions);

    // Replay completed conversation history so the transcript survives a
    // reload.
    if !history.is_empty() {
        if let Ok(json) = serde_json::to_string(&AgUiEvent::<JsonValue>::Custom(CustomEvent {
            base: BaseEvent::default(),
            name: "surface.history".to_string(),
            value: json!({ "entries": history }),
        })) {
            initial.push(json);
        }
    }

    // A reconnect can land while a model turn is still streaming. Completed
    // history must arrive first because clients rebuild their transcript from
    // it; then reconstruct the active AG-UI message so subsequent deltas keep
    // appending to the same bubble instead of creating a duplicate.
    initial.extend(active_message_events);

    let connection_guard = ActivityConnectionGuard {
        activity: rt.activity.clone(),
        kind: "connection.sse",
    };
    let stream = futures_util::stream::iter(initial)
        .chain(tokio_stream::wrappers::BroadcastStream::new(rx).scan(
            connection_guard,
            |_guard, item| async move {
                match item {
                    Ok(json) => Some(json),
                    Err(error) => {
                        tracing::warn!(%error, "SSE client lagged; closing for clean reconnect");
                        None
                    }
                }
            },
        ))
        .map(|json| Ok::<_, std::convert::Infallible>(SseEvent::default().data(json)));
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn subscribe_surface_replay(
    rt: &runtime_state::RuntimeState,
    surface: &dyn Surface,
) -> Result<(broadcast::Receiver<String>, String), String> {
    let receiver = rt.sse_tx.subscribe();
    let snapshot = serialized_surface_snapshot(surface)?;
    Ok((receiver, snapshot))
}

fn initial_tutor_state(rt: &runtime_state::RuntimeState) -> JsonValue {
    if let Some(error) = rt.provider_error.lock().clone() {
        json!({ "state": "failed", "question": error })
    } else if rt.warming.load(Ordering::Relaxed) || !rt.ready.load(Ordering::Relaxed) {
        json!({ "state": "warming" })
    } else if rt.busy.load(Ordering::Relaxed) {
        json!({ "state": "thinking" })
    } else {
        json!({ "state": "ready" })
    }
}

fn websocket_replay_or_status(
    state: &dyn SurfaceState,
) -> Result<Vec<Vec<u8>>, (StatusCode, String)> {
    state.ws_hello().map_err(|error| {
        let status = if error == BINARY_TRANSPORT_NOT_IMPLEMENTED {
            StatusCode::NOT_IMPLEMENTED
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        (status, error)
    })
}

async fn ws_handler(
    State(st): State<RouterState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !websocket_origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "untrusted WebSocket Origin").into_response();
    }
    // This authenticates the human replica at the door. CRDT operations remain
    // replica-signed inside the document; this does not authenticate each op.
    if st.human_route_cookie_enabled && !human_replica_credential_is_valid(&st, &headers) {
        return (StatusCode::FORBIDDEN, "human replica credential required").into_response();
    }

    // Subscribe before capturing the authoritative replay so a mutation that
    // races this pre-upgrade check is present in either the replay or the
    // receiver queue. Ask the Surface to prove it implements the transport
    // before returning HTTP 101: the default is an explicit error, never an
    // accepted connection whose input is discarded.
    let frames = st.rt.ws_tx.subscribe();
    let hello = match websocket_replay_or_status(st.surface.state()) {
        Ok(hello) => hello,
        Err((status, error)) => {
            tracing::warn!(%error, "refusing websocket upgrade");
            return (status, error).into_response();
        }
    };
    upgrade
        .on_upgrade(move |socket| ws_connection(st.rt, st.surface, socket, frames, hello))
        .into_response()
}

fn websocket_origin_allowed(headers: &HeaderMap) -> bool {
    // Keep browser transport on the same loopback-origin policy as MCP.
    // Native clients omit Origin and remain supported.
    mcp::origin_allowed(headers)
}

/// The one binary transport route, fully generic: subscribe to the runtime's
/// own `ws_tx` and forward frames verbatim; ask the Surface for opaque hello
/// frames on connect and to interpret opaque inbound frames (see
/// [`SurfaceState::ws_hello`]/[`SurfaceState::ws_receive`]'s docs for why the
/// content is deliberately opaque to this crate).
async fn ws_connection(
    rt: Arc<runtime_state::RuntimeState>,
    surface: Arc<dyn Surface>,
    socket: WebSocket,
    mut frames: broadcast::Receiver<Vec<u8>>,
    hello: Vec<Vec<u8>>,
) {
    rt.activity.record_connection(
        "connection.websocket",
        ActivityOutcome::Connected,
        Some("binary surface subscriber connected"),
    );
    let _connection_guard = ActivityConnectionGuard {
        activity: rt.activity.clone(),
        kind: "connection.websocket",
    };
    let (mut sink, mut stream) = socket.split();
    for frame in hello {
        if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
            return;
        }
    }

    // Drive both halves from one task. If the outbound receiver lags, this
    // task must drop the read half too: leaving a separate inbound task alive
    // keeps the WebSocket open with an irrecoverably incomplete CRDT history.
    loop {
        tokio::select! {
            // Prefer the state stream when both branches are ready so a busy
            // inbound peer cannot postpone a detected lag indefinitely.
            biased;
            outbound = frames.recv() => match outbound {
                Ok(frame) => {
                    if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ws client lagged by {n} frames; closing for full resync");
                    let _ = sink.send(WsMessage::Close(None)).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = sink.send(WsMessage::Close(None)).await;
                    return;
                }
            },
            inbound = stream.next() => match inbound {
                Some(Ok(WsMessage::Binary(data))) => {
                    let replies = match surface.state().ws_receive(&data) {
                        Ok(replies) => replies,
                        Err(error) => {
                            tracing::warn!(%error, "rejecting invalid websocket state frame");
                            let _ = sink.send(WsMessage::Close(None)).await;
                            return;
                        }
                    };
                    for reply in replies {
                        let _ = rt.ws_tx.send(reply);
                    }
                }
                Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
                Some(Ok(WsMessage::Text(_))) => {
                    tracing::warn!("text frame on binary-only state websocket; closing");
                    let _ = sink.send(WsMessage::Close(None)).await;
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    fn a_labeled_actors_byline_is_its_room_label() {
        let agent = AttachedAgent {
            session: "session-1".to_string(),
            participant_id: "agent-1".to_string(),
            label: "reviewer".to_string(),
            client_name: "claimed-client".to_string(),
            client_version: Some("1.0".to_string()),
            responsible: None,
        };
        assert_eq!(Actor::attached(Caller::Agent, &agent).byline(), "reviewer");
    }

    /// Two people are two participants even when they have not said who they
    /// are, which is the whole difference between a byline and a pronoun.
    #[test]
    fn two_people_who_never_named_themselves_are_still_distinguishable() {
        let people = identity::People::new();
        let (first, first_token) = people.admit(
            identity::Principal::Local {
                id: "one".to_string(),
            },
            "someone",
            &[],
        );
        let (second, second_token) = people.admit(
            identity::Principal::Local {
                id: "two".to_string(),
            },
            "someone",
            &[],
        );
        let first = Actor::person(Caller::Human, &first);
        let second = Actor::person(Caller::Human, &second);
        assert_ne!(first.participant_id, second.participant_id);
        assert_ne!(first.byline(), second.byline());
        assert_ne!(first_token, second_token);
        assert_eq!(
            first.caller, second.caller,
            "identity must not have moved authorization"
        );
    }

    #[test]
    fn anonymous_actor_bylines_name_each_caller_role() {
        for (caller, expected) in [
            (Caller::Human, "human"),
            (Caller::Agent, "agent"),
            (Caller::Companion, "companion"),
            (Caller::Unknown, "unknown"),
        ] {
            assert_eq!(Actor::anonymous(caller).byline(), expected);
        }
    }

    /// The property this whole layer exists for: four agents that all call
    /// themselves "claude" are four distinguishable participants, not one.
    #[test]
    fn four_agents_named_claude_do_not_collapse_into_one_byline() {
        let mut taken: Vec<String> = Vec::new();
        for _ in 0..4 {
            taken.push(unique_label("claude", &taken));
        }
        assert_eq!(taken, ["claude", "claude-2", "claude-3", "claude-4"]);
        let unique: std::collections::HashSet<&String> = taken.iter().collect();
        assert_eq!(unique.len(), 4, "every label must be distinct");
    }

    #[test]
    fn an_unused_name_is_left_alone() {
        assert_eq!(
            unique_label("reviewer", &["claude".to_string()]),
            "reviewer"
        );
    }

    #[test]
    fn a_nameless_agent_still_gets_a_label() {
        assert_eq!(unique_label("   ", &[]), "agent");
        assert_eq!(unique_label("", &["agent".to_string()]), "agent-2");
    }

    #[test]
    fn a_label_cannot_be_stretched_past_the_participant_limit() {
        let long = "x".repeat(400);
        assert_eq!(unique_label(&long, &[]).chars().count(), 100);
    }

    /// Trivial concrete state proving `ToolDef.apply -> Effect::Mutate`
    /// composes end to end. Note it captures its own state (`Arc<Mutex<_>>`)
    /// in the tool's `apply` closure and ignores the `&dyn SurfaceState`
    /// argument the `Mutate` closure is handed — see `ToolDef::apply`'s doc
    /// comment for why that's the expected shape, not a workaround.
    struct CounterState(Mutex<i64>);

    impl SurfaceState for CounterState {
        fn backing(&self) -> StateBacking {
            StateBacking::Ephemeral
        }

        fn describe(&self) -> Result<String, String> {
            Ok(format!("count = {}", self.0.lock()))
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::Ephemeral,
                body: serde_json::json!({ "count": *self.0.lock() }),
                chrome: None,
            })
        }
    }

    #[test]
    fn tool_def_apply_composes_with_effect_mutate() {
        let state = Arc::new(CounterState(Mutex::new(0)));

        let increment = {
            let state = state.clone();
            ToolDef::new(
                "increment",
                "Increment the counter by `by`.",
                serde_json::json!({
                    "type": "object",
                    "properties": { "by": { "type": "number" } },
                    "required": ["by"]
                }),
                move |args: &serde_json::Value| {
                    let by = args.get("by").and_then(|v| v.as_i64()).unwrap_or(1);
                    let state = state.clone();
                    // A plain draw with no specific reply — `None`, like
                    // `plot_points`/`clear`/`axes` today (case 3 below).
                    Effect::Mutate(Box::new(move |_surface_state: &dyn SurfaceState| {
                        *state.0.lock() += by;
                        Ok(None)
                    }))
                },
            )
        };

        // The runtime's dispatch path: call `apply`, get an `Effect`, match
        // on it, and — for `Mutate` — invoke the closure with a `&dyn
        // SurfaceState` (here, the same concrete state the tool already
        // captured, standing in for whatever `Surface::state()` returns).
        match (increment.apply)(&serde_json::json!({ "by": 5 })) {
            Effect::Mutate(f) => assert_eq!(f(state.as_ref()), Ok(None)),
            _ => panic!("expected Effect::Mutate"),
        }

        assert_eq!(*state.0.lock(), 5);
        assert_eq!(state.backing(), StateBacking::Ephemeral);
        assert_eq!(
            state.snapshot().expect("snapshot").body,
            serde_json::json!({ "count": 5 })
        );
    }

    /// Concrete state with an "objects" list, standing in for the canvas's
    /// scene — enough to prove the three real reply shapes `apply_tool`
    /// (`examples/teaching-canvas/src/main.rs:417`) has today all compose
    /// with `ToolDef.apply -> Effect`.
    struct ListState(Mutex<Vec<String>>);

    impl SurfaceState for ListState {
        fn backing(&self) -> StateBacking {
            StateBacking::Ephemeral
        }

        fn describe(&self) -> Result<String, String> {
            Ok(format!(
                "{} objects: {:?}",
                self.0.lock().len(),
                self.0.lock()
            ))
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::Ephemeral,
                body: serde_json::json!({ "objects": *self.0.lock() }),
                chrome: None,
            })
        }
    }

    /// The three `apply_tool -> Option<String>` reply shapes from
    /// `ag-ui-surface-m2-handoff.md` §2, each mapped onto `Effect`.
    #[test]
    fn effect_covers_query_mutate_then_summarize_and_generic_ack() {
        let state = Arc::new(ListState(Mutex::new(vec!["existing".to_string()])));

        // Case 1 — `read_canvas`: a PURE QUERY. No mutation; always replies.
        let read = {
            let state = state.clone();
            ToolDef::new(
                "read_canvas",
                "See the canvas.",
                serde_json::json!({ "type": "object", "properties": {} }),
                move |_args| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_s| {
                        Ok(format!("{} objects", state.0.lock().len()))
                    }))
                },
            )
        };
        match (read.apply)(&serde_json::json!({})) {
            Effect::Query(f) => assert_eq!(f(state.as_ref()), Ok("1 objects".to_string())),
            _ => panic!("expected Effect::Query"),
        }
        assert_eq!(state.0.lock().len(), 1, "a Query must not mutate");

        // Case 2 — `diagram`: MUTATES, then replies with a summary that
        // depends on state AFTER the mutation (the post-insert total count,
        // which the caller's args alone can't tell it) — the case that kills
        // a naive "compute the reply at apply() time" design. Works here
        // because the reply is computed inside the same `FnOnce`, after the
        // mutation line, over the same captured state — no re-fetch, no
        // race, no second call into the closure.
        let diagram = {
            let state = state.clone();
            ToolDef::new(
                "diagram",
                "Draw a node/edge diagram.",
                serde_json::json!({ "nodes": { "type": "array" } }),
                move |args| {
                    let state = state.clone();
                    let new_nodes: Vec<String> = args
                        .get("nodes")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|n| n.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    Effect::Mutate(Box::new(move |_s| {
                        let mut objects = state.0.lock();
                        objects.extend(new_nodes);
                        Ok(Some(format!(
                            "drew diagram; canvas now has {} objects",
                            objects.len()
                        )))
                    }))
                },
            )
        };
        match (diagram.apply)(&serde_json::json!({ "nodes": ["a", "b"] })) {
            Effect::Mutate(f) => {
                assert_eq!(
                    f(state.as_ref()),
                    Ok(Some("drew diagram; canvas now has 3 objects".to_string()))
                )
            }
            _ => panic!("expected Effect::Mutate"),
        }
        assert_eq!(state.0.lock().len(), 3);

        // Case 3 — `clear`/`plot_points`/`axes`: mutates, no specific reply.
        // The runtime supplies the generic ack (mirroring `openai.rs`'s
        // `.unwrap_or_else(|| format!("applied {name}"))`), not this layer.
        let clear = {
            let state = state.clone();
            ToolDef::new(
                "clear",
                "Wipe the canvas.",
                serde_json::json!({ "type": "object", "properties": {} }),
                move |_args| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_s| {
                        state.0.lock().clear();
                        Ok(None)
                    }))
                },
            )
        };
        match (clear.apply)(&serde_json::json!({})) {
            Effect::Mutate(f) => assert_eq!(f(state.as_ref()), Ok(None)),
            _ => panic!("expected Effect::Mutate"),
        }
        assert!(state.0.lock().is_empty());
    }

    /// A Surface whose pointing vocabulary (`focus_events`) is its OWN, plus a
    /// `resolve` that succeeds for one known id and a non-empty
    /// `reconnect_events` — the exact hooks that closed trait-review leak 3.
    struct FocusSurface {
        state: FocusState,
        tools: Vec<ToolDef>,
    }
    struct FocusState;

    impl SurfaceState for FocusState {
        fn backing(&self) -> StateBacking {
            StateBacking::Ephemeral
        }
        fn describe(&self) -> Result<String, String> {
            Ok(String::new())
        }
        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::Ephemeral,
                body: serde_json::json!({}),
                chrome: None,
            })
        }
        fn resolve(&self, id: &str) -> Result<Option<String>, String> {
            Ok((id == "known").then(|| "the known object".to_string()))
        }
        fn reconnect_events(&self) -> Vec<(String, serde_json::Value)> {
            vec![("test.chrome".to_string(), serde_json::json!({ "k": "v" }))]
        }
    }

    impl Surface for FocusSurface {
        fn state(&self) -> &dyn SurfaceState {
            &self.state
        }
        fn tools(&self) -> &[ToolDef] {
            &self.tools
        }
        fn client_modules(&self) -> Vec<ClientModule> {
            vec![ClientModule::host("test.focus", "1.0.0", "x").event("test.chrome")]
        }
        fn focus_events(&self) -> &[&str] {
            &["test.point"]
        }
    }

    struct WebsocketState;

    impl SurfaceState for WebsocketState {
        fn backing(&self) -> StateBacking {
            StateBacking::Crdt
        }

        fn describe(&self) -> Result<String, String> {
            Ok(String::new())
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::Crdt,
                body: serde_json::json!({}),
                chrome: None,
            })
        }

        fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
            Ok(Vec::new())
        }

        fn ws_receive(&self, _data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
            Ok(Vec::new())
        }
    }

    struct WebsocketSurface {
        state: WebsocketState,
        tools: Vec<ToolDef>,
    }

    impl Surface for WebsocketSurface {
        fn state(&self) -> &dyn SurfaceState {
            &self.state
        }

        fn tools(&self) -> &[ToolDef] {
            &self.tools
        }

        fn client_modules(&self) -> Vec<ClientModule> {
            Vec::new()
        }
    }

    fn test_router_state_with_channels(
        surface: Arc<dyn Surface>,
    ) -> (RouterState, runtime_state::RuntimeChannels) {
        let human_route_cookie_enabled = needs_human_route_cookie(surface.as_ref());
        let extensions = Arc::new(ExtensionManifest::from_surface(surface.as_ref()).unwrap());
        let mcp_catalog = Arc::new(mcp::Catalog::from_actions(surface.tools()).unwrap());
        let (ws_tx, _ws_rx) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _sse_rx) = tokio::sync::broadcast::channel(8);
        let history = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-surface-router-test-auth-{}.json",
                std::process::id()
            )))
            .expect("temporary auth store should open"),
        );
        let byok = crate::turn_loop::openai::ByokConfig::default();
        let (rt, channels) = runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history,
            Vec::new(),
            auth,
            "test".to_string(),
            byok,
            false,
            0,
        );
        rt.install_action_schemas(surface.tools()).unwrap();
        (
            RouterState {
                rt,
                surface,
                semantic_targets: None,
                extensions,
                mcp_catalog,
                human_route_token: Arc::from("test-human-route-token"),
                human_route_cookie_enabled,
            },
            channels,
        )
    }

    fn test_router_state(surface: Arc<dyn Surface>) -> RouterState {
        test_router_state_with_channels(surface).0
    }

    fn websocket_surface_with_human_route() -> Arc<dyn Surface> {
        let human_action = ToolDef::new(
            "human_write",
            "Apply a human-authored write.",
            serde_json::json!({ "type": "object", "properties": {} }),
            |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
        )
        .human_only();
        Arc::new(WebsocketSurface {
            state: WebsocketState,
            tools: vec![human_action],
        })
    }

    async fn websocket_upgrade_status_line(state: RouterState, cookie: Option<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let guard = LoopbackRequestGuard {
            port,
            human_route_token: state.human_route_token.clone(),
            human_route_cookie_enabled: state.human_route_cookie_enabled,
        };
        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state)
            .layer(middleware::from_fn_with_state(
                guard,
                loopback_request_guard,
            ));
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let cookie = cookie
            .map(|value| format!("Cookie: {value}\r\n"))
            .unwrap_or_default();
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let request = format!(
            "GET /ws HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Origin: http://127.0.0.1:{port}\r\n\
             {cookie}\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = [0_u8; 1024];
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read(&mut response),
        )
        .await
        .expect("websocket handshake response timed out")
        .expect("websocket handshake response failed");
        let status_line = std::str::from_utf8(&response[..read])
            .unwrap()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        drop(stream);
        server.abort();
        status_line
    }

    struct FailingState;

    impl SurfaceState for FailingState {
        fn backing(&self) -> StateBacking {
            StateBacking::Ephemeral
        }

        fn describe(&self) -> Result<String, String> {
            Err("describe unavailable".to_string())
        }

        fn snapshot_png(&self) -> SnapshotPngFuture<'_> {
            Box::pin(async { Err("renderer unavailable".to_string()) })
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Err("snapshot unavailable".to_string())
        }

        fn resolve(&self, _id: &str) -> Result<Option<String>, String> {
            Err("focus unavailable".to_string())
        }

        fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
            Err("replay unavailable".to_string())
        }

        fn ws_receive(&self, _data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
            Err("update unavailable".to_string())
        }
    }

    struct FailingSurface {
        state: FailingState,
    }

    impl Surface for FailingSurface {
        fn state(&self) -> &dyn SurfaceState {
            &self.state
        }

        fn tools(&self) -> &[ToolDef] {
            &[]
        }

        fn client_modules(&self) -> Vec<ClientModule> {
            vec![ClientModule::host("test.failing", "1.0.0", "failing-root")]
        }

        fn focus_events(&self) -> &[&str] {
            &["test.focus"]
        }
    }

    #[tokio::test]
    async fn authoritative_state_failures_reach_runtime_boundaries() {
        let surface: Arc<dyn Surface> = Arc::new(FailingSurface {
            state: FailingState,
        });
        let rs = test_router_state(surface.clone());

        assert_eq!(
            state_describe_handler(State(rs.clone())).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            state_png_handler(State(rs.clone()))
                .await
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            state_png_b64_handler(State(rs.clone()))
                .await
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            semantic_handler(
                State(rs.clone()),
                Json(json!({ "name": "test.focus", "value": { "id": "object-1" } })),
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(serialized_surface_snapshot(surface.as_ref()).is_err());
        assert!(subscribe_surface_replay(rs.rt.as_ref(), surface.as_ref()).is_err());
        assert_eq!(
            sse_handler(State(rs.clone())).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(surface.state().ws_hello().is_err());
        assert!(surface.state().ws_receive(b"invalid").is_err());
    }

    #[test]
    fn text_only_surface_rejects_binary_transport() {
        let state = CounterState(Mutex::new(0));
        assert_eq!(
            state.ws_hello().expect_err("text-only replay must fail"),
            BINARY_TRANSPORT_NOT_IMPLEMENTED
        );
        assert_eq!(
            state
                .ws_receive(b"state update")
                .expect_err("text-only receive must fail"),
            BINARY_TRANSPORT_NOT_IMPLEMENTED
        );
        let (status, error) =
            websocket_replay_or_status(&state).expect_err("the HTTP upgrade must be refused");
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(error, BINARY_TRANSPORT_NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn sse_reconnect_does_not_fabricate_a_run_lifecycle() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let response = sse_handler(State(test_router_state(surface))).await;
        assert_eq!(response.status(), StatusCode::OK);

        let mut body = response.into_body().into_data_stream();
        let mut replay = String::new();
        for _ in 0..8 {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
                .await
                .expect("initial SSE replay should be immediately available")
                .expect("initial SSE replay ended before the state snapshot")
                .expect("initial SSE replay body failed");
            replay.push_str(std::str::from_utf8(&chunk).expect("SSE replay must be UTF-8"));
            if replay.contains("STATE_SNAPSHOT") {
                break;
            }
        }
        assert!(
            replay.contains("STATE_SNAPSHOT"),
            "test did not observe the authoritative reconnect boundary"
        );
        assert!(
            !replay.contains("RUN_STARTED"),
            "a connection is not a provider run and must not synthesize RUN_STARTED"
        );
    }

    #[tokio::test]
    async fn audio_control_requires_an_explicit_boolean() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);

        for body in [
            json!({ "action": "audio" }),
            json!({
                "action": "audio",
                "on": "true"
            }),
        ] {
            assert_eq!(
                control_handler(State(rs.clone()), Json(body))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
            assert!(!rs.rt.audio_on.load(Ordering::Relaxed));
        }

        assert_eq!(
            control_handler(
                State(rs.clone()),
                Json(json!({ "action": "audio", "on": true })),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert!(rs.rt.audio_on.load(Ordering::Relaxed));
    }

    #[test]
    fn websocket_origin_gate_accepts_native_and_loopback_clients_only() {
        let mut headers = HeaderMap::new();
        assert!(websocket_origin_allowed(&headers));

        headers.insert(header::ORIGIN, "http://127.0.0.1:8090".parse().unwrap());
        assert!(websocket_origin_allowed(&headers));

        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!websocket_origin_allowed(&headers));
    }

    #[tokio::test]
    async fn websocket_upgrade_without_human_credential_is_refused_when_enforcement_is_on() {
        let state = test_router_state(websocket_surface_with_human_route());
        assert!(state.human_route_cookie_enabled);
        let status_line = websocket_upgrade_status_line(state.clone(), None).await;
        assert_eq!(status_line, "HTTP/1.1 403 Forbidden");

        let forged = format!("{PERSON_COOKIE}=not-host-minted");
        let status_line = websocket_upgrade_status_line(state, Some(forged)).await;
        assert_eq!(status_line, "HTTP/1.1 403 Forbidden");
    }

    #[tokio::test]
    async fn websocket_upgrade_accepts_navigation_and_admitted_participant_credentials() {
        let state = test_router_state(websocket_surface_with_human_route());

        let navigation_cookie =
            format!("{HUMAN_ROUTE_COOKIE}={}", state.human_route_token.as_ref());
        assert_eq!(
            websocket_upgrade_status_line(state.clone(), Some(navigation_cookie)).await,
            "HTTP/1.1 101 Switching Protocols"
        );

        let _first_person = join_handler(State(state.clone()), HeaderMap::new(), None)
            .await
            .into_response();
        let second_person = join_handler(State(state.clone()), HeaderMap::new(), None)
            .await
            .into_response();
        assert_eq!(state.rt.people.everyone().len(), 2);
        let participant_cookie = second_person
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("joining a second person must mint a resume cookie")
            .to_string();
        assert_eq!(
            websocket_upgrade_status_line(state, Some(participant_cookie)).await,
            "HTTP/1.1 101 Switching Protocols"
        );
    }

    #[tokio::test]
    async fn websocket_upgrade_without_credentials_remains_open_when_enforcement_is_off() {
        let surface: Arc<dyn Surface> = Arc::new(WebsocketSurface {
            state: WebsocketState,
            tools: Vec::new(),
        });
        let state = test_router_state(surface);
        assert!(!state.human_route_cookie_enabled);
        assert_eq!(
            websocket_upgrade_status_line(state, None).await,
            "HTTP/1.1 101 Switching Protocols"
        );
    }

    #[test]
    fn application_bind_is_restricted_to_the_mcp_descriptor_host() {
        assert_eq!(
            loopback_socket_addr("127.0.0.1:0").unwrap(),
            "127.0.0.1:0".parse().unwrap()
        );
        for unsafe_addr in ["0.0.0.0:8090", "127.0.0.2:8090", "[::1]:8090"] {
            assert!(matches!(
                loopback_socket_addr(unsafe_addr),
                Err(AppError::InvalidConfiguration(_))
            ));
        }
    }

    #[test]
    fn request_guard_requires_exact_loopback_authority_and_matching_origin() {
        let mut headers = HeaderMap::new();
        assert!(!loopback_request_headers_allowed(&headers, 8090));

        headers.insert(header::HOST, "127.0.0.1:8090".parse().unwrap());
        assert!(loopback_request_headers_allowed(&headers, 8090));
        headers.insert(header::ORIGIN, "http://127.0.0.1:8090".parse().unwrap());
        assert!(loopback_request_headers_allowed(&headers, 8090));

        headers.insert(header::ORIGIN, "http://127.0.0.1:8091".parse().unwrap());
        assert!(!loopback_request_headers_allowed(&headers, 8090));
        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!loopback_request_headers_allowed(&headers, 8090));

        headers.insert(header::HOST, "localhost:8090".parse().unwrap());
        headers.insert(header::ORIGIN, "http://localhost:8090".parse().unwrap());
        assert!(loopback_request_headers_allowed(&headers, 8090));
        headers.insert(header::HOST, "attacker.example:8090".parse().unwrap());
        assert!(!loopback_request_headers_allowed(&headers, 8090));
    }

    #[tokio::test]
    async fn live_request_guard_blocks_forged_host_and_cross_origin_requests() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let human_route_token: Arc<str> = Arc::from("test-human-route-token");
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn_with_state(
                LoopbackRequestGuard {
                    port,
                    human_route_token: human_route_token.clone(),
                    human_route_cookie_enabled: true,
                },
                loopback_request_guard,
            ));
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/");

        let plain = client.get(&url).send().await.unwrap();
        assert_eq!(plain.status(), reqwest::StatusCode::NO_CONTENT);
        assert!(
            plain.headers().get(header::SET_COOKIE).is_none(),
            "a native GET is not proof of a human browser session"
        );
        let navigation = client
            .get(&url)
            .header(header::ACCEPT, "text/html,application/xhtml+xml")
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-user", "?1")
            .send()
            .await
            .unwrap();
        assert_eq!(navigation.status(), reqwest::StatusCode::NO_CONTENT);
        assert_eq!(
            navigation
                .headers()
                .get(header::SET_COOKIE)
                .and_then(|value| value.to_str().ok()),
            Some(
                format!(
                    "{HUMAN_ROUTE_COOKIE}={human_route_token}; Path=/; HttpOnly; SameSite=Strict"
                )
                .as_str()
            )
        );
        assert_eq!(
            client
                .get(&url)
                .header("origin", format!("http://127.0.0.1:{port}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NO_CONTENT
        );
        assert_eq!(
            client
                .get(&url)
                .header("host", format!("attacker.example:{port}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(&url)
                .header("origin", "https://evil.example")
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn accepted_human_turn_exposes_history_and_thinking_at_one_replay_boundary() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);
        rs.rt.warming.store(false, Ordering::Relaxed);
        rs.rt.ready.store(true, Ordering::Relaxed);

        let replay_guard = rs.rt.transcript_replay_lock.lock();
        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let turn_rt = rs.rt.clone();
        let writer = std::thread::spawn(move || {
            attempted_tx.send(()).unwrap();
            narration::begin_human_turn(
                &turn_rt,
                "you",
                "atomic question",
                Some(("surface.ask", json!({ "text": "atomic question" }))),
                Some("atomic question"),
            );
        });
        attempted_rx.recv().unwrap();
        assert!(!rs.rt.busy.load(Ordering::SeqCst));
        assert!(rs.rt.history.lock().is_empty());
        drop(replay_guard);
        writer.join().unwrap();

        let _replay_guard = rs.rt.transcript_replay_lock.lock();
        assert_eq!(rs.rt.history.lock()[0]["text"], "atomic question");
        assert_eq!(initial_tutor_state(rs.rt.as_ref())["state"], "thinking");
    }

    #[tokio::test]
    async fn ask_rejects_overlap_and_interrupt_waits_for_provider_ack() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);
        rs.rt.warming.store(false, Ordering::Relaxed);
        rs.rt.ready.store(true, Ordering::Relaxed);
        rs.rt.busy.store(true, Ordering::SeqCst);

        let overlap_response = ask_handler(
            State(rs.clone()),
            axum::http::HeaderMap::new(),
            Json(json!({ "question": "must not queue" })),
        )
        .await
        .into_response();
        let overlap_bytes = axum::body::to_bytes(overlap_response.into_body(), 4096)
            .await
            .unwrap();
        let overlap: JsonValue = serde_json::from_slice(&overlap_bytes).unwrap();
        assert_eq!(overlap["ok"], false);
        assert_eq!(overlap["busy"], true);

        let mut interrupt_rx = rs.rt.interrupt_tx.subscribe();
        let ack_rt = rs.rt.clone();
        let provider = tokio::spawn(async move {
            interrupt_rx.recv().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            ack_rt.busy.store(false, Ordering::SeqCst);
        });
        let response = interrupt_handler(State(rs)).await;
        assert_eq!(response.status(), StatusCode::OK);
        provider.await.unwrap();
    }

    #[tokio::test]
    async fn accepted_turn_owns_interrupt_receiver_before_provider_dequeue() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let (rs, mut channels) = test_router_state_with_channels(surface);
        rs.rt.warming.store(false, Ordering::Relaxed);
        rs.rt.ready.store(true, Ordering::Relaxed);
        let mut events = rs.rt.sse_tx.subscribe();

        let _accepted = ask_handler(
            State(rs.clone()),
            axum::http::HeaderMap::new(),
            Json(json!({ "question": "cancel me immediately" })),
        )
        .await;
        assert!(rs.rt.busy.load(Ordering::SeqCst));

        let interrupt_state = rs.clone();
        let interrupt =
            tokio::spawn(async move { interrupt_handler(State(interrupt_state)).await });
        let mut turn =
            tokio::time::timeout(std::time::Duration::from_secs(1), channels.ask_rx.recv())
                .await
                .expect("accepted turn should reach provider queue")
                .expect("provider queue should remain open");
        tokio::time::timeout(std::time::Duration::from_secs(1), turn.cancel.recv())
            .await
            .expect("immediate interrupt must already target queued turn")
            .expect("queued turn receiver should remain live");

        let mut thinking = 0;
        while let Ok(event) = events.try_recv() {
            let event: JsonValue = serde_json::from_str(&event).expect("valid SSE event JSON");
            if event.get("name").and_then(JsonValue::as_str) == Some("surface.tutor")
                && event.pointer("/value/state").and_then(JsonValue::as_str) == Some("thinking")
            {
                thinking += 1;
            }
        }
        assert_eq!(thinking, 1, "acceptance publishes one thinking transition");

        narration::finish_turn(&rs.rt);
        let response = interrupt.await.expect("interrupt task should finish");
        assert_eq!(response.status(), StatusCode::OK);

        let mut next_turn = rs.rt.interrupt_tx.subscribe();
        assert!(matches!(
            next_turn.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn unavailable_turn_queue_publishes_terminal_failed_state() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let (rs, channels) = test_router_state_with_channels(surface);
        rs.rt.warming.store(false, Ordering::Relaxed);
        rs.rt.ready.store(true, Ordering::Relaxed);
        drop(channels);

        let _response = ask_handler(
            State(rs.clone()),
            axum::http::HeaderMap::new(),
            Json(json!({ "question": "provider disappeared" })),
        )
        .await;

        assert!(!rs.rt.busy.load(Ordering::SeqCst));
        assert_eq!(initial_tutor_state(rs.rt.as_ref())["state"], "failed");
    }

    #[tokio::test]
    async fn failed_provider_stays_down_until_an_explicit_retry() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _) = tokio::sync::broadcast::channel(8);
        let history = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-provider-failure-test-{}.json",
                std::process::id()
            )))
            .unwrap(),
        );
        let (rt, channels) = runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history,
            Vec::new(),
            auth,
            "missing".to_string(),
            crate::turn_loop::openai::ByokConfig::default(),
            false,
            0,
        );
        rt.install_action_schemas(surface.tools()).unwrap();
        let supervisor = tokio::spawn(turn_loop::run_supervisor(
            rt.clone(),
            surface,
            Arc::new(turn_loop::Prompts::from("test")),
            None,
            std::env::current_dir().unwrap(),
            channels.ask_rx,
            channels.mission_rx,
            channels.switch_rx,
        ));

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while rt.provider_error.lock().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!rt.warming.load(Ordering::Relaxed));
        assert_eq!(rt.provider_generation.load(Ordering::SeqCst), 0);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(rt.provider_generation.load(Ordering::SeqCst), 0);

        rt.switch_tx.send("missing".to_string()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while rt.provider_generation.load(Ordering::SeqCst) != 1
                || rt.provider_error.lock().is_none()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(rt.provider_generation.load(Ordering::SeqCst), 1);
        supervisor.abort();
    }

    #[tokio::test]
    async fn malformed_passthrough_json_is_rejected_before_surface_handler() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let route = Arc::new(RouteDef {
            method: HttpMethod::Post,
            path: "/focus",
            handler: Box::new({
                let called = called.clone();
                move |_request| {
                    let called = called.clone();
                    Box::pin(async move {
                        called.store(true, Ordering::SeqCst);
                        RouteResponse::json(200, json!({ "ok": true }))
                    })
                }
            }),
        });

        let response = passthrough_route(
            route,
            HashMap::new(),
            HeaderMap::new(),
            Bytes::from_static(b"{broken"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn invalid_surface_status_fails_closed() {
        let route = Arc::new(RouteDef {
            method: HttpMethod::Get,
            path: "/custom",
            handler: Box::new(|_request| {
                Box::pin(async { RouteResponse::json(999, json!({ "ok": true })) })
            }),
        });
        let response =
            passthrough_route(route, HashMap::new(), HeaderMap::new(), Bytes::new()).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn action_routes_fail_closed_and_dispatch_only_established_humans() {
        let applied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = ToolDef::new(
            "human_write",
            "Apply a human-authored write.",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false
            }),
            {
                let applied = applied.clone();
                move |_args| {
                    let applied = applied.clone();
                    Effect::Mutate(Box::new(move |_state| {
                        applied.fetch_add(1, Ordering::SeqCst);
                        Ok(Some("written by human".to_string()))
                    }))
                }
            },
        )
        .human_only();
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: vec![tool],
        });
        let state = test_router_state(surface);
        let route = Arc::new(ActionRouteDef::post(
            "/paper/write",
            "human_write",
            |_request| Box::pin(async { RouteResponse::html(200, "<p>rendered</p>") }),
        ));

        let response = action_route_request(
            state.clone(),
            route.clone(),
            HashMap::new(),
            HeaderMap::new(),
            Bytes::from_static(b"text=anonymous"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(applied.load(Ordering::SeqCst), 0);

        let mut agent_headers = HeaderMap::new();
        agent_headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", state.rt.mcp_token).parse().unwrap(),
        );
        agent_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let response = action_route_request(
            state.clone(),
            route.clone(),
            HashMap::new(),
            agent_headers,
            Bytes::from_static(b"{not-json"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: JsonValue = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["result"],
            "action 'human_write' is not available to this agent"
        );
        assert_eq!(
            applied.load(Ordering::SeqCst),
            0,
            "audience refusal must happen before malformed arguments are parsed"
        );

        let mut fake_cookie = HeaderMap::new();
        fake_cookie.insert(
            header::COOKIE,
            format!("{HUMAN_ROUTE_COOKIE}=forged").parse().unwrap(),
        );
        let response = action_route_request(
            state.clone(),
            route.clone(),
            HashMap::new(),
            fake_cookie,
            Bytes::from_static(b"text=forged"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(applied.load(Ordering::SeqCst), 0);

        let mut human_headers = HeaderMap::new();
        human_headers.insert(
            header::COOKIE,
            format!("{HUMAN_ROUTE_COOKIE}={}", state.human_route_token.as_ref())
                .parse()
                .unwrap(),
        );
        human_headers.insert(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        let response = action_route_request(
            state.clone(),
            route.clone(),
            HashMap::new(),
            human_headers.clone(),
            Bytes::from_static(b"text=real"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(applied.load(Ordering::SeqCst), 1);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<p>rendered</p>");

        human_headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", state.rt.mcp_token).parse().unwrap(),
        );
        let response = action_route_request(
            state,
            route,
            HashMap::new(),
            human_headers,
            Bytes::from_static(b"text=ambiguous"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            applied.load(Ordering::SeqCst),
            1,
            "conflicting agent and human credentials must not select human"
        );
    }

    /// `RouteRequest::query` used to be documented but always empty, so a
    /// `GET` route with a parameter read `""` and failed in a way that looked
    /// like a caller bug.
    #[tokio::test]
    async fn a_surface_route_receives_the_query_string() {
        let route = Arc::new(RouteDef {
            method: HttpMethod::Get,
            path: "/custom",
            handler: Box::new(|request: RouteRequest| {
                Box::pin(async move {
                    RouteResponse::json(
                        200,
                        json!({
                            "path": request.query.get("path").cloned().unwrap_or_default(),
                            "lines": request.query.get("lines").cloned().unwrap_or_default(),
                        }),
                    )
                })
            }),
        });
        let query = HashMap::from([
            (
                "path".to_string(),
                "crates/ag-ui-core/src/event.rs".to_string(),
            ),
            ("lines".to_string(), "1-40".to_string()),
        ]);
        let response = passthrough_route(route, query, HeaderMap::new(), Bytes::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let body: JsonValue = serde_json::from_slice(&body).expect("json body");
        assert_eq!(body["path"], "crates/ag-ui-core/src/event.rs");
        assert_eq!(body["lines"], "1-40");
    }

    #[test]
    fn dynamic_http_layout_rejects_paths_that_would_conflict_or_panic() {
        fn route(path: &'static str, method: HttpMethod) -> RouteDef {
            RouteDef {
                method,
                path,
                handler: Box::new(|_request| {
                    Box::pin(async { RouteResponse::json(200, json!({})) })
                }),
            }
        }

        fn layout(routes: &[RouteDef], mounts: &[(String, PathBuf)]) -> Result<(), String> {
            validate_http_layout(routes, &[], &[], mounts)
        }

        assert!(layout(&[route("relative", HttpMethod::Get)], &[]).is_err());
        assert!(layout(&[route("/:id", HttpMethod::Get)], &[]).is_err());
        assert!(layout(&[route("/custom/:id", HttpMethod::Get)], &[]).is_err());
        assert!(layout(&[route("/ws", HttpMethod::Get)], &[]).is_err());
        assert!(layout(&[route("/pkg/board.js", HttpMethod::Get)], &[]).is_err());
        assert!(layout(&[route("/_agui/alternate.js", HttpMethod::Get)], &[]).is_err());
        assert!(layout(
            &[
                route("/custom", HttpMethod::Post),
                route("/custom", HttpMethod::Post),
            ],
            &[],
        )
        .is_err());
        assert!(layout(
            &[route("/preview/state", HttpMethod::Get)],
            &[("/preview".to_string(), PathBuf::from("."))],
        )
        .is_err());
        assert!(layout(
            &[],
            &[
                ("/preview".to_string(), PathBuf::from(".")),
                ("/preview/assets".to_string(), PathBuf::from(".")),
            ],
        )
        .is_err());
        assert!(layout(
            &[
                route("/mission", HttpMethod::Get),
                route("/mission", HttpMethod::Post),
            ],
            &[("/preview".to_string(), PathBuf::from("."))],
        )
        .is_ok());
        assert!(layout(
            &[],
            &[("/extensions/board".to_string(), PathBuf::from("."))],
        )
        .is_ok());
        assert!(layout(&[], &[("/pkg/board".to_string(), PathBuf::from("."))],).is_err());
        assert!(layout(&[], &[("/_agui/assets".to_string(), PathBuf::from("."))],).is_err());
        assert!(layout(&[], &[("/:id".to_string(), PathBuf::from("."))],).is_err());
        assert!(layout(&[], &[("/extensions/:id".to_string(), PathBuf::from("."))],).is_err());
        assert!(layout(
            &[route("/preview", HttpMethod::Get)],
            &[("/preview/assets".to_string(), PathBuf::from("."))],
        )
        .is_ok());

        fn action_route(
            path: &'static str,
            method: HttpMethod,
            action: &'static str,
        ) -> ActionRouteDef {
            ActionRouteDef {
                method,
                path,
                action,
                on_success: Box::new(|_request| {
                    Box::pin(async { RouteResponse::json(200, json!({})) })
                }),
            }
        }
        let action = ToolDef::new(
            "write",
            "write",
            json!({ "type": "object", "properties": {} }),
            |_args| Effect::Reject("not called by layout validation".to_string()),
        )
        .human_only();
        assert!(validate_http_layout(
            &[],
            &[action_route("/paper/write", HttpMethod::Post, "write")],
            std::slice::from_ref(&action),
            &[],
        )
        .is_ok());
        assert!(validate_http_layout(
            &[],
            &[action_route("/paper/missing", HttpMethod::Post, "missing")],
            std::slice::from_ref(&action),
            &[],
        )
        .unwrap_err()
        .contains("unknown action"));
        assert!(validate_http_layout(
            &[],
            &[action_route("/paper/read", HttpMethod::Get, "write")],
            std::slice::from_ref(&action),
            &[],
        )
        .unwrap_err()
        .contains("mutating HTTP method"));
        assert!(validate_http_layout(
            &[route("/paper/write", HttpMethod::Post)],
            &[action_route("/paper/write", HttpMethod::Post, "write")],
            std::slice::from_ref(&action),
            &[],
        )
        .unwrap_err()
        .contains("duplicate surface route"));
    }

    #[test]
    fn static_directory_preflight_rejects_missing_and_non_directory_roots() {
        let root = std::env::temp_dir().join(format!(
            "ag-ui-static-preflight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let static_dir = root.join("static");
        let empty_dir = root.join("empty");
        let pkg_dir = root.join("pkg");
        let pkg_file = root.join("pkg-file");
        let preview_dir = root.join("preview");
        std::fs::create_dir_all(&static_dir).unwrap();
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::create_dir_all(&preview_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), b"<!doctype html>").unwrap();
        std::fs::write(&pkg_file, b"not a directory").unwrap();

        assert!(validate_static_directories(Path::new(""), None, &[])
            .unwrap_err()
            .contains("must not be empty"));

        assert_eq!(
            validate_static_directories(
                &static_dir,
                None,
                &[("/preview".to_string(), preview_dir.clone())],
            ),
            Ok(())
        );
        assert!(validate_static_directories(&empty_dir, None, &[])
            .unwrap_err()
            .contains("static entry"));
        assert_eq!(
            validate_static_directories(&static_dir, Some(&pkg_dir), &[]),
            Ok(())
        );
        assert!(
            validate_static_directories(&static_dir, Some(&pkg_file), &[])
                .unwrap_err()
                .contains("not a directory")
        );
        assert!(
            validate_static_directories(&root.join("missing"), None, &[])
                .unwrap_err()
                .contains("cannot be read")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn invalid_choice_reply_does_not_consume_pending_decision() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        rs.rt.decision.lock().insert(
            "choice-1".to_string(),
            runtime_state::PendingDecision {
                token: "choice-token".to_string(),
                reply_kind: ReplyKind::Choice {
                    allowed: vec!["option-a".to_string(), "blank".to_string()],
                    text_required_for: vec!["blank".to_string()],
                },
                on_reply: Some(Box::new(|reply: JsonValue| Ok(reply.to_string()))),
                event_json: json!({ "type": "CUSTOM", "name": "test.choice" }).to_string(),
                reply_tx: Some(tx),
            },
        );

        let _invalid = decision_handler(
            State(rs.clone()),
            Json(json!({ "id": "choice-1", "chosen": "" })),
        )
        .await;
        assert!(rs.rt.decision.lock().contains_key("choice-1"));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        let _missing_text = decision_handler(
            State(rs.clone()),
            Json(json!({ "id": "choice-1", "chosen": "blank", "text": "  " })),
        )
        .await;
        assert!(rs.rt.decision.lock().contains_key("choice-1"));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        let reply = json!({ "id": "choice-1", "chosen": "option-a", "text": "because" });
        let _valid = decision_handler(State(rs.clone()), Json(reply.clone())).await;
        assert!(!rs.rt.decision.lock().contains_key("choice-1"));
        assert_eq!(rx.await, Ok(Ok(reply.to_string())));
    }

    #[tokio::test]
    async fn any_reply_kind_delegates_bespoke_shape_after_id_validation() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);
        let (tx, rx) = tokio::sync::oneshot::channel();
        rs.rt.decision.lock().insert(
            "variant-a".to_string(),
            runtime_state::PendingDecision {
                token: "any-token".to_string(),
                reply_kind: ReplyKind::Any,
                on_reply: Some(Box::new(|reply: JsonValue| Ok(reply.to_string()))),
                event_json: json!({ "type": "CUSTOM", "name": "test.any" }).to_string(),
                reply_tx: Some(tx),
            },
        );

        let reply = json!({ "id": "variant-a", "status": "approved", "comment": "ship it" });
        let _response = decision_handler(State(rs), Json(reply.clone())).await;
        assert_eq!(rx.await, Ok(Ok(reply.to_string())));
    }

    #[tokio::test]
    async fn invalid_approval_does_not_consume_pending_decision() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);
        let (tx, rx) = tokio::sync::oneshot::channel();
        rs.rt.decision.lock().insert(
            "variant-a".to_string(),
            runtime_state::PendingDecision {
                token: "approval-token".to_string(),
                reply_kind: ReplyKind::Approval {
                    require_reject_comment: true,
                },
                on_reply: Some(Box::new(|reply: JsonValue| Ok(reply.to_string()))),
                event_json: json!({ "type": "CUSTOM", "name": "test.approval" }).to_string(),
                reply_tx: Some(tx),
            },
        );

        let invalid = decision_handler(
            State(rs.clone()),
            Json(json!({ "id": "variant-a", "status": "rejected", "comment": "" })),
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(rs.rt.decision.lock().contains_key("variant-a"));

        let reply =
            json!({ "id": "variant-a", "status": "rejected", "comment": "reduce contrast" });
        let valid = decision_handler(State(rs), Json(reply.clone())).await;
        assert_eq!(valid.status(), StatusCode::OK);
        assert_eq!(rx.await, Ok(Ok(reply.to_string())));
    }

    #[test]
    fn reconnect_frames_include_state_snapshot_and_pending_decisions() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface.clone());
        let (a_tx, _a_rx) = tokio::sync::oneshot::channel();
        let (b_tx, _b_rx) = tokio::sync::oneshot::channel();
        rs.rt.decision.lock().insert(
            "b".to_string(),
            runtime_state::PendingDecision {
                token: "pending-b-token".to_string(),
                reply_kind: ReplyKind::Any,
                on_reply: Some(Box::new(|_| Ok("b".to_string()))),
                event_json: json!({ "type": "CUSTOM", "name": "pending.b" }).to_string(),
                reply_tx: Some(b_tx),
            },
        );
        rs.rt.decision.lock().insert(
            "a".to_string(),
            runtime_state::PendingDecision {
                token: "pending-a-token".to_string(),
                reply_kind: ReplyKind::Any,
                on_reply: Some(Box::new(|_| Ok("a".to_string()))),
                event_json: json!({ "type": "CUSTOM", "name": "pending.a" }).to_string(),
                reply_tx: Some(a_tx),
            },
        );

        let snapshot: JsonValue = serde_json::from_str(
            &serialized_surface_snapshot(surface.as_ref()).expect("snapshot should serialize"),
        )
        .expect("snapshot event should be JSON");
        assert_eq!(snapshot["type"], "STATE_SNAPSHOT");
        assert_eq!(snapshot["snapshot"]["backing"], "ephemeral");
        assert_eq!(snapshot["snapshot"]["body"], json!({}));

        let pending = pending_decision_events(rs.rt.as_ref());
        assert_eq!(pending.len(), 2);
        assert!(pending[0].contains("pending.a"));
        assert!(pending[1].contains("pending.b"));
    }

    #[test]
    fn reconnect_subscribes_before_a_blocking_surface_snapshot() {
        struct BlockingState {
            entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
            release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
            version: std::sync::atomic::AtomicU64,
        }
        impl SurfaceState for BlockingState {
            fn backing(&self) -> StateBacking {
                StateBacking::LastWriterWins
            }
            fn describe(&self) -> Result<String, String> {
                Ok(self.version.load(Ordering::SeqCst).to_string())
            }
            fn snapshot(&self) -> Result<StateSnapshot, String> {
                if let Some(entered) = self.entered.lock().take() {
                    entered.send(()).map_err(|error| error.to_string())?;
                }
                if let Some(release) = self.release.lock().take() {
                    release.recv().map_err(|error| error.to_string())?;
                }
                Ok(StateSnapshot {
                    backing: StateBacking::LastWriterWins,
                    body: json!({ "version": self.version.load(Ordering::SeqCst) }),
                    chrome: None,
                })
            }
        }
        struct BlockingSurface {
            state: Arc<BlockingState>,
            tools: Vec<ToolDef>,
        }
        impl Surface for BlockingSurface {
            fn state(&self) -> &dyn SurfaceState {
                self.state.as_ref()
            }
            fn tools(&self) -> &[ToolDef] {
                &self.tools
            }
            fn client_modules(&self) -> Vec<ClientModule> {
                vec![ClientModule::host(
                    "test.blocking",
                    "1.0.0",
                    "blocking-root",
                )]
            }
        }

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let state = Arc::new(BlockingState {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
            version: std::sync::atomic::AtomicU64::new(0),
        });
        let surface: Arc<dyn Surface> = Arc::new(BlockingSurface {
            state: state.clone(),
            tools: Vec::new(),
        });
        let router = test_router_state(surface.clone());
        let rt = router.rt.clone();
        let capture = std::thread::spawn(move || subscribe_surface_replay(&rt, surface.as_ref()));

        entered_rx.recv().expect("snapshot entered");
        state.version.store(1, Ordering::SeqCst);
        router
            .rt
            .sse_tx
            .send("version-1".into())
            .expect("subscribed reconnect receives the racing update");
        release_tx.send(()).expect("release snapshot");

        let (mut receiver, _snapshot) = capture
            .join()
            .expect("capture thread")
            .expect("reconnect capture");
        assert_eq!(receiver.try_recv(), Ok("version-1".into()));
    }

    #[test]
    fn pending_decision_replay_boundary_is_snapshot_or_live_and_close_is_keyed() {
        fn pending(
            token: &str,
            id: &str,
        ) -> (
            runtime_state::PendingDecision,
            tokio::sync::oneshot::Receiver<EffectResult<String>>,
        ) {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let event = AgUiEvent::<JsonValue>::Custom(CustomEvent {
                base: BaseEvent::default(),
                name: "test.decision.pending".to_string(),
                value: json!({ "id": id }),
            });
            (
                runtime_state::PendingDecision {
                    token: token.to_string(),
                    reply_kind: ReplyKind::Any,
                    on_reply: Some(Box::new(|_| Ok("resolved".to_string()))),
                    event_json: serde_json::to_string(&event).unwrap(),
                    reply_tx: Some(reply_tx),
                },
                reply_rx,
            )
        }

        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);

        // Register first: reconnect snapshot contains the pending event and its
        // newly-created receiver has no duplicate live copy.
        let (a, _a_reply) = pending("token-a", "a");
        assert!(rs.rt.register_pending_decision("a".to_string(), a).is_ok());
        let (mut a_rx, a_snapshot) = {
            let _guard = rs.rt.transcript_replay_lock.lock();
            (
                rs.rt.sse_tx.subscribe(),
                pending_decision_events(rs.rt.as_ref()),
            )
        };
        assert_eq!(a_snapshot.len(), 1);
        assert!(a_snapshot[0].contains("\"id\":\"a\""));
        assert!(a_rx.try_recv().is_err());
        rs.rt
            .close_pending_decision_if_token("a", "token-a", "interrupted");
        let a_closed: JsonValue = serde_json::from_str(&a_rx.try_recv().unwrap()).unwrap();
        assert_eq!(a_closed["name"], "surface.decision.closed");
        assert_eq!(a_closed["value"]["id"], "a");

        // Subscribe first: snapshot is empty and registration arrives live.
        let (mut b_rx, b_snapshot) = {
            let _guard = rs.rt.transcript_replay_lock.lock();
            (
                rs.rt.sse_tx.subscribe(),
                pending_decision_events(rs.rt.as_ref()),
            )
        };
        assert!(b_snapshot.is_empty());
        let (b, _b_reply) = pending("token-b", "b");
        assert!(rs.rt.register_pending_decision("b".to_string(), b).is_ok());
        let b_live: JsonValue = serde_json::from_str(&b_rx.try_recv().unwrap()).unwrap();
        assert_eq!(b_live["name"], "test.decision.pending");
        rs.rt
            .close_pending_decision_if_token("b", "token-b", "resolved");

        // Close before reconnect: neither snapshot nor future live queue can
        // retain the obsolete decision.
        let (c, _c_reply) = pending("token-c", "c");
        assert!(rs.rt.register_pending_decision("c".to_string(), c).is_ok());
        rs.rt
            .close_pending_decision_if_token("c", "token-c", "interrupted");
        let (mut c_rx, c_snapshot) = {
            let _guard = rs.rt.transcript_replay_lock.lock();
            (
                rs.rt.sse_tx.subscribe(),
                pending_decision_events(rs.rt.as_ref()),
            )
        };
        assert!(c_snapshot.is_empty());
        assert!(c_rx.try_recv().is_err());

        // An old waiter's Drop token cannot close a same-id replacement.
        let (old, _old_reply) = pending("old-token", "same");
        assert!(rs
            .rt
            .register_pending_decision("same".to_string(), old)
            .is_ok());
        rs.rt
            .close_pending_decision_if_token("same", "old-token", "resolved");
        let (new, _new_reply) = pending("new-token", "same");
        assert!(rs
            .rt
            .register_pending_decision("same".to_string(), new)
            .is_ok());
        assert!(rs
            .rt
            .close_pending_decision_if_token("same", "old-token", "wait_dropped")
            .is_none());
        assert_eq!(
            rs.rt
                .decision
                .lock()
                .get("same")
                .map(|pending| pending.token.as_str()),
            Some("new-token")
        );
        rs.rt
            .close_pending_decision_if_token("same", "new-token", "cleanup");
    }

    #[test]
    fn provider_cleanup_closes_all_decisions_and_wakes_waiters() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);
        let mut events = rs.rt.sse_tx.subscribe();
        let mut replies = Vec::new();
        for id in ["a", "b"] {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            replies.push(reply_rx);
            assert!(rs
                .rt
                .register_pending_decision(
                    id.to_string(),
                    runtime_state::PendingDecision {
                        token: format!("token-{id}"),
                        reply_kind: ReplyKind::Any,
                        on_reply: Some(Box::new(|_| Ok("resolved".to_string()))),
                        event_json: json!({ "pending": id }).to_string(),
                        reply_tx: Some(reply_tx),
                    },
                )
                .is_ok());
        }
        // Discard the two pending broadcasts, then observe terminal events.
        assert!(events.try_recv().is_ok());
        assert!(events.try_recv().is_ok());
        assert_eq!(rs.rt.close_all_pending_decisions("provider_switched"), 2);
        assert!(rs.rt.decision.lock().is_empty());
        assert!(replies.iter_mut().all(|reply| matches!(
            reply.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        )));
        let mut closed_ids = std::collections::BTreeSet::new();
        for _ in 0..2 {
            let event: JsonValue = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
            assert_eq!(event["name"], "surface.decision.closed");
            assert_eq!(event["value"]["reason"], "provider_switched");
            closed_ids.insert(event["value"]["id"].as_str().unwrap().to_string());
        }
        assert_eq!(closed_ids, ["a".to_string(), "b".to_string()].into());
    }

    #[test]
    fn transport_tutor_state_is_live_and_shared_with_reconnect_state() {
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, mut events) = tokio::sync::broadcast::channel(8);
        let awaiting = Arc::new(AtomicBool::new(false));
        let replay_lock = Arc::new(Mutex::new(()));
        let transport = Transport {
            ws_tx: ws_tx.clone(),
            sse_tx: sse_tx.clone(),
            history: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            awaiting: awaiting.clone(),
            transcript_replay_lock: replay_lock.clone(),
        };
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-tutor-transport-test-{}.json",
                std::process::id()
            )))
            .unwrap(),
        );
        let (rt, _) = runtime_state::RuntimeState::new_with_shared_tutor(
            ws_tx,
            sse_tx,
            transport.history.clone(),
            Vec::new(),
            auth,
            "test".to_string(),
            turn_loop::openai::ByokConfig::default(),
            false,
            0,
            awaiting,
            replay_lock,
        );

        transport.tutor_state("awaiting", Some("recall?"));
        assert!(rt.awaiting.load(Ordering::Relaxed));
        let live: JsonValue = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
        assert_eq!(live["name"], "surface.tutor");
        assert_eq!(live["value"]["state"], "awaiting");
        transport.tutor_state("thinking", None);
        assert!(!rt.awaiting.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn extension_manifest_is_generated_from_client_module_and_tools() {
        let tool = ToolDef::new(
            "point",
            "Point at a thing.",
            serde_json::json!({ "type": "object", "properties": {} }),
            |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
        );
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: vec![tool],
        });
        let manifest = ExtensionManifest::from_surface(surface.as_ref()).unwrap();

        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.protocol, "ag-ui");
        assert_eq!(manifest.extensions.len(), 1);
        let extension = &manifest.extensions[0];
        assert_eq!(extension.id, "test.focus");
        assert_eq!(extension.loading, "host");
        assert_eq!(extension.module, None);
        assert_eq!(extension.mount, "x");
        assert_eq!(extension.actions, vec!["point"]);
        assert_eq!(extension.events, vec!["test.chrome"]);

        let rs = test_router_state(surface);
        let Json(wire) = extensions_handler(State(rs)).await;
        assert_eq!(wire, manifest);
        let json = serde_json::to_value(wire).unwrap();
        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["extensions"][0]["loading"], "host");
        assert!(json["extensions"][0].get("module").is_none());
    }

    #[test]
    fn client_module_rejects_cross_origin_and_parent_paths() {
        let external = ClientModule::lazy("bad", "1.0.0", "https://example.com/plugin.js", "x");
        assert!(external.validate().unwrap_err().contains("same-origin"));

        let traversal = ClientModule::lazy("bad", "1.0.0", "/extensions/../secret.js", "x");
        assert!(traversal.validate().unwrap_err().contains("same-origin"));

        let valid = ClientModule::lazy("board", "1.0.0", "/extensions/board/index.js", "board")
            .wasm(WasmMount {
                pkg_name: "board_wasm".to_string(),
                js: "/pkg/board.js".to_string(),
                wasm: "/pkg/board_bg.wasm".to_string(),
            });
        assert_eq!(valid.validate(), Ok(()));
    }

    struct MultiModuleSurface {
        state: FocusState,
        tools: Vec<ToolDef>,
        modules: Vec<ClientModule>,
    }

    impl Surface for MultiModuleSurface {
        fn state(&self) -> &dyn SurfaceState {
            &self.state
        }
        fn tools(&self) -> &[ToolDef] {
            &self.tools
        }
        fn client_modules(&self) -> Vec<ClientModule> {
            self.modules.clone()
        }
    }

    #[test]
    fn multiple_extensions_keep_action_ownership_separate() {
        let tool = |name: &str| {
            ToolDef::new(
                name,
                "test action",
                serde_json::json!({ "type": "object", "properties": {} }),
                |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
            )
        };
        let surface = MultiModuleSurface {
            state: FocusState,
            tools: vec![tool("add_note"), tool("draw")],
            modules: vec![
                ClientModule::lazy("board", "1.0.0", "/extensions/board.js", "board")
                    .action("add_note"),
                ClientModule::lazy("canvas", "1.0.0", "/extensions/canvas.js", "canvas")
                    .action("draw"),
            ],
        };
        let manifest = ExtensionManifest::from_surface(&surface).unwrap();
        assert_eq!(manifest.extensions.len(), 2);
        assert_eq!(manifest.extensions[0].actions, vec!["add_note"]);
        assert_eq!(manifest.extensions[1].actions, vec!["draw"]);

        let invalid = MultiModuleSurface {
            state: FocusState,
            tools: vec![tool("add_note")],
            modules: vec![
                ClientModule::lazy("board", "1.0.0", "/extensions/board.js", "board")
                    .action("delete_everything"),
            ],
        };
        assert!(ExtensionManifest::from_surface(&invalid)
            .unwrap_err()
            .contains("unknown or non-human action"));
    }

    #[test]
    fn browser_manifest_contains_only_human_visible_actions() {
        let action = |name: &str, audience: ActionAudience| {
            ToolDef::new(
                name,
                "audience test",
                serde_json::json!({ "type": "object", "properties": {} }),
                |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
            )
            .audience(audience)
        };
        let surface = MultiModuleSurface {
            state: FocusState,
            tools: vec![
                action("human", ActionAudience::Human),
                action("agent", ActionAudience::Agent),
                action("both", ActionAudience::Both),
            ],
            modules: vec![
                ClientModule::lazy("board", "1.0.0", "/extensions/board.js", "board")
                    .action("human")
                    .action("both"),
            ],
        };
        let manifest = ExtensionManifest::from_surface(&surface).unwrap();
        assert_eq!(manifest.extensions[0].actions, vec!["human", "both"]);

        let invalid = MultiModuleSurface {
            state: FocusState,
            tools: vec![action("agent", ActionAudience::Agent)],
            modules: vec![
                ClientModule::lazy("board", "1.0.0", "/extensions/board.js", "board")
                    .action("agent"),
            ],
        };
        assert!(ExtensionManifest::from_surface(&invalid)
            .unwrap_err()
            .contains("non-human"));
    }

    #[tokio::test]
    async fn human_action_arguments_are_validated_before_apply() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let apply_count = Arc::new(AtomicUsize::new(0));
        let action = ToolDef::new(
            "set_label",
            "Set a label.",
            serde_json::json!({
                "type": "object",
                "properties": { "label": { "type": "string", "minLength": 1 } },
                "required": ["label"],
                "additionalProperties": false
            }),
            {
                let apply_count = apply_count.clone();
                move |_args| {
                    apply_count.fetch_add(1, Ordering::SeqCst);
                    Effect::Query(Box::new(|_state| Ok("set".to_string())))
                }
            },
        )
        .human_only();
        let surface: Arc<dyn Surface> = Arc::new(MultiModuleSurface {
            state: FocusState,
            tools: vec![action],
            modules: vec![
                ClientModule::lazy("labels", "1.0.0", "/extensions/labels.js", "labels")
                    .action("set_label"),
            ],
        });
        let state = test_router_state(surface);
        let mut human_headers = axum::http::HeaderMap::new();
        human_headers.insert(
            header::COOKIE,
            format!("{HUMAN_ROUTE_COOKIE}={}", state.human_route_token.as_ref())
                .parse()
                .unwrap(),
        );

        let invalid = tool_handler(
            State(state.clone()),
            human_headers.clone(),
            Json(serde_json::json!({ "name": "set_label", "args": { "label": 9 } })),
        )
        .await
        .into_response();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(apply_count.load(Ordering::SeqCst), 0);

        for identity in [json!("humna"), json!(7)] {
            let unknown = tool_handler(
                State(state.clone()),
                human_headers.clone(),
                Json(serde_json::json!({
                    "name": "set_label",
                    "args": { "label": "forged" },
                    "as": identity
                })),
            )
            .await
            .into_response();
            assert_eq!(unknown.status(), StatusCode::FORBIDDEN);
            assert_eq!(
                apply_count.load(Ordering::SeqCst),
                0,
                "an unknown caller must not reach a human-only action"
            );
        }

        let valid = tool_handler(
            State(state),
            human_headers,
            Json(serde_json::json!({ "name": "set_label", "args": { "label": "ready" } })),
        )
        .await
        .into_response();
        assert_eq!(valid.status(), StatusCode::OK);
        assert_eq!(apply_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn surface_action_without_human_cookie_cannot_call_human_only_action() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let human_apply_count = Arc::new(AtomicUsize::new(0));
        let human_action = ToolDef::new(
            "human_write",
            "Apply a human-authored write.",
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false
            }),
            {
                let apply_count = human_apply_count.clone();
                move |_args| {
                    let apply_count = apply_count.clone();
                    Effect::Mutate(Box::new(move |_state| {
                        apply_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Some("written by human".to_string()))
                    }))
                }
            },
        )
        .human_only();
        let agent_apply_count = Arc::new(AtomicUsize::new(0));
        let agent_action = ToolDef::new(
            "agent_write",
            "Apply an agent-authored write.",
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false
            }),
            {
                let apply_count = agent_apply_count.clone();
                move |_args| {
                    let apply_count = apply_count.clone();
                    Effect::Mutate(Box::new(move |_state| {
                        apply_count.fetch_add(1, Ordering::SeqCst);
                        Ok(Some("written by agent".to_string()))
                    }))
                }
            },
        )
        .agent_only();
        let surface: Arc<dyn Surface> = Arc::new(MultiModuleSurface {
            state: FocusState,
            tools: vec![human_action, agent_action],
            modules: vec![ClientModule::lazy(
                "records",
                "1.0.0",
                "/extensions/records.js",
                "records",
            )
            .action("human_write")],
        });
        let state = test_router_state(surface);
        let mcp_token = state.rt.mcp_token.clone();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/surface/action", post(tool_handler))
            .route("/canvas-tool", post(tool_handler))
            .with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/surface/action"))
            .json(&serde_json::json!({
                "name": "human_write",
                "args": { "text": "forged" }
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(human_apply_count.load(Ordering::SeqCst), 0);

        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/canvas-tool"))
            .json(&serde_json::json!({
                "name": "human_write",
                "args": { "text": "forged alias" }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(human_apply_count.load(Ordering::SeqCst), 0);

        let cookie = format!("{HUMAN_ROUTE_COOKIE}=test-human-route-token");
        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/surface/action"))
            .header(header::COOKIE, &cookie)
            .json(&serde_json::json!({
                "name": "human_write",
                "args": { "text": "real" }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(human_apply_count.load(Ordering::SeqCst), 1);

        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/surface/action"))
            .header(header::COOKIE, &cookie)
            .json(&serde_json::json!({
                "name": "agent_write",
                "args": { "text": "narrowed" },
                "as": "agent"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(agent_apply_count.load(Ordering::SeqCst), 1);

        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/surface/action"))
            .header(header::AUTHORIZATION, format!("Bearer {mcp_token}"))
            .json(&serde_json::json!({
                "name": "agent_write",
                "args": { "text": "authenticated" }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(agent_apply_count.load(Ordering::SeqCst), 2);

        let response = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/surface/action"))
            .header(header::AUTHORIZATION, format!("Bearer {mcp_token}"))
            .json(&serde_json::json!({
                "name": "human_write",
                "args": { "text": "widened" },
                "as": "human"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(human_apply_count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[test]
    fn human_route_cookie_follows_human_actions_not_action_route_adapters() {
        let action = |audience| {
            ToolDef::new(
                "write",
                "Write once.",
                serde_json::json!({ "type": "object", "properties": {} }),
                |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
            )
            .audience(audience)
        };
        let surface = FocusSurface {
            state: FocusState,
            tools: vec![action(ActionAudience::Human)],
        };
        assert!(surface.action_routes().is_empty());
        assert!(needs_human_route_cookie(&surface));

        let agent_only = FocusSurface {
            state: FocusState,
            tools: vec![action(ActionAudience::Agent)],
        };
        assert!(!needs_human_route_cookie(&agent_only));
    }

    #[test]
    fn unknown_caller_identity_is_never_a_human_or_agent() {
        assert_eq!(Caller::parse(None), Caller::Human);
        assert_eq!(Caller::parse(Some("human")), Caller::Human);
        assert_eq!(Caller::parse(Some("companion")), Caller::Companion);
        assert_eq!(Caller::parse(Some("humna")), Caller::Unknown);
        assert_eq!(Caller::parse(Some("")), Caller::Unknown);
        for audience in [
            ActionAudience::Human,
            ActionAudience::Agent,
            ActionAudience::Both,
        ] {
            assert!(!Caller::Unknown.may_call(audience));
        }
    }

    #[test]
    fn embedded_browser_core_stays_small_and_ui_agnostic() {
        assert!(
            AGUI_CLIENT_JS.len() < 12 * 1024,
            "shared browser core grew to {} bytes; move capability code into an extension",
            AGUI_CLIENT_JS.len()
        );
        assert!(AGUI_CLIENT_JS.contains("loadExtensions"));
        assert!(AGUI_CLIENT_JS.contains("/surface/action"));
        assert!(!AGUI_CLIENT_JS.contains("board-form"));
        assert!(!AGUI_CLIENT_JS.contains("navigator.gpu"));
    }

    /// Rendering markup is only half a tier. A page made of real `<form>`
    /// elements posts form-encoded, and until the glue decoded that, every
    /// submit on a server-rendered page came back `400 invalid JSON body` —
    /// a UI you could look at and not use.
    #[tokio::test]
    async fn a_real_form_post_reaches_a_route_the_same_way_json_does() {
        fn echo_route() -> Arc<RouteDef> {
            Arc::new(RouteDef {
                method: HttpMethod::Post,
                path: "/echo",
                handler: Box::new(|request: RouteRequest| {
                    Box::pin(async move {
                        RouteResponse::json(200, request.body.unwrap_or(JsonValue::Null))
                    })
                }),
            })
        }
        async fn post(content_type: &'static str, body: &'static str) -> JsonValue {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            let response = passthrough_route(
                echo_route(),
                HashMap::new(),
                headers,
                axum::body::Bytes::from_static(body.as_bytes()),
            )
            .await;
            let bytes = axum::body::to_bytes(response.into_body(), 8192)
                .await
                .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        // What a browser actually sends, percent-encoding and all.
        let form = post(
            "application/x-www-form-urlencoded",
            "kind=claim&text=a+%22quoted%22+claim%20%26+more",
        )
        .await;
        assert_eq!(form["kind"], "claim");
        assert_eq!(
            form["text"], "a \"quoted\" claim & more",
            "percent-encoding and + must be decoded, or the text is mangled"
        );

        // Browsers append the charset; matching the full header would miss it.
        let with_charset = post(
            "application/x-www-form-urlencoded; charset=UTF-8",
            "kind=question",
        )
        .await;
        assert_eq!(with_charset["kind"], "question");

        // A handler cannot tell which encoding it arrived on — that is the
        // point. One route serves a fetch() and a form submit identically.
        let json = post("application/json", r#"{"kind":"claim"}"#).await;
        assert_eq!(json["kind"], "claim");
    }

    /// A surface that serves its own markup is a supported shape, not a
    /// workaround. This proves the whole path: the body reaches the wire
    /// verbatim, under a content type a browser will actually parse as HTML,
    /// and the JSON tier is untouched beside it.
    #[tokio::test]
    async fn a_surface_route_can_serve_its_own_markup() {
        let markup = "<ul><li>rendered by the server</li></ul>";
        let html = surface_route_response("/page", RouteResponse::html(200, markup));
        assert_eq!(html.status(), StatusCode::OK);
        assert_eq!(
            html.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8",
            "a body served as application/json is a body the browser renders as text"
        );
        let body = axum::body::to_bytes(html.into_body(), 4096).await.unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            markup,
            "markup must arrive byte-for-byte; nothing re-encodes it"
        );

        // The default tier is unchanged.
        let json = surface_route_response("/data", RouteResponse::json(201, json!({"ok": true})));
        assert_eq!(json.status(), StatusCode::CREATED);
        assert_eq!(
            json.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );

        let text = surface_route_response("/plain", RouteResponse::text(200, "read back"));
        assert_eq!(
            text.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );

        // An invalid status is still refused whatever the body shape is, so the
        // new tier cannot smuggle one past the check the JSON tier has.
        let bad = surface_route_response("/page", RouteResponse::html(999, "<p>x</p>"));
        assert_eq!(bad.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Pull every SCREAMING_SNAKE string literal out of a shipped browser
    /// asset. Those are AG-UI event names; nothing else in these files is
    /// spelled that way, and the count assertion below fails loudly if that
    /// ever stops being true.
    fn event_names_in(source: &str) -> Vec<String> {
        source
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|token| {
                token.contains('_')
                    && token.starts_with(|ch: char| ch.is_ascii_uppercase())
                    && token
                        .chars()
                        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            })
            .map(str::to_string)
            .collect()
    }

    /// The shipped browser assets name AG-UI events as bare strings, while
    /// `ag_ui_core::EventType` is what those names actually mean. Nothing made
    /// the two agree: rename a variant in Rust and the page goes on listening
    /// for an event that no longer exists — silently, at runtime, in the one
    /// place nobody is watching.
    ///
    /// This does not make the browser half typed. Whether the shell client
    /// should be Rust at all is still open (`docs/ag-ui-surface-spec.md`, "One
    /// client contract, or per-Surface clients?" — recorded as needing
    /// validation and never validated). It does mean that until that is
    /// settled, a rename breaks the build instead of the page.
    #[test]
    fn shipped_browser_assets_only_name_events_the_core_still_defines() {
        let mut checked = 0;
        for asset in [AGUI_CLIENT_JS, CONVERSATION_JS, PROVIDER_SETTINGS_JS] {
            for name in event_names_in(asset) {
                serde_json::from_value::<ag_ui_core::event::EventType>(JsonValue::String(
                    name.clone(),
                ))
                .unwrap_or_else(|_| {
                    panic!(
                        "a shipped browser asset listens for {name:?}, which \
                         ag_ui_core::EventType no longer defines"
                    )
                });
                checked += 1;
            }
        }
        assert!(
            checked >= 4,
            "found only {checked} event names in the shipped assets; the \
             extractor has stopped matching and this test is now proving nothing"
        );
    }

    /// The conversation panel is chrome the runtime lends every app. It must
    /// stay chrome: the moment it knows what a *particular* surface holds, the
    /// apps start forking it again and the lift is undone.
    #[test]
    fn the_conversation_stays_chrome_and_knows_no_surface() {
        assert!(CONVERSATION_JS.contains("agui-conversation"));
        assert!(CONVERSATION_JS.contains("customElements.define"));
        // It speaks the protocol's own vocabulary and nothing above it.
        for protocol in ["TEXT_MESSAGE_START", "surface.ask", "surface.tutor"] {
            assert!(CONVERSATION_JS.contains(protocol), "missing {protocol}");
        }
        // ...and none of any app's.
        for app_specific in ["board-form", "atlas", "navigator.gpu", "vellum", "canvas"] {
            assert!(
                !CONVERSATION_JS.contains(app_specific),
                "the shared conversation must not know about {app_specific:?}"
            );
        }
        // Same CSP shape as the settings element: linked stylesheet, no inline
        // <style>, themeable from outside the shadow root.
        assert!(CONVERSATION_JS.contains("/_agui/conversation.css"));
        assert!(!CONVERSATION_JS.contains("<style>"));
        assert!(CONVERSATION_CSS.contains(":host"));
        assert!(RUNTIME_ROUTE_PATHS.contains(&"/_agui/conversation.js"));
        assert!(RUNTIME_ROUTE_PATHS.contains(&"/_agui/conversation.css"));
    }

    #[test]
    fn provider_settings_styles_are_csp_compatible() {
        assert!(PROVIDER_SETTINGS_JS.contains("/_agui/provider-settings.css"));
        assert!(!PROVIDER_SETTINGS_JS.contains("<style>"));
        assert!(PROVIDER_SETTINGS_CSS.contains(":host"));
        assert!(RUNTIME_ROUTE_PATHS.contains(&"/_agui/provider-settings.css"));
    }

    #[test]
    fn extension_asset_preflight_rejects_missing_installed_files() {
        let root = std::env::temp_dir().join(format!(
            "ag-ui-extension-assets-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let static_dir = root.join("static");
        let pkg_dir = root.join("pkg");
        std::fs::create_dir_all(static_dir.join("extensions/board")).unwrap();
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(static_dir.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(
            static_dir.join("extensions/board/index.js"),
            "export default () => {};",
        )
        .unwrap();
        std::fs::write(pkg_dir.join("board.js"), "export default () => {};").unwrap();
        std::fs::write(pkg_dir.join("board.wasm"), []).unwrap();

        let manifest = ExtensionManifest {
            schema_version: 1,
            protocol: "ag-ui".to_string(),
            extensions: vec![ExtensionDescriptor {
                id: "board".to_string(),
                version: "1.0.0".to_string(),
                loading: "lazy".to_string(),
                module: Some("/extensions/board/index.js".to_string()),
                mount: "board".to_string(),
                actions: vec![],
                events: vec![],
                capabilities: vec!["wasm".to_string()],
                required: true,
                wasm: Some(WasmMount {
                    pkg_name: "board".to_string(),
                    js: "/pkg/board.js".to_string(),
                    wasm: "/pkg/board.wasm".to_string(),
                }),
            }],
        };
        assert_eq!(
            validate_static_directories(&static_dir, Some(&pkg_dir), &[]),
            Ok(())
        );
        assert_eq!(
            validate_extension_assets(&manifest, &static_dir, Some(&pkg_dir), &[]),
            Ok(())
        );

        std::fs::remove_file(pkg_dir.join("board.wasm")).unwrap();
        assert_eq!(
            validate_static_directories(&static_dir, Some(&pkg_dir), &[]),
            Ok(())
        );
        assert!(
            validate_extension_assets(&manifest, &static_dir, Some(&pkg_dir), &[])
                .unwrap_err()
                .contains("missing")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Leak-3 regression guard: `POST /semantic` must gate on the Surface's
    /// declared [`Surface::focus_events`], NOT on any `canvas.*` literal baked
    /// into the runtime. An undeclared or unresolvable event is rejected
    /// instead of being acknowledged and dropped.
    #[tokio::test]
    async fn semantic_handler_accepts_only_a_resolved_declared_focus() {
        let surface: Arc<dyn Surface> = Arc::new(FocusSurface {
            state: FocusState,
            tools: Vec::new(),
        });
        let rs = test_router_state(surface);

        // Declared event + a resolvable id → focus is set.
        let accepted = semantic_handler(
            axum::extract::State(rs.clone()),
            axum::Json(serde_json::json!({ "name": "test.point", "value": { "id": "known" } })),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK);
        assert!(
            rs.rt.current_focus.lock().is_some(),
            "a declared focus_event with a resolvable id must set current_focus",
        );

        // Reset, then fire the OLD hardcoded literal — same resolvable id, but
        // NOT in this Surface's focus_events(). It must fail loudly.
        *rs.rt.current_focus.lock() = None;
        let undeclared = semantic_handler(
            axum::extract::State(rs.clone()),
            axum::Json(serde_json::json!({ "name": "canvas.focus", "value": { "id": "known" } })),
        )
        .await;
        assert_eq!(undeclared.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            rs.rt.current_focus.lock().is_none(),
            "an event NOT in focus_events() must not mutate current focus",
        );

        let unknown_target = semantic_handler(
            axum::extract::State(rs.clone()),
            axum::Json(serde_json::json!({
                "name": "test.point",
                "value": { "id": "missing" }
            })),
        )
        .await;
        assert_eq!(unknown_target.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let malformed = semantic_handler(
            axum::extract::State(rs.clone()),
            axum::Json(serde_json::json!({ "name": "test.point", "value": {} })),
        )
        .await;
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

        // The reconnect-replay hook returns this Surface's own named events.
        assert_eq!(
            rs.surface.state().reconnect_events(),
            vec![("test.chrome".to_string(), serde_json::json!({ "k": "v" }))],
        );
    }
}
