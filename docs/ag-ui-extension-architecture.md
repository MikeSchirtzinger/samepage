# AG-UI app template: core and extension architecture

## Decision

Build one small browser shell around standard AG-UI, then compose capabilities
as namespaced extensions. Keep server extensions compile-time and type-checked.
Load their browser modules only when an app enables them. Do not make a dynamic
native plugin ABI the first version.

**Gate 1 landed 2026-07-11.** `ag-ui-surface` now serves a shared
`/_agui/client.js`, generates `GET /extensions` from validated
`Surface::client_modules()`, and rejects cross-origin/traversal module paths at
startup. It also resolves every advertised module/WASM URL through configured
static mounts and fails launch when an installed asset is missing. The board and
WebGPU canvas are real lazy ES modules. Vellum and Colab are truthfully reported
as `loading: "host"` until their existing page boot code is extracted. The loader
never calls those migrated when they are not.

**Server composition landed 2026-07-11.** The new `Extension` trait keeps each
capability's state, actions, routes, reconnect behavior, and browser module
together. `CompositeSurface` validates ownership and adapts an enabled set back
into the runtime's one existing `Surface`/dispatcher path. A strict
`agui.app.toml` pins the locally compiled ids, versions, capabilities, and
required policy. It cannot install or fetch code. The starter proves two real
extensions (`workspace` + `board`), namespaced composite state, independent
routes/events, and two lazy browser modules.

**Runtime-owned agent schema projection landed 2026-07-12.** `ActionDef`
(`ToolDef` remains a compatibility alias) now declares `Human`, `Agent`, or
`Both` discovery plus optional visual-result metadata. Once
`CompositeSurface` has applied `agui.app.toml`, `ag-ui-surface` derives every
agent schema from that effective `Surface::tools()` and serves MCP in process at
Streamable HTTP `POST /mcp`. ACP setup selects that HTTP endpoint when the
adapter advertises support. An application-supplied `McpBridge` is retained
only as an optional custom stdio fallback. The current applications no longer
carry bridge binaries or duplicate schema allowlists.

**Authoritative action-input enforcement landed 2026-07-12.** Before binding or
starting a provider, `App::serve` meta-validates and compiles the JSON Schema on
every effective `ActionDef`, including human-only actions. Schemas without an
explicit dialect use JSON Schema 2020-12. Supported explicit dialects are
honored, while invalid schemas, unsupported dialects, and unresolved references
fail startup. Human, OpenAI, and MCP calls then reuse that compiled contract at
the single dispatcher immediately before `apply`. Malformed OpenAI argument JSON
is rejected instead of being converted to an empty object. This closes the
server-side tool-input requirement in the MCP 2025-06-18 tools specification:
<https://modelcontextprotocol.io/specification/2025-06-18/server/tools>.

This gives the project three honest product shapes from one contract:

1. **Browser core**: conversation, streaming, reconnect, shared state, and
   human/agent actions. One HTML/JS payload, estimated below 1 MB.
2. **Browser core plus extensions**: uploads, TTS controls, diagrams, or the
   WASM/WebGPU canvas are fetched only when declared in the app manifest.
3. **Full application**: the same browser client packaged with the Rust host,
   local storage, OS integrations, and any heavyweight model or media assets.

The sub-megabyte target applies to the browser core, not the complete native
Rust executable. Tokio, HTTP, TLS, and provider integrations make a sub-megabyte
native server an unrealistic and unhelpful constraint. Current measured assets:
the shared protocol/loader core is 8.1 KB raw / 2.6 KB gzip. Starter chrome is
16.7 KB raw / 5.6 KB gzip. The board extension is 2.1 KB raw / 0.95 KB gzip.
The canvas adapter is 1.3 KB raw / 0.62 KB gzip. The optional renderer is
estimated at 74 KB of generated JavaScript plus 1.19 MB of WASM before
compression.

## Irreducible core

