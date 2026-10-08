# SamePage: shared Map and optional diagrams

This application composes one native `samepage-doc` Host with the pure Map capability and an optional Diagrams capability. The browser loads the unchanged `samepage.document.v1` facade and a strict `samepage.capability.v2` Map artifact, opens an authenticated document socket, and uses Map WASM to project cards and plan atomic batches for pointer moves. The shell does not provide an Atlas fallback. Diagrams adds typed hierarchies, state roles and routed transitions through its own artifact. See [the diagram contract](../../crates/samepage-diagrams/README.md).

## Build

From the workspace root, choose a private target directory outside the checkout and a private artifact output directory:

```sh
export CARGO_TARGET_DIR=/path/to/private-cargo-target
export CARGO_BUILD_JOBS=2
apps/samepage/build-capabilities.sh /path/to/private-artifacts \
  doc=crates/samepage-doc/web \
  map=crates/samepage-map=apps/samepage/modules/map.js \
  diagrams=crates/samepage-diagrams=apps/samepage/modules/diagrams.js
cargo build --locked -j 2 -p samepage-app
```

Omit the diagrams selection for a Map-only composition. For a worktree-contained build, artifact output may also live under the ignored `.local/` directory.

To include ordinary drawing, append `sketch=crates/samepage-sketch=apps/samepage/modules/sketch.js` to the build selections. Sketch loads when a shape appears, a sketch action runs, or a person chooses a drawing tool. Its rail supports rectangles, ellipses, arrows, pen strokes, text, movement, resizing, Excalidraw import/export, and lifting a labeled drawing into a card. Review remains optional; append `review=crates/samepage-review=apps/samepage/modules/review.js` for human marks. The native host composes lift with review retargeting in one document commit and refuses a marked lift if review is absent. See [the sketch contract](../../crates/samepage-sketch/README.md).

The build script inspects the selected Cargo dependency closure, builds real WASM packages, optimizes them, verifies their exported ABIs and descriptors, and publishes a manifest with file sizes and SHA-256 hashes. The native host checks the selected files and compiled Map descriptor before serving. Keep the artifact directory intact while the host runs.

## Run

Set `SAMEPAGE_STATE_DIR` to a private writable directory and `SAMEPAGE_CAPABILITIES_DIR` to the selected artifact directory. `SAMEPAGE_APP_ASSETS_DIR` is required and must contain the exact `web/` directory, `models.json`, and built `protocol/` package used by this build; the native host never falls back to its compile-time checkout. Set `SAMEPAGE_PROJECT_ROOT` to a real project path for agent context and `AGUI_MCP_TOKEN` to a private bearer token for MCP clients. `SAMEPAGE_ADDR` defaults to `127.0.0.1:8190`.

```sh
SAMEPAGE_STATE_DIR=/path/to/private-state \
SAMEPAGE_CAPABILITIES_DIR=/path/to/private-artifacts \
SAMEPAGE_APP_ASSETS_DIR=/path/to/frozen-app-assets \
SAMEPAGE_PROJECT_ROOT=/path/to/project \
SAMEPAGE_ADDR=127.0.0.1:8190 \
AGUI_MCP_TOKEN="$AGUI_MCP_TOKEN" \
"$CARGO_TARGET_DIR/debug/samepage"
```

The page joins a person through `/surface/me` before opening `/ws`. MCP agents attach with the bearer token. Authorship comes from those host-issued identities; action arguments cannot supply an author. A failed document save stops further writes and state reads, and appears in the page status and alert.

To import a legacy Atlas base64 state file, use a **new** target path. The source is read only and an existing target is refused:

```sh
"$CARGO_TARGET_DIR/debug/samepage" import-legacy /path/to/atlas.json /path/to/new-document.json
```

## Optional architecture comparison

Select `architecture=crates/samepage-architecture=apps/samepage/modules/architecture.js`
in the build command to add relationship proposals. It depends on Map and loads
on its first proposal record or Compare action. `architecture_read` reports the
semantic graph revision, changes and potential upstream paths. Each
`architecture_propose` retains a separate revision; `architecture_decide` records
a human accept/reject verdict without applying it to the Map. Omission leaves
Map, Diagrams and Review usable. When selected, Review includes the portable
architecture comparison in its report. See [the contract](../../crates/samepage-architecture/README.md).

## Optional Board

Add `board=crates/samepage-board=apps/samepage/modules/board.js` to the build
selection to include the independent Board capability. Its artifact is
`/capabilities/board/samepage_board_bg.wasm`. It loads on the first Board action,
opening the Board panel, or receiving saved Board data. A missing or corrupt
optional artifact produces a named failure while Map remains usable.

The native adapter saves `board.json` in `SAMEPAGE_STATE_DIR`, separately from
the CRDT document. Existing Board JSON loads directly with authors and marks
intact; reader cursors reset on restart. Human sign-off is checked against the
current assertion revision. Agent rewrites retain the mark and show stale
sign-off. The native `await_input` combines Board, document, human attention,
browser validation and chat wakes. Board cement is a human-approved advisory
export. See [the Board contract](../../crates/samepage-board/README.md).

## Optional trace

