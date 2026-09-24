# SamePage: minimal shared Map host

This application composes one native `samepage-doc` Host with the pure Map capability. The browser loads the unchanged `samepage.document.v1` facade and a strict `samepage.capability.v2` Map artifact, opens an authenticated document socket, and uses Map WASM to project cards and plan atomic batches for pointer moves. The shell does not provide an Atlas fallback. Other retained SamePage interactions are added as separate capabilities after this Map slice.

## Build

From the workspace root, choose a private target directory outside the checkout and a private artifact output directory:

```sh
export CARGO_TARGET_DIR=/path/to/private-cargo-target
export CARGO_BUILD_JOBS=2
apps/samepage/build-capabilities.sh /path/to/private-artifacts \
  doc=crates/samepage-doc/web \
  map=crates/samepage-map=apps/samepage/modules/map.js
cargo build --locked -j 2 -p samepage-app
```

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