The core owns only the behavior every human-agent workspace needs:

- standard AG-UI message streaming (`TEXT_MESSAGE_START`,
  `TEXT_MESSAGE_CONTENT`, `TEXT_MESSAGE_END`).
- connection, reconnect, completed history, and in-flight message replay.
- provider selection and BYOK configuration without returning secrets to the
  browser.
- one named action dispatcher used by both humans and agents.
- state snapshot/replay and extension discovery.
- interruption and generic human-in-the-loop decisions.

The stable HTTP surface should be:

| Route | Core meaning |
|---|---|
| `GET /events` | AG-UI event stream |
| `POST /ask` | Human message / turn request |
| `POST /interrupt` | Cancel the active turn |
| `POST /surface/action` | Invoke a named action through the shared dispatcher |
| `GET /surface/state` | Text or structured state read-back |
| `GET /extensions` | Enabled extension manifest |
| `POST /mcp` | Runtime-owned Streamable HTTP MCP. Schemas and calls project the effective agent-visible actions |

The current `/canvas-tool` and `/canvas-state` routes can remain compatibility
aliases while clients migrate. New code should not acquire more `canvas-*`
names in the generic runtime.

## Extension contract

An extension is one complete capability loop, not merely a UI component. It
contributes all of the following or explicitly opts out:

- a stable id and version.
- owned state and reconnect representation.
- named actions with JSON Schema inputs.
- `ActionAudience::{Human, Agent, Both}` discovery metadata.
- whether a successful model-facing result explicitly includes a current
  surface PNG via `.with_state_snapshot()`.
- agent read-back/context for that state.
- server routes or static mounts, if needed.
- a browser entry module and mount point, if it has UI.
- namespaced events and interaction events back to the agent.

The browser manifest supports more than one module per Surface:

```rust,ignore
fn client_modules(&self) -> Vec<ClientModule> {
    vec![
        ClientModule::lazy("board", "0.1.0", "/extensions/board/index.js", "board")
            .action("add_note")
            .event("surface.board")
            .capability("dom"),
        ClientModule::lazy("canvas.webgpu", "0.1.0", "/extensions/canvas/index.js", "canvas")
            .capability("wasm")
            .capability("webgpu"),
    ]
}
```

The runtime validates unique ids/mounts and verifies that every declared action
exists in `Surface::tools()` and is human-visible. A sole module defaults to the
Surface's human-visible action vocabulary. Every module in a composed Surface
must call `.action(...)` or `.no_actions()` so ownership is explicit. The
browser hands each module a narrow context:

```js
export async function activate({ client, extension, mount, action, semantic }) {
  // mount UI, subscribe with client.on("namespace.event", handler),
  // and mutate only through action(name, args).
}
```

`client` in that context is a subscription-only facade, not the host's full
request client. `action` checks the extension's declared action ownership before
dispatch. This is useful least-authority API design, though same-origin JavaScript
is still trusted page code rather than a security sandbox.

Server-side state/action composition is now explicit:

```rust,ignore
let recipe = AppRecipe::from_file("agui.app.toml")?;

App::new().surface(move |transport| {
    CompositeSurface::from_recipe(
        &recipe,
        vec![
            Box::new(Board::memory(transport.clone())),
            Box::new(Workspace::new(transport)),
        ],
    )
    .expect("validated extension recipe")
});
```

The recipe selects only implementations already compiled into the application.
`CompositeSurface` rejects duplicate ids, actions, route method/path pairs,
focus events, mismatched browser/server metadata, core-route collisions, and a
second claimant on the currently unnamespaced raw WebSocket transport before
the server binds. Its selected action list is the only projection source:

- browser manifests include `Human` and `Both` actions.
- OpenAI function tools and MCP `tools/list` include `Agent` and `Both` actions.
- MCP `tools/call`, OpenAI calls, and browser controls all reach the same
  `ActionDef::apply` through the runtime dispatcher.