Select `trace=crates/samepage-trace=apps/samepage/modules/trace.js` to add
the Activity drawer. Its separate artifact loads only when Activity opens.
The drawer combines typed spans from every loaded WASM capability, displays
timing and parent links, and offers local clear, benchmark and export. The
host exposes `trace_read`, `trace_clear`, and `trace_benchmark` for native spans.
Host operational activity persists and replays through `/activity`; span rings
are process-local and start empty after restart/reload. Without the selection
there is no ring or activity file, activity routes return 404, and the toolbar
shows how many events were not recorded. Document history and authorship remain
active in both compositions.

## Optional Panes

Add `panes=crates/samepage-panes=apps/samepage/modules/panes.js` to the selected
build to load the retained shared pane renderer on first use. Panes uses the
same document as Map and requires no Map capability dependency. Human drag,
resize, raise, marks and notes preserve separate host bylines; agents place
panes by relation and named size. See [the Panes contract](../../crates/samepage-panes/README.md)
for preferred actions, aliases, source policy, legacy import and verification.

Optional Sources adds verified bindings, drift chips and a host-resolved
source viewer. Append
`sources=crates/samepage-sources=apps/samepage/modules/sources.js` to the
selected build. The first bound card, source action or viewer opening loads
the artifact. A failed source binding publishes no document change. When
Sources and Diagrams are selected together, the build transfers Diagrams'
provisional binding keys to Sources. See [Sources](../../crates/samepage-sources/README.md).

Optional Lanes adds the collapsed “Found in source” strip with unresolved
counts, inspectable static findings and verified extra-binding claims. Append
`lanes=crates/samepage-lanes=apps/samepage/modules/lanes.js` with Sources
selected. It loads when the strip opens, a lane action runs or a saved binding
is present. See [Lanes](../../crates/samepage-lanes/README.md).

Cement is optional: add `cement=crates/samepage-cement=apps/samepage/modules/cement.js`
to the selected build, alongside Sources, Lanes and Review. Its native adapter
requires real `g8` 0.1.4 from PATH and the selected Git project root. Agents draft
and propose decisions; the person previews and publishes from the Cement strip.
Current gates and their reasons refresh together on Sources cards without reload.

## Optional walkthroughs

Append `explain=crates/samepage-explain=apps/samepage/modules/explain.js` together
with Sketch to the build selection. Explain loads on the first explanation
record or walkthrough action. It provides shared steps, revisioned controls,
point/reveal animations, connector tracing, button and response transitions,
and scoped drawing. `walkthrough_read` exposes the same cursor to agents;
`replay-motion` is visibly unsupported. See [the contract](../../crates/samepage-explain/README.md).

## Complete and minimal recipes

From a clean shell with Rust (including `wasm32-unknown-unknown`), wasm-pack,
wasm-opt, Node, Python 3 and ripgrep installed, run either recipe with a project
directory. The full recipe also needs real `g8` 0.1.4 on PATH and a Git project.
Both use the existing capability build script, build the typed protocol client
and native app, copy this checkout's web assets, and run the one SamePage host:

```sh
apps/samepage/recipes/full.sh /path/to/git-project
apps/samepage/recipes/map-only.sh /path/to/project
```

The full composition selects doc, map, diagrams, review, architecture, board,
sketch, trace, panes, sources, lanes and cement. Map-only selects doc and map.
Selection permits use; optional WASM still loads on its first record, action
or gesture. There is no separate Legend capability or WASM artifact.

Defaults need no environment: address `127.0.0.1:8190`, four build jobs,
`target/` in this checkout, and artifacts, assets, temporary files and persistent
state under `.local/recipes/full/` or `.local/recipes/map-only/`. The default
local MCP bearer is `local-samepage-token`. Override `AGUI_MCP_TOKEN`,
`SAMEPAGE_ADDR`, `SAMEPAGE_RECIPE_DIR` (absolute output directory),
`SAMEPAGE_STATE_DIR`, `CARGO_TARGET_DIR` (absolute), `CARGO_BUILD_JOBS` (at most
four), or `TMPDIR` as needed. The recipes add the usual Cargo and local bin
directories to PATH. The server runs until interrupted; start it detached with
captured output for an unattended run and wait for its listening message.

The shell's Legend stays beside the surface and composes each successfully
loaded capability's ABI `legend(view)`. Its default shows keys currently drawn;
“Show all” also lists keys of loaded capabilities that are absent from the
current view. Failed and unselected capabilities contribute no keys. Swatches
use the real elements' classes, SVG geometry and computed paint, including
ancestor-scoped CSS. Glyphs and words accompany every key. Colours in authored
Sketch drawings carry the author's meaning. Trace has no map keys; Activity is
its own surface.

For a new full-composition project, initialize its G8 space before scanning
Lanes or publishing decisions: run `g8 init --no-claude-import --no-subagents`
from the project directory. The recipe does not author project obligations.

With a real Chromium page already exercised over CDP, audit the legend against
the live elements (including SVG child paint) without changing the surface:

```bash
node apps/samepage/proofs/legend.mjs http://127.0.0.1:9481 .local/legend/audit.json
```

The JSON records each key, its page source, paint pairs, loaded sections and
resource URLs. This check covers the current view; use pointer and keyboard
input to expose other states and capture side-by-side screenshots separately.
