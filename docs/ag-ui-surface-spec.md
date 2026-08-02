# AG-UI Surface: design spec

> **One line:** the reusable shell for "put an agent in your browser and talk to it," where the agent can *see and touch the same live surface you're looking at*: over any provider. AG-UI is the protocol. This is the application layer above it that everyone currently rebuilds by hand.

**Status:** implemented through M4.4. Teaching, Vellum, Colab, and the starter
compile against the runtime. Typed server composition and the pinned app recipe
are live, and the runtime owns the agent-facing MCP projection. Later
capability/package gates remain open.
**Packaging (decided):** a thin runtime crate + a cloneable template (Next.js / create-next-app split).
**Audience (decided):** internal reuse first, hardened for public later.

---

## 1. The problem: we're building the same app N times

Three apps in this repo are the *same shell* wearing different hats, plus a fourth partial:

| Instance | What it is | Server today |
|---|---|---|
| `examples/teaching-canvas` | live AI tutor drawing on a shared canvas | own `main.rs` (estimated 85k) |
| `examples/vellum-canvas` | collaborative architecture / decision tool | own `main.rs` |
| Colab (`~/.claude/skills/Colab`) | agent↔human visual iteration loop | own JS server |
| `crates/ag-ui-canvas-server` | demo server, used by nobody | standalone bin |

Every one of them re-implements an estimated 80%: a WS/SSE server speaking AG-UI events, a provider-agnostic agent loop, chat/voice, tool dispatch, and a shared-state document. Only the last estimated 20%, meaning *what's on the surface and what the agent can do to it*, is actually unique. That part is the product. The rest is tax we keep paying.

**Goal:** pay the estimated 80% once. Each use case becomes a small plug-in, not a new server.

---

## 2. The thesis: you have the protocol, you're missing the shell

- **Protocol** = AG-UI (`ag-ui-core` events: `STATE_SNAPSHOT`, `TEXT_MESSAGE_*`, `CUSTOM`, `TOOL_CALL_*`). Done. Not re-litigating this: the request is explicitly *not* a new protocol.
- **Shell** = the opinionated default runtime that turns the protocol into a working app. Missing. That's this project.

The runtime executes exactly one **Surface**. A single-capability app can
implement it directly. A multi-capability app implements typed **Extension**s
and lets `CompositeSurface` validate/adapt them into that one runtime seam.

---

## 3. Architecture

```
┌───────────────────────── browser ──────────────────────────┐
│  AG-UI client shell (wasm)  ·  chat/voice bar  ·  HITL UI    │
│         mounts Surface.client_modules() as lazy views       │
└───────────▲───────────────────────────────────┬────────────┘
            │ AG-UI events (WS/SSE)              │ tool calls · human drags
            │ STATE_SNAPSHOT · TEXT_MESSAGE_*    │   (tool.apply → Effect)
            │ CUSTOM · TOOL_CALL_*               │
┌───────────┴───────────────────────────────────▼────────────┐
│                  ag-ui-surface  (THE RUNTIME)               │
│                                                             │
│   transport (WS+SSE) · chat/voice                           │
│   opaque WS relay + snapshots/reconnect events              │
│   Surface owns CRDT/LWW/revision/ephemeral mechanics        │
│   Composite namespaces independently-owned child states     │
│   turn protocol: dispatch Effect::EmitAndAwait ↔ human      │
│   typed decision reply (choice or approval)                 │
│   HITL: barge-in · approve/reject · focus pointer           │
│   feedback: state/image read-back; app-owned visual diff    │
│   provider-agnostic agent loop:                             │
│        ACP (Claude/Gemini/Codex/opencode)                   │
│        OpenAI-compatible BYOK · subscription-direct         │
│   MCP: in-process Streamable HTTP `POST /mcp`               │
│   auth store (per-provider keys)                            │
│                                                             │
│  ─ seam: SurfaceState + Surface or typed Extensions ─────── │
│   SurfaceState: backing()  describe()  snapshot_png()       │
│                 snapshot()                                  │
│   Surface:      state()  tools()  client_modules()  store() │
│      tools() apply → Effect: Mutate / Commit / EmitAndAwait │
│      (NOT "edits the CRDT": see §4)                        │
└────────────────────────────▲────────────────────────────────┘
                             │ implements
      ┌───────────────────────┼───────────────────────┐
 CompositeSurface       ChoiceSurface           DomSurface        (…your Surface)
 (teaching-canvas)      (vellum-canvas)         (Colab)           spreadsheet · map ·
 board + canvas Exts.   ADRs/choice sets        dendrogram+iframe form · chart · code-review
 LWW + Crdt             state: Ephemeral        backing: LWW
                        optional jj store
```

The runtime owns everything generic. A direct Surface owns one use case.
Extensions keep independently enabled capabilities cohesive while
`CompositeSurface` preserves the same one-dispatcher runtime contract.

---

## 4. The seam: `Surface` and `Extension`

`Surface` remains the execution seam. Implement it directly for one cohesive
capability, or implement `Extension` several times and compose them before
handing the result to the runtime.

