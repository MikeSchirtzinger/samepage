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