MCP `tools/list` also carries the audience metadata standardized by the stable
[MCP Apps 2026-01-26 specification](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx):
`Agent` projects as `_meta.ui.visibility: ["model"]`, while `Both` projects as
`["model", "app"]`. The runtime deliberately does **not** project `Human` as an
app-only MCP tool. Human-only actions remain absent from the model-facing
catalog entirely, so their names and schemas are not disclosed merely because
MCP Apps permits `visibility: ["app"]`. Browser discovery continues through
the separately filtered `GET /extensions` manifest. Likewise, the runtime does
not synthesize `ui.resourceUri`. That optional field requires a truthful MCP UI
resource declaration.

`ActionAudience` controls discovery, not authority and not implementation.
There must never be a second browser-only mutation path. Visual return is also
explicit: only an action marked `.with_state_snapshot()` asks a vision-capable
backend to append `SurfaceState::snapshot_png()` when one exists. Query effects
alone do not imply an image.

The runtime now generates an active allowlist like:

```json
{
  "schemaVersion": 1,
  "protocol": "ag-ui",
  "extensions": [
    {
      "id": "board",
      "version": "0.1.0",
      "loading": "lazy",
      "module": "/extensions/board/index.js",
      "mount": "board-extension",
      "actions": ["add_note", "clear_board", "read_board"],
      "events": ["surface.board"],
      "capabilities": ["dom"],
      "required": true
    }
  ]
}
```

The shared client validates the manifest again, permits only same-origin module
and WASM URLs, imports lazy entries, calls `activate`, records ready/error/host
status, and only then connects `/events`. Loading before SSE is important: the
extension is subscribed before reconnect events replay its state.

The agent-facing projection is deliberately not another manifest or
application allowlist. `ag-ui-surface` handles MCP `initialize`, `tools/list`,
and `tools/call` at `POST /mcp` directly from the already-composed Surface.
The endpoint authenticates every request, rejects non-loopback browser origins,
and enforces the protocol version negotiated during MCP initialization.
Authenticated `GET /mcp` is not a second transport and returns
method-not-allowed. During ACP session creation the runtime prefers this
Streamable HTTP endpoint when the adapter reports HTTP MCP capability.
`App::mcp_bridge(...)` exists only for a caller that deliberately supplies a
custom stdio compatibility server.

## Two manifests, plus one local recipe

Do not make the marketplace catalog executable runtime state. They serve
different trust and lifecycle needs:

1. **Package manifest (marketplace/install time)**: source location, hashes,
   compatible core versions, server crate/feature, browser entry, permissions,
   supported distribution profiles, defaults, license, and upgrade notes. An
   installer or ADA skill reviews and materializes this package locally.
2. **Runtime manifest (`GET /extensions`)**: the small immutable allowlist this
   particular server compiled/enabled: module URL, mount, actions, events,
   capabilities, and required/optional status. It contains no registry URL and
   cannot cause a browser to fetch code from the marketplace.

This preserves a shared marketplace without making page load a supply-chain
installation event. Browser modules still run with the page's authority. The
action allowlist in their context is a clean API boundary, not a security
sandbox. Truly untrusted widgets still require an isolated origin or sandboxed
iframe.

`agui.app.toml` is neither manifest. It is the reviewable local selection over
packages that are already installed and compiled: exact id/version pins,
enabled/required policy, capability grants, and inert extension settings. The
host compares it to compiled metadata and stops on drift. Only the resulting
active allowlist reaches the browser.

## Product profiles and the future setup skill

Profiles are recipes over extensions, not forks of the core:

- **web**: DOM core plus selected browser modules. No WASM unless an enabled
  extension declares it.
- **wasm**: the same protocol core with one or more WASM render/compute
  extensions, still served by the Rust host.
- **full**: web/WASM modules plus explicitly trusted native capabilities such
  as files, local models, neural TTS, notifications, or a desktop shell.