```rust
/// Descriptive metadata for snapshots/composition. The Surface owns every
/// backing-specific sync, mutation, and persistence mechanism.
pub enum StateBacking {
    /// True multi-writer convergence. yrs `Doc`. Canvas, co-editing.
    /// **Corrected v3 (M2 trait-review round, 2026-07-08):** the runtime does
    /// NOT run CRDT sync itself: that claim was aspirational and M2's real
    /// implementation contradicts it. The runtime's `/ws` route is a dumb
    /// subscribe-and-relay with zero yrs dependency; the actual sync
    /// protocol lives behind `SurfaceState::ws_hello`/`ws_receive` as opaque
    /// `Vec<u8>` frames a `Crdt`-backed Surface fills in and every other
    /// backing takes as empty defaults. Permanent design, not a stopgap :
    /// see `ag-ui-surface-trait-review.md` §6, Leak 2. "Awareness"
    /// (multi-cursor presence) is unimplemented by either layer today.
    Crdt,
    /// Single-writer authority. Surface-defined replacement/reconnect policy.
    LastWriterWins,
    /// Append-with-revision-history (jj/git). ADRs, decision logs.
    /// "Revise an early decision" = oplog rewind, NOT an edit. Store-owned
    /// commit/reload; no generic runtime merge.
    RevisionHistory,
    /// In-memory only; nothing durable survives a process restart.
    Ephemeral,
    /// Several independently-backed extension states, namespaced by id.
    Composite,
}

/// What the state IS: the live view both human and agent look at. `describe`
/// and `snapshot_png` live here (they're properties of the state, not the
/// surface). A Surface picks its `backing`; the runtime does NOT assume yrs.
pub trait SurfaceState: Send + Sync + 'static {
    /// Declarative metadata; the runtime does not branch on it.
    fn backing(&self) -> StateBacking;

    /// Text read-back: the agent's *structured* perception of the surface, in
    /// the agent's own coordinate space. Cheap, always available.
    /// (teaching-canvas: `describe_scene` → world-coord summary incl. drags.)
    fn describe(&self) -> String;

    /// Optional *pixel* perception: render the surface exactly as the human
    /// sees it: browser-free, deterministic, PNG bytes. Enables the visual
    /// feedback gate and image read-back to vision models. Boxed future, not
    /// `async fn`: `&dyn SurfaceState` is used as a trait object
    /// (`Surface::state()`), and native async-fn-in-trait isn't
    /// dyn-compatible without boxing; spelled out explicitly here rather
    /// than pulling in `async-trait` to box every method on the trait for
    /// the sake of this one (teaching-canvas: `render_scene_png` hops to a
    /// blocking thread for the wgpu render, hence async at all).
    /// Default `Box::pin(async { None })` = text-only.
    fn snapshot_png(&self) -> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + Send + '_>> {
        Box::pin(async { None })
    }

    /// Serialize for `STATE_SNAPSHOT` + reconnects. Crdt → yrs update;
    /// LastWriterWins → JSON; RevisionHistory → revision id + working-copy
    /// state; Ephemeral → JSON.
    fn snapshot(&self) -> StateSnapshot;

    /// Resolve an object id to a short human-readable description, for
    /// deixis ("point at something, then say 'this'"). Distinct from the
    /// full-scene `describe()`: this is one object, already worded for
    /// splicing into a preamble, not structured fields (kind/coords/color
    /// don't generalize past canvas: see trait-review round 2, Gap 2).
    /// (teaching-canvas: `resolve_focus` + the wording half of
    /// `focus_preamble`, folded together.) Default `None` = no focus support.
    fn resolve(&self, id: &str) -> Option<String> { None }

    /// **Added v3 (M2 trait-review round, 2026-07-08), Leak 2.** Opaque
    /// binary frames to send a freshly-connected client on the runtime's
    /// `/ws` transport, before live broadcast frames start flowing: a
    /// Surface's own wire protocol for resuming a live binary session
    /// (teaching-canvas: the yrs sync `greeting` frame + any live cloud-blob
    /// frames). Deliberately opaque bytes, not a typed CRDT method: see
    /// `ag-ui-surface-trait-review.md` §6, Leak 2, for why the runtime does
    /// NOT (and should not) inspect these. Default: no hello frames.
    fn ws_hello(&self) -> Vec<Vec<u8>> { Vec::new() }

    /// **Added v3**, paired with `ws_hello`. Apply one inbound binary frame
    /// from a `/ws` client, returning any reply frames to broadcast back.
    /// Default: ignore inbound frames entirely.
    fn ws_receive(&self, data: &[u8]) -> Vec<Vec<u8>> { Vec::new() }

    /// **Added v3, Leak 3.** Named SSE custom-event replay entries for a
    /// freshly-connected/reloading client, derived from this state's own
    /// UI-chrome: teaching-canvas: `[("canvas.chat", chat_config),
    /// ("canvas.widget", widget)]`. Distinct from `snapshot()`'s `chrome`
    /// field (the wire-payload byte shape); this is the same data
    /// reprojected as the individual named events a live session emits.
    /// Wired: the SSE handler iterates this (M5 follow-up, 2026-07-10;
    /// renamed from `chrome_events`: "chrome" = UI framing, not the browser).
    /// Default empty.
    fn reconnect_events(&self) -> Vec<(String, serde_json::Value)> { Vec::new() }
}

