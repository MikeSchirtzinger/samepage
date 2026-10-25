# Changelog

Every crate in this workspace shares one version. Before 1.0, a minor bump
(0.1 to 0.2) may break the public API and a patch bump never does.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
A pull request that changes what a user sees adds a line under Unreleased.

## [Unreleased]

The first release, 0.1.0. Nothing has been published to crates.io yet; the
`samepage` name is reserved at 0.0.0.

### Added

- The SamePage host (`apps/samepage`): one shared page that people edit in the
  browser and coding agents edit over MCP, with each recipe selecting the
  capabilities it loads.
- Capabilities, each a portable crate whose WASM build answers every shared
  case the way its native build does: map (cards and directed links), diagrams
  (state machines and hierarchies), review (marks, claims and answers),
  architecture (proposals and comparison), board, sketch (drawings and frames
  that contain cards), sources (cards bound to files and line ranges), lanes
  (what a source scan finds, beside what people placed), cement (decision
  drafts checked by `g8 check --enforce`), explain (walkthroughs), trace and
  panes.
- Agent tools with typed schemas and real descriptions, including
  `page_draw` for a whole picture in one call, `page_read` with what others
  changed since the last read, and `await_input`, which says what woke the
  agent and who did it.
- People keep their chosen name across reconnects and host restarts, and every
  record carries who wrote it and when.
- `samepage-agui`: AG-UI protocol types and stream assembly, derived from the
  upstream AG-UI Rust SDK under its MIT license.
- The shared canvas (`ag-ui-canvas`, `ag-ui-canvas-render`), the component
  host, the session-record validator (`ag-ui-record`) and the evaluation runner
  (`ag-ui-eval`) with a conformance suite.
- Two runnable examples: the room (`examples/same-page-room`) and the atlas
  (`examples/same-page-atlas`).
- CI on every pull request: formatting, clippy for native and wasm32, tests,
  WASM equivalence, the full recipe build and `cargo deny`.

### Changed

- `ag-ui-core` is now `samepage-agui`; the old name belongs to the upstream SDK.
- Rust 1.99 and edition 2024.
- Cement runs g8 0.1.5, and refuses any other version.
- The renderer draws through Vulkan on Linux, where it had no backend before.

### Removed

- `convergence-attrs`, a no-op attribute stub that kept `samepage-agui` from
  being publishable.

### Security

- wasmtime 49.0.4, rustls-webpki 0.103.16 and webpki-roots 1.0.11 for
  RUSTSEC-2026-0378, 0379, 0381 and 0382.