A future ADA setup skill should interview the user, write one reviewable app
recipe, explain requested permissions/costs, install pinned packages, build the
host, and prove every enabled loop in the browser. First-run onboarding and a
later “change my setup” request should execute the same deterministic flow. The
skill should never silently substitute a mock when a selected extension or
profile cannot run.

**Repository routing.** The root `AGENTS.md` and `Cargo.toml` describe the
checked-in packages, commands, extension mechanics, and proof vocabulary. The
Same Page Room can project host-resolved source anchors without requiring a
hand-built page for every subsystem.

The enforcement layer currently called Govern will likely ship under a
different name because its crates.io name is taken, but its machine-checked
agreements still keep people and agents aligned while stopping project drift.

## Capability ladder

Add extensions in increasing order of cost and trust:

1. **Board / structured state**: text notes and small forms. Plain DOM. Whole
   state snapshots are sufficient.
2. **Uploads**: bounded files and images, explicit size/type limits, content
   hashing, no automatic execution, and an agent read-back adapter.
3. **Diagrams**: structured nodes/edges rendered as SVG first. The agent edits
   data, not generated pixels, and human selection returns semantic ids.
4. **TTS**: a transport/control extension. Browser speech can remain tiny.
   Local neural voices and their model weights belong to the full-app package.
5. **Canvas**: CRDT scene state plus the existing optional WASM/WebGPU
   renderer. Bulk geometry stays on its binary blob path.
6. **Interactive widgets**: only after a sandbox and capability policy exist.
   Third-party code should not execute in the core document by default.

Each rung is accepted only when the full loop is proven:

`human or model action -> one dispatcher -> state mutation -> all clients -> agent read-back`

## Packaging and budgets

| Package | Initial budget | Loading rule |
|---|---:|---|
| Core HTML + JS + CSS | under 100 KB gzip | always |
| Board | under 25 KB gzip | enabled by starter |
| Upload UI | under 40 KB gzip | on manifest entry |
| SVG diagrams | under 100 KB gzip | on manifest entry |
| Canvas JS + WASM | separately budgeted | only for canvas apps |
| TTS model weights | no core budget | full app or explicit download |

Dependencies are measured in the browser transfer, parsed JavaScript, and
startup work, not only repository size. CI should build each package, record raw
and gzip sizes, and fail when the core exceeds its budget.

## Trust boundary

- Server extensions are compiled into the application for version 1.
- Browser modules are loaded only from the server-generated allowlist.
- Extension ids namespace state, routes, events, and assets.
- Action audiences filter discovery. Actions validate inputs server-side
  against startup-compiled schemas regardless of where they originated, and all
  calls share one pre-`apply` dispatcher gate.
- The in-process `/mcp` endpoint exposes only effective agent-visible actions.
  Recipe-disabled and human-only actions do not enter its schema list.
- `/mcp` requires the per-process bearer token supplied in the ACP HTTP server
  descriptor and rejects non-loopback browser origins.
- Uploads never imply execution.
- Arbitrary widget code requires an isolated origin or sandboxed iframe plus a
  narrow message bridge. It is not a base capability.
- Secrets remain host-side. A future fully static BYOK client is a separate,
  explicit security mode because browser-held keys and provider CORS policies
  have different risks.

## Migration from the current code

### Gate 0: make the current vertical slice true

- Use standard AG-UI text events so one turn is one streaming message.
- Let the browser invoke board actions through the same dispatcher as models.
- Persist BYOK endpoint/model settings without clobbering the saved key.
- Prove the loop live with a real OpenAI-compatible provider.

### Gate 1: operational extension discovery: DONE

- `GET /extensions` is generated from validated `ClientModule` metadata and the
  real Surface tool vocabulary.
- `/_agui/client.js` owns generic SSE, JSON requests, action dispatch, manifest
  validation, same-origin checks, and dynamic imports.