/// What a tool does to state. NOT assumed to be a CRDT edit. The runtime
/// dispatches on `Effect` so Vellum's `accept_adr` (a jj commit) and
/// `render_choice_set` (a turn handoff) are first-class, not forced through a
/// CRDT-shaped hole.
///
/// **Synced v3 to the real M2 implementation** (this enum had drifted from
/// `crates/ag-ui-surface/src/lib.rs`'s actual shape, grown during M2 Phase A
/// to express `apply_tool`'s three real reply shapes: see that file's
/// `Effect` doc comment for the full reasoning): a `Query` variant for pure
/// reads, and `Mutate`/`Commit` now reply-carrying (`Option<String>`), not
/// bare `()`.
pub enum Effect {
    Query(QueryEffect),
    AsyncQuery(AsyncQueryEffect),
    Mutate(MutateEffect),
    AsyncMutate(AsyncMutateEffect),
    Commit { message: String, apply: CommitEffect },
    /// Keyed suspend/resume. The runtime emits the event, validates a typed
    /// reply posted to `/decision`, and only then invokes `on_reply`.
    EmitAndAwait {
        id: String,
        event: CustomEvent,
        reply_kind: ReplyKind,
        on_reply: DecisionReply,
    },
    Reject(String),
}

/// The one thing a use case implements. Everything else: providers,
/// transport, chat, HITL, feedback: the runtime provides for free.
pub trait Surface: Send + Sync + 'static {
    /// The live view. For teaching-canvas this IS the state (Crdt). For Vellum
    /// this is the ephemeral projection; `store()` is the durable ledger.
    fn state(&self) -> &dyn SurfaceState;

    /// The effective action vocabulary after any recipe composition. Each
    /// `ActionDef` (`ToolDef` is a compatibility alias) declares its audience,
    /// JSON Schema, optional visual-result metadata, and an `apply` closure
    /// returning [`Effect`]. The runtime derives OpenAI and MCP schemas from
    /// agent-visible entries here; applications do not maintain a bridge
    /// schema or allowlist.
    fn tools(&self) -> &[ToolDef];

    /// Browser extensions that draw/interact with `state()`. The runtime
    /// validates these into `/extensions`; its shared client imports lazy
    /// same-origin entries before opening the event stream.
    fn client_modules(&self) -> Vec<ClientModule>;

    /// The durable projection. For teaching-canvas: peripheral learner model
    /// (None or small). For Vellum, the PRIMARY state is the ADR log and
    /// `state()` is its live projection. Optional by capability, NOT by
    /// priority: an entire class of decision/ledger apps inverts the default.
    fn store(&self) -> Option<&dyn SurfaceStore> { None }

    /// Extra HTTP routes this Surface needs beyond the tool-call vocabulary :
    /// browser-side hydrate/read endpoints with no agent turn involved
    /// (teaching-canvas: `GET/POST /mission`; Vellum: `GET /artifacts`).
    /// Framework-neutral by design: the runtime mounts these as real axum
    /// routes internally, but the trait never names axum, so a Surface's
    /// `Cargo.toml` never has to either. Default empty: most Surfaces need
    /// nothing here, everything goes through `tools()`.
    fn routes(&self) -> Vec<RouteDef> { Vec::new() }

    /// **Added v3 (M2 trait-review round, 2026-07-08), Leak 3.** The `POST
    /// /semantic` event names this Surface treats as a pointing gesture
    /// worth resolving into a focus (see `SurfaceState::resolve`):
    /// teaching-canvas: `["canvas.focus", "canvas.selected",
    /// "canvas.dragged"]`. Default empty: no pointing vocabulary declared.
    fn focus_events(&self) -> &[&str] { &[] }

    /// CompositeSurface overrides this to route only to the owning extension.
    fn resolve_focus(&self, event: &str, id: &str) -> Option<String>;
}

/// One complete, independently enabled server capability. CompositeSurface
/// validates several of these and adapts them into the one runtime Surface.
pub trait Extension: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn state(&self) -> &dyn SurfaceState;
    fn actions(&self) -> &[ToolDef];
    fn client_module(&self) -> Option<ClientModule>;
    fn capabilities(&self) -> &[&str];
    fn routes(&self) -> Vec<RouteDef> { Vec::new() }
    fn focus_events(&self) -> &[&str] { &[] }
    fn binary_transport(&self) -> bool { false }
}

/// One Surface-contributed HTTP route (see `Surface::routes()`). Plain JSON
/// in, JSON out: the only shape teaching-canvas's `GET/POST /mission` and
/// Vellum's `GET /artifacts` actually need; no multipart, no SSE, no upgrade.
pub struct RouteDef {
    pub method: HttpMethod,
    pub path: &'static str,
    /// Boxed future for the same dyn-compatibility reason as
    /// `SurfaceState::snapshot_png()`: see that method's doc comment.
    pub handler: Box<dyn Fn(RouteRequest) -> Pin<Box<dyn Future<Output = RouteResponse> + Send>> + Send + Sync>,
}

pub enum HttpMethod { Get, Post, Put, Delete }

pub struct RouteRequest {
    pub query: std::collections::HashMap<String, String>,
    pub body: Option<serde_json::Value>,
}

pub struct RouteResponse {
    pub status: u16,
    pub body: serde_json::Value,
}
```

`CompositeSurface` rejects duplicate ids/actions/routes/focus events, metadata
drift, core-route collisions, and more than one owner of the unnamespaced raw
WebSocket path. It namespaces child state in `/surface/state`, joins reconnect
events, and still exposes one flat action list to the existing dispatcher.

Design notes:
- The runtime does not assume yrs or select behavior from [`StateBacking`]. It relays opaque `ws_hello`/`ws_receive` frames, transports snapshots/reconnect events, and dispatches typed effects uniformly. Each Surface owns its actual CRDT, replacement, revision-store, or ephemeral mechanics. `Composite` only namespaces independently-owned child snapshots. The enum documents ownership for clients/composition rather than acting as a runtime strategy switch.
- `tools()`'s `apply` returns an [`Effect`], not "an edit to the CRDT." `Mutate` covers canvas draws and form sets. `Commit` covers Vellum's `accept_adr` (a jj change). `EmitAndAwait` covers `render_choice_set` → human `OptionChosen` (the turn-protocol primitive). Forcing all three through a CRDT-shaped `apply` is the leak this revision fixes.
- `ActionDef::audience` is discovery metadata, not a second dispatcher. `Human`, `Agent`, and `Both` decide whether an action appears in the browser manifest, agent schemas, or both. Either actor still reaches the same `apply` closure. `ToolDef` remains only as a compatibility alias while call sites migrate.
- Visual results are opt-in metadata. `.with_state_snapshot()` marks the actions whose successful model-facing result should include the current `SurfaceState::snapshot_png()` when one exists. A query is not assumed to be visual: `read_board` remains text-only while `read_canvas` opts in.
- `ag-ui-surface` owns the in-process Streamable HTTP MCP endpoint at `POST /mcp`. Its `tools/list` result is generated from the agent-visible portion of the effective `Surface::tools()` after `CompositeSurface` applies `agui.app.toml`. `tools/call` reaches the same runtime dispatcher as `/surface/action`. ACP adapters that advertise HTTP MCP receive that endpoint during session setup. `App::mcp_bridge(McpBridge)` remains only as an optional custom stdio fallback for an adapter without HTTP MCP support.
- `store()` is optional by capability, not by priority. teaching-canvas: `state()` is the state, `store()` is a peripheral learner model. Vellum **inverts** this: `store()` (the jj ADR log) is the primary state and `state()` is the ephemeral projection, re-synced from the working copy on reload. The trait must not encode "canvas is primary, store is peripheral": that's teaching-canvas's shape, not a universal.
- `describe()` and `snapshot_png()` are the two halves of *perception*. Text is mandatory and cheap. Pixels are optional and powerful. Having both is what makes these agents actually work instead of guessing. They live on `SurfaceState` because they're properties of the state, not the surface.
- Why this matters now (not at M5): the flagship app, teaching-canvas, runs `Scene` under a `Mutex` and rejects client writes (`"client blob write rejected (single-writer)"`). The CRDT machinery has passing tests but is not load-bearing as multi-writer in the app today. M1 extracting "CRDT sync (yrs)" as the default crown jewel would bake in an assumption even teaching-canvas does not exercise, and that Vellum actively contradicts. Full reasoning: `ag-ui-surface-trait-review.md`.
- **v2 (M1 trait-review round, 2026-07-08):** four gaps surfaced grounding the deferred M2 turn loop against the real trait (`ag-ui-surface-m2-handoff.md` §5): closed here, full reasoning in `ag-ui-surface-trait-review.md` §5:
  - `Surface::routes()`: Surface-contributed HTTP routes (mission GET/POST, Vellum's `GET /artifacts`), framework-neutral (`RouteDef`), default empty.
  - `SurfaceState::resolve(id) -> Option<String>`: deixis/focus: resolve an object id to a worded description, distinct from the full-scene `describe()`. The generic "[Pointing context]" wrapper sentence and TTL clock stay in the runtime's `Hitl` machinery, not the trait.
  - `snapshot_png()` is now `-> Pin<Box<dyn Future<Output = Option<Vec<u8>>> + Send + '_>>` (was sync): `render_scene_png` is genuinely async (blocking-thread GPU hop). Hand-rolled boxed future rather than `async-trait`, so only this one method boxes, not `describe()`/`snapshot()` too, and no new dependency.
  - The vision feedback re-injection policy (`openai.rs:255-272`, OpenAI-only prune-and-reinject. ACP never touches this path) got **no trait change**: deliberately kept out of `Surface`/`SurfaceState` as backend/transport policy, not a Surface concern. See trait-review round 2, Gap 3, for why a `VisionPolicy` enum lives beside the runtime's OpenAI backend adapter instead.