- The board owns its DOM/action/event loop as a lazy module. The teaching canvas
  owns its WebGPU/WASM boot adapter as a lazy module.
- Rust contract tests cover manifest/action generation and path rejection. Live
  browser tests cover module fetch, activation, reconnect state, human actions,
  and actual WebGPU pixels.
- A future packaging step may bundle these files into one artifact, but the
  source/runtime contract remains modular. A single file is distribution, not
  architecture.

### Gate 2: first optional capabilities

- Typed server-side `Extension`/`CompositeSurface` composition and strict
  `agui.app.toml` selection are done.
- Ship uploads and SVG diagrams as separate extensions.
- Action audience and explicit visual-result metadata are done. Composite state
  is already namespaced by id.
- Effective action schemas are meta-validated and compiled before bind, then
  enforced once in the shared dispatcher for browser, OpenAI, and MCP calls.
- Add size-budget checks and extension contract tests.

### Gate 3: heavy visual capability: DONE for the local compiled slice

- The existing CRDT + WASM + WebGPU canvas is a manifest-loaded extension with
  explicit `wasm`, `webgpu`, `websocket`, and `crdt` capabilities.
- WebGPU feature failure is explicit. The board proves a non-WASM core path.
- teaching-canvas's former `CanvasSurface` is now `CanvasExtension`. Its CRDT
  state, canvas actions, mission routes, learner store, focus vocabulary, raw
  binary transport ownership, and lazy browser module stay under one owner.
- One `agui.app.toml` selects the real DOM board and WASM/WebGPU canvas in the
  same unchanged host. A disabled pin is not constructed, contributes no state,
  actions, routes, bridge, or static mount, and its module/package URLs return
  404. The enabled canvas path still loads the real wasm-pack bundle and renders
  WebGPU pixels. The enabled board path still sends human actions through the
  shared dispatcher and appears in agent read-back.
- The former app-local ACP seam is removed. `ag-ui-surface` derives agent-visible
  schemas from the recipe-composed Surface, serves them at Streamable HTTP
  `POST /mcp`, and dispatches calls in process. Teaching, Vellum, Colab, and the
  starter carry no bridge binary, handwritten allowlist, or `.mcp_bridge()`
  wiring. A custom stdio bridge remains available only as a compatibility
  fallback when an ACP adapter does not support HTTP MCP.

### Gate 4: full application distribution

- Bundle the Rust host and browser assets in a native shell only after the web
  contract is stable.
- Add storage, OS file integration, and local TTS as explicit full-app
  extensions rather than core dependencies.

## Acceptance tests for the public template

- A provider's key, endpoint, and model survive a process restart.
- No credential appears in browser responses or logs.
- A streamed turn produces one message id, one bubble, and one history entry.
- Reconnecting mid-turn reconstructs that message and continues appending.
- The same action invoked by a human and by a model produces the same state
  transition and event shape.
- A second browser receives both actors' changes without refresh.
- Removing an extension removes its assets and routes from the build/runtime.
- MCP `tools/list` includes only agent-visible actions from the effective recipe
  selection. Human-only and disabled-extension actions are absent.
- MCP `tools/call`, OpenAI tool calls, and browser actions reach the same
  `ActionDef::apply` implementation.
- Authenticated `POST /mcp` executes MCP. Authenticated `GET /mcp` returns 405,
  while missing credentials, untrusted origins, and unsupported or mismatched
  `MCP-Protocol-Version` headers fail before dispatch.
- Extensions cannot claim the core `/mcp` route.
- ACP session setup selects Streamable HTTP when supported and uses a custom
  stdio bridge only when one was explicitly configured as a fallback.
- A text-only query returns no image, while an action marked
  `.with_state_snapshot()` includes a PNG when the Surface can render one.
- Core size stays within budget.
- Every advertised provider and extension is executable. Unavailable features
  are hidden or explicitly reported as unavailable, never replaced by mocks.