- **v3 (M2 trait-review round, 2026-07-08):** M2's GO/NO-GO passed (teaching-canvas runs entirely on `ag-ui-surface`), but shipping it for real surfaced four more seam leaks: closed/specified here, full reasoning in `ag-ui-surface-trait-review.md` §6:
  - `App::surface()` now takes a `FnOnce(Transport) -> S` factory (a `Transport { ws_tx, sse_tx, history, awaiting, transcript_replay_lock }` bundle), replacing the old `App::transport()`-then-`.surface()` two-step whose ordering was enforced only by a doc comment. The shared awaiting flag and replay mutex make live tutor state and reconnect snapshots one ordered boundary. `App::mission()` folded into `App::prompt(impl Into<Prompts>)`.
  - The `StateBacking::Crdt` "runtime runs CRDT sync + awareness" claim above was **wrong**. The corrected contract makes the Surface own wire sync entirely via opaque `SurfaceState::ws_hello`/`ws_receive` `Vec<u8>` frames. The runtime's `/ws` is a dumb subscribe-and-relay with zero yrs dependency. Ratified as the permanent design, not a stopgap.
  - `Surface::focus_events() -> &[&str]` and `SurfaceState::reconnect_events() -> Vec<(String, Value)>` (the latter renamed from `chrome_events`) let a Surface declare its own pointer-event vocabulary and UI-chrome-replay shape instead of the runtime's `/semantic` handler and SSE chrome-replay hardcoding teaching-canvas's `canvas.*` strings. Signatures landed M2. Both handlers wired to call them in the M5 follow-up (2026-07-10), closing Leak 3.
  - `Effect::EmitAndAwait` is now keyed and fully implemented: the async dispatcher registers a typed pending decision, emits it, waits for validated `POST /decision` input, and invokes `on_reply`. Vellum and Colab exercise choice and approval replies, including concurrent named waits and reconnect replay.

---

## 5. The runtime API: what you get for free

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    App::new()
        // Your 20%. A FACTORY, not a built Surface: receives a `Transport`
        // (second handles onto the runtime's WS/SSE/history channels) so
        // your state constructor can hold them for its own chrome, no
        // separate pre-call needed (v3, corrected: see §4).
        .surface(|transport| MySurface::new(transport))
        .providers(Providers::from_file("models.json")?) // ACP + BYOK + subscription
        .prompt(Prompts::new(include_str!("../prompt.md"), mission_prompt)) // persona + mission framing, one call (v3)
        .voice(true)                                   // server-side speak
        .hitl(Hitl { barge_in: true, decisions: true, focus: true })
        .serve("127.0.0.1:8090")
        .await?;
    Ok(())
}
```

A Surface with no mission-framing needs (Vellum, Colab) just writes `.prompt(include_str!("../prompt.md"))`: a bare string still works via `Prompts: From<&str>`.

Everything in that builder except `.surface()` and `.prompt()` is provided by the runtime and shared across every use case. Crown jewels: the parts nobody wants to rebuild:

- **Provider-agnostic agent + auth**: ACP, OpenAI-compatible BYOK, subscription-direct, per-provider key store. Extracted from what teaching-canvas already ships.
- **One agent action contract**: the runtime projects agent-visible `ActionDef`s into OpenAI tools and in-process MCP at `POST /mcp`, then dispatches both through the same `Effect` path. ACP negotiates the HTTP endpoint, with an optional custom stdio fallback for older adapters.
- **Surface-owned shared state**: the runtime transports opaque WS frames and reconnect snapshots. Each Surface implements its declared CRDT, replacement, revision-store, or ephemeral semantics. `Composite` namespaces child snapshots without inventing a shared backing.
- **The perceive-your-output loop**: the differentiator. Most "AI in your app" is a blind chat sidebar. This agent reads the surface back (text always, pixels when available) and can gate on whether its own action took effect.

---

## 6. What extracts from where (this is not vapor)

Every runtime capability already exists in `teaching-canvas`: extraction, not invention:

> **Note (M1, 2026-07-08):** this table originally conflated "provider layer + auth" with "agent turn loop" as one row. They're not the same extraction: providers/auth extract verbatim with no other coupling. The turn loop (`openai.rs`/`acp.rs`) is welded to teaching-canvas-only code (`apply_tool`, `describe_scene`, `render_scene_png`, `memory::Workspace`) and does not move verbatim. Split below. Full coupling inventory: `ag-ui-surface-m2-handoff.md`.

| Runtime capability | Comes out of |
|---|---|
| Provider layer + auth | `providers.rs`, `models.json`, `auth.rs`: **M1 DONE, verbatim** (re-exported as `ag_ui_surface::{providers, auth}`) |
| Agent turn loop + `openai.rs`/`acp.rs` adapters | `openai.rs`, `acp.rs`, `main.rs` (turn loop): **M2** (coupled to the surface via tools/render/memory. See `ag-ui-surface-m2-handoff.md` for the full symbol-level map) |
| MCP projection (ACP → actions) | Runtime-owned `POST /mcp`, generated from the effective agent-visible `Surface::tools()` after recipe composition. App-local bridge binaries and schema allowlists are gone. Custom `McpBridge` is only an optional stdio fallback. |
| CRDT scene + WS sync + AG-UI events | `main.rs` + `crates/ag-ui-canvas`, `crates/ag-ui-core` |
| Text read-back | `describe_scene` (`main.rs`) |
| Pixel render (browser-free) | `render.rs` (`RenderService`, `png_to_data_uri`) |
| Barge-in | `main.rs:102` |
| Visual diff gate | **NEW**: port Colab's pixelmatch over `snapshot_png` |
| Decisions / focus pointer | **NEW**: port from Colab (`browser-colab`, `colab-focus`) |
| Reference browser client shell | `crates/ag-ui-canvas-web` |

Stays inside the Surface (an estimated teaching-canvas-specific 20%): the draw tools (`plot_points`, `render_diagram`, `axes` in `tools.rs`), the tutor prompt, the learner model (`memory.rs`), the canvas renderer specifics.

---

## 7. The template (`create-ag-ui-app`)

**Built at `examples/create-ag-ui-app` (M4/M4.2, 2026-07-11).** A newcomer clones this and gets a working agent-in-browser app. The recipe selects two real typed extensions (workspace goal + sticky-note board), each with its own state/actions/events/browser module.

```
create-ag-ui-app/
  Cargo.toml            → depends on ag-ui-surface (+ tokio/serde_json/tracing)
  agui.app.toml         → pinned local extension recipe          ◄── EDIT
  src/main.rs           → CompositeSurface::from_recipe(..)
                          + App::new().surface(..).providers(..).auth(..)
                          .prompt(..).static_dir(..).serve(..)   (~30 lines, don't touch)
  src/surface.rs        → impl Extension for BoardExtension      ◄── EDIT
  src/workspace.rs      → second state/actions/route Extension   ◄── EDIT
  src/tools.rs          → your tool vocabulary               ◄── EDIT
  prompt.md             → your agent's mission               ◄── EDIT
  web/index.html        → app chrome + empty extension mount
  web/extensions/...    → manifest-loaded browser view       ◄── EDIT
  models.json           → optional OpenAI-compatible provider presets
```

Ships with a shared-goal Extension plus a **sticky-note board** Extension
(DOM-native, no wasm, no MCP subprocess), selected by `agui.app.toml`. The
recipe can only select locally compiled implementations. It cannot fetch code.
The starter needs neither `App::mcp_bridge()` nor `App::pkg_dir()`. The runtime
owns `/mcp` for any ACP provider enabled later. The former builder hook is only
for a custom stdio compatibility bridge. No WASM bundle is mounted unless an
enabled extension needs one.

---

## 8. How the three existing instances collapse

The proof the seam is right: each app becomes a thin Surface, not a server.

- **teaching-canvas → `CompositeSurface(BoardExtension, CanvasExtension)`**: the recipe independently selects the LWW DOM board and the CRDT/WASM/WebGPU canvas. Canvas objects, draw tools, tutor prompt, learner store, mission routes, focus events, and raw binary transport remain owned by `CanvasExtension`. The wgpu renderer backs its `snapshot_png`, and `describe_scene` backs its `describe`.
- **vellum-canvas → `ChoiceSurface`**: choice-sets/ADR objects + `render_choice_set`/pick actions + architect prompt + optional jj artifact store. Its live state is ephemeral. When configured, `store()` separately supplies revision-backed durability. This is the instance that proved state projection and durable action effects cannot be collapsed into a CRDT edit.
- **Colab → `DomSurface`**: `data-feature` dendrogram + isolated preview origin. `set_focus`/approve/reject as actions + HITL. Capture and pixel diff remain Colab-owned async effects while the runtime owns dispatch, typed waits, and image/state seams.

If any of these needs a runtime hack to fit, the seam is wrong and we fix the trait before generalizing.

---

## 9. Milestones

- **M0: DONE.** Surface API and state/effect shape agreed and implemented. `Surface` is split into `SurfaceState + Surface`. `StateBacking` has five descriptive variants. Actions return typed `Effect`s.
- **M1: extract the runtime. DONE, with a scope note.** New `ag-ui-surface` crate: trait stubs verbatim from §4, `App` builder skeleton, `surface_shim()` seam. Providers + auth extracted **verbatim** (`providers.rs`/`auth.rs` → `ag_ui_surface::{providers, auth}`, re-exported. Workspace builds clean). Transport/agent-loop/MCP-bridge did **not** move: they can't move verbatim (`openai.rs`/`acp.rs` are welded to teaching-canvas's `apply_tool`/`describe_scene`/`render_scene_png`/`memory::Workspace`), so per Mike's "clear handoff" decision (2026-07-08) that work is deferred to M2 rather than forced through a fat shim. `ag-ui-canvas-server` demo reconciliation also deferred to M2. Full symbol-level coupling map + M2 task list: `ag-ui-surface-m2-handoff.md`.
- **M2: DONE.** teaching-canvas runs on the generic Surface runtime with live tutor, read-back, barge-in, providers, reconnect, and WebGPU rendering preserved.
- **M3: DONE.** Feedback and HITL are runtime-owned: visual snapshots, typed keyed decisions, focus, interrupt, reconnect replay, and shared action validation are implemented.
- **M4: the template. DONE (2026-07-11).** `examples/create-ag-ui-app` is a DOM-native `BoardSurface` with `add_note`/`clear_board`/`read_board` and a generic OpenAI-compatible BYOK default. At this milestone, apps without `.mcp_bridge()` hid ACP providers they could not execute. M4.4 removes that limitation. `.pkg_dir()` remains optional for a no-WASM surface. `Transport::emit(name, value)` is the Surface-owned live-event seam. Assistant output uses standard AG-UI text-message lifecycle events, and human board controls invoke the same `ToolDef`/`Effect` dispatcher as model tool calls through `POST /surface/action` (`/canvas-tool` remains an alias). Provider endpoint/model settings persist beside the key without overwriting it. The deterministic tool→state→event→reconnect loop is covered by `tools_drive_the_board_and_emit_surface_events`. Streaming and credential persistence have focused regression tests. Live DeepSeek proof on the repaired build: one human action appeared in server read-back, the model read it and issued two real `add_note` calls across a three-step streamed turn, the browser observed exactly one new assistant bubble, and a second tab replayed history/state then broadcast its own note back to the first.
- **M4.1: manifest-driven browser extensions. DONE (2026-07-11).** `Surface::client_modules()` returns one or more validated descriptors. The runtime exposes the active allowlist at `/extensions` and one embedded protocol/loader module at `/_agui/client.js`. Board and WebGPU canvas views are lazy same-origin modules loaded before SSE reconnect replay. Vellum and Colab remain explicitly `loading: "host"`, not falsely marked migrated. Package-marketplace versus runtime-manifest boundaries, profiles, and size budgets are specified in `ag-ui-extension-architecture.md`.
- **M4.2: typed server composition + app recipe. DONE (2026-07-11).** `Extension` owns one capability's state, actions, routes, focus events, reconnect behavior, and optional browser module. `CompositeSurface` validates and flattens an enabled set into the runtime's existing one-Surface/one-dispatcher contract, with namespaced composite read-back and an explicit one-owner gate on the unnamespaced raw WebSocket path. Strict `agui.app.toml` pins locally compiled ids, versions, capabilities, and required policy. It never fetches code. The starter now composes real `workspace` and `board` extensions rather than hand-flattening browser modules into one `BoardSurface`.
- **M4.3: optional DOM + WASM/WebGPU composition. DONE (2026-07-12).** teaching-canvas now composes a real LWW `BoardExtension` with the migrated CRDT `CanvasExtension` from one `agui.app.toml`. Recipe-disabled extensions are not constructed and contribute no actions, state, routes, browser modules, package mount, or canvas MCP bridge. Their asset URLs return 404. At that gate, the app-local ACP bridge received the same recipe-derived action allowlist, advertised the board and canvas schemas it could execute, rejected out-of-recipe calls, forwarded through `/surface/action`, and returned real query output. M4.4 replaces that seam with runtime-owned MCP. Contract tests cover both, board-only, and canvas-only selection from the same recipe. Live Chrome proof covered the DOM form → shared dispatcher → board state/read-back loop, canvas-only WASM fetch + real WebGPU pixels + CRDT read-back, and the combined manifest with both extensions ready in one unchanged host. A live JSON-RPC bridge probe proved `add_note` and `read_board` through the ACP path.
- **M4.4: runtime-owned agent schemas + MCP. DONE (2026-07-12).** `ActionDef` makes `Human`/`Agent`/`Both` discovery and optional `.with_state_snapshot()` visual results explicit. `ToolDef` remains a compatibility alias. After recipe composition, `ag-ui-surface` filters the effective `Surface::tools()` once and derives both OpenAI tools and MCP `tools/list` from the agent-visible actions. The runtime serves authenticated Streamable HTTP MCP in process at `POST /mcp`, dispatches `tools/call` directly through the shared `Effect` path, and negotiates that HTTP capability with ACP adapters. It also reserves `/mcp` against extension-route collisions and enforces the MCP version selected during initialization. A caller-supplied `McpBridge` remains an optional stdio fallback only. Teaching, Vellum, and Colab app-local bridge binaries and handwritten allowlists are removed, and none of the current examples requires `.mcp_bridge()`. Gate proof: 61 focused tests cover exact audience/schema projection, recipe-selected action inventories, real mutation/query/rejection, opt-in image results, independent concurrent waits, route ownership, protocol/version negotiation, transport negotiation, and all four example apps. `cargo check --workspace --all-targets` passes. With strict version-header validation enabled, a live Claude ACP session advertised runtime HTTP MCP, reached ready without a sidecar, used the runtime tool path to add the exact note `MCP_VERSION_GATE_20260712`, and returned that mutation through the server's real board read-back. Runtime probes confirmed unauthenticated MCP returns 401 and a hostile browser Origin returns 403. The authenticated `GET` route is explicitly 405. Focused checks also reject unknown actions and find no legacy bridge symbols, extra app binary targets, or leaked app/ACP child processes. The only workspace-check warning remains the pre-existing unused `ag-ui-wasm-client::to_js` helper.
- **M4.5: authoritative action-input schemas. DONE (2026-07-12).** `App::serve` now meta-validates and compiles every effective `ActionDef` schema before bind, not merely the agent-visible MCP projection. Schemas default explicitly to JSON Schema 2020-12 when `$schema` is absent. Supported explicit dialects are honored, while invalid roots/schemas, duplicate names, unsupported dialects, and unresolved external references fail startup. The shared dispatcher validates arguments exactly once before the sole production `ActionDef::apply` call, so browser, OpenAI, and MCP origins cannot bypass the same contract. Malformed OpenAI argument JSON is no longer coerced to `{}`. Focused tests prove invalid human and MCP calls never reach mutation, valid calls apply once, nested/range/additional-property rules execute, and human-only invalid schemas fail startup compilation. All four applications reached their real listener with their complete action inventory compiled. `cargo test --workspace --all-targets` and `cargo check --workspace --all-targets` pass, with only the pre-existing unused `ag-ui-wasm-client::to_js` warning.
- **M5: DONE (2026-07-13).** Vellum (`ChoiceSurface`) and Colab (`DomSurface`) are real consumers. Vellum proves durable ADR commits plus typed choice waits. Colab proves concurrent approval waits, browser capture/diff, focus persistence, and an isolated sandboxed preview origin.

---

## 10. Open questions / risks (the honest part)

1. **Surface seam: RESOLVED.** Teaching, Vellum, Colab, and the starter all use the same runtime-owned transport/action/provider path while retaining their own state and renderer contracts.
2. **One client contract, or per-Surface clients?** Today there are two browser clients: `ag-ui-wasm-client` (generic SSE+fetch, Colab uses it) and `ag-ui-canvas-web` (canvas, teaching-canvas uses it). Lean: the runtime owns the *shell* client (chat/HITL/transport). Each Surface supplies only its *surface-renderer* module mounted into the shell. Needs validation.
3. **Auth genericization for "open later."** The `auth.json` (0600) store is fine internally. Public use needs env + pluggable secret store, no hardcoded keychain assumptions.
4. **One backing, or many?: RESOLVED (M0).** `StateBacking` declares five descriptive shapes: `Crdt`, `LastWriterWins`, `RevisionHistory`, `Ephemeral`, and `Composite`. The runtime does not select sync/turn behavior from them. Surfaces own those mechanics and Effects describe execution. Vellum makes the distinction concrete: ephemeral live choice state can coexist with an optional revision-backed store and emit-and-await turn protocol.
5. **Provider matrix drift.** ACP/BYOK/subscription auth is high-value but high-maintenance. It's the thing most likely to rot. Treat `models.json` + `providers.rs` as the contract and test it.
6. **`RouteDef` JSON shape: RESOLVED.** Teaching mission routes, Vellum artifacts, and Colab focus/tree/preview-config routes all fit the bounded JSON contract. Colab's file preview is intentionally not a `RouteDef` or privileged static mount. A separate loopback preview server owns its strict file allowlist and CSP.
7. **`App` builder construction ordering: RESOLVED (M2 trait-review round, 2026-07-08).** M2 Phase D had to add `App::transport()`, a getter that had to be called BEFORE `.surface(...)`, with the ordering enforced only by a doc comment. `App::surface()` now takes a `FnOnce(Transport) -> S` factory closure instead of a pre-built `Surface`, making the ordering structural rather than documented. See `ag-ui-surface-trait-review.md` §6, Leak 1.
8. **CRDT sync ownership: RESOLVED, by fixing the claim, not the code.** §4/§5 above used to say "runtime runs CRDT sync + awareness" for `StateBacking::Crdt`. M2's real implementation (`SurfaceState::ws_hello`/`ws_receive`, opaque `Vec<u8>` frames) already did the honest thing. The runtime never touches yrs, and the spec's prose had not caught up. Corrected throughout this document. See `ag-ui-surface-trait-review.md` §6, Leak 2.
9. **Surface event vocabulary: RESOLVED (M5 follow-up, 2026-07-10).** `Surface::focus_events()`/`SurfaceState::reconnect_events()` (the latter renamed from the ambiguous `chrome_events`, where "chrome" means UI framing rather than the Chrome browser) are now wired. `semantic_handler` gates on `st.surface.focus_events().contains(&name)` and `sse_handler` iterates `surface.state().reconnect_events()`, so no `canvas.*` literal survives in generic runtime code. teaching-canvas declares its own vocabulary (`canvas.focus`/`canvas.selected`/`canvas.dragged`, `canvas.chat`/`canvas.widget`). Vellum and Colab return empty and use routes/`snapshot().chrome` instead. Caveat (honest): the three proofs all happen to share teaching-canvas's `canvas.*` names, so a genuinely *different* deixis vocabulary is unexercised end-to-end. The runtime is now vocabulary-agnostic, proven by `semantic_handler_gates_on_declared_focus_events_not_canvas_literals` (the old `canvas.focus` literal, now undeclared, is ignored even when `resolve()` would succeed). See `ag-ui-surface-trait-review.md` §6, Leak 3.
10. **`Effect::EmitAndAwait` suspend/resume: RESOLVED.** The shared async dispatcher registers keyed waits, replays pending events on reconnect, validates `Choice`/`Approval` reply shapes without consuming invalid attempts, races interrupts, and runs the Surface-owned reply interpreter. Vellum and Colab exercise the mechanism.

---

## 11. Naming (bikeshed later)

- Runtime crate: **`ag-ui-surface`** (exports `App` + the `Surface` trait). Alt: `ag-ui-app`, `ag-ui-shell`.
- Template: **`create-ag-ui-app`** (or `templates/surface-starter/`).
- Product framing / pitch: **"an agent that shares your surface."**

---

## 12. "Open later" hardening path

M5 now proves the abstraction across all three instances. A public release still requires an explicit API-stability review, genericized secret storage, publication documentation, and remotely reproducible dependency/CI setup. Those release tasks are separate from the working runtime proof.
