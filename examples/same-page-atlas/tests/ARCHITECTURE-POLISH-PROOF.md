# Architecture review and appearance polish

The updated walkthrough runs at `http://127.0.0.1:8174/`. A separate view of this repository's declared Rust package dependencies runs at `http://127.0.0.1:8177/?view=code-map-20260906061455720664`. Both serve `/Users/mike/dev/ag-ui-rust-wt-samepage`. The walkthrough keeps its existing project root, `.local/understanding-walkthrough/project`. The code view uses the worktree root. Existing diagrams and decision drafts were preserved.

## What changed

- Workspace offers Slate, Paper, Sand, and Contrast. Appearance persists locally without changing the shared document or authored semantic colors. Connection settings follow the selected theme.
- Functional labels were enlarged. Mobile Workspace stays inside the viewport, the shared question wraps, presence no longer overlaps the minimap, and closed controls do not occupy layout space. Live sync retains an accessible label without stuffing status text into its dot.
- Review shows source gaps, current file readability, explicit claims and human verdicts, incoming and outgoing authored connections, and recorded rationale, alternatives, trade-offs, consequences, and uncovered requirements. Missing information stays explicit. The browser and `atlas_review` use the same host projection.
- `dev/samepage-code-map` publishes dated source views through real shared-page tools. It reads indexed Cargo manifests, resolves local package declarations and workspace inheritance, records source hashes, labels dependency kinds and omissions, and retains publication/read-back receipts. `--check` detects changed captured manifest bytes.
- Source cards have room for paths and notes before layout. Direct diagram URLs open the whole view without step dimming. Focus step, Whole diagram, and Next object control emphasis deliberately and preserve shared state.

## Verification

Evidence is local under `.local/architecture-polish/evidence/` and `.local/architecture-polish/qa/`. These directories contain machine-readable results, commands, screenshots, and logs. The publication receipt is `.samepage/code-maps/code-map-20260906061455720664.json`.

| Check | Result and evidence |
| --- | --- |
| Rust host and core | `cargo test -p same-page-atlas -p same-page-atlas-core`: 171 host tests passed, one ignored, 365 core tests passed. `cargo-test.log`. A final `cargo test -p same-page-atlas` also passed after the last browser changes, `final-host-tests.log`. |
| Build and lint | `cargo build -p same-page-atlas`, `cargo clippy -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings`, and `cargo fmt -p same-page-atlas -- --check` passed. |
| Manifest helper | `python3 -m unittest discover -s dev/tests -p 'test_samepage_code_map.py'`: four tests passed, including unresolved local dependency refusal and inherited, renamed, optional, and target-specific declarations. |
| Existing exploration boundaries | `node --test examples/same-page-atlas/tests/explore-graph.test.mjs`: four tests passed. |
| Resource graph | `python3 dev/agent-harness/check.py --json`: `ok: true`, no errors. Dirty-worktree warning retained. |
| Real repository comparison | `cargo metadata --offline --locked --no-deps --format-version 1` independently matched all six selected dependency pairs among the five published packages. `code-map-verification.json` and `cargo-metadata.json`. |
| Source freshness and negative control | `python3 dev/samepage-code-map --check .samepage/code-maps/code-map-20260906061455720664.json` returned current sources. A copied receipt with an altered hash returned exit 2 and identified `Cargo.toml`. No source file was changed for this negative control. `final-parity-freshness.json`. |
| Agent/browser agreement | Live MCP `atlas_review` and HTTP `/atlas/review` returned identical content at the same document revision. `final-parity-freshness.json`. |
| Independent real Chrome stories | `node .local/architecture-polish/qa/run-story.cjs`: 103 assertions passed. `retest.cjs`: 24 assertions passed after fixing offscreen mobile theme controls and light-theme zoom contrast. The final focus regression added 12 passing assertions. See `qa/REPORT.md` for commands and screenshots. |
| Final browser checks | `node .local/architecture-polish/evidence/final-browser.mjs`: 15 assertions passed, including deep links, undimmed overview, mobile source readability, Review/Discuss transitions, readable connection text, viewport bounds, claim-count parity, and accessible sync. Zero console or page errors. |

Browser checks used real Chrome over CDP at desktop 1440 by 1000 and mobile 390 by 844. Theme changes and local navigation preserved shared-document digests in the independent checks. The final screenshots were opened and visually inspected. No claim was accepted and no decision was confirmed during this pass.

The final visible pass used `browser-start --collab` and `node .local/architecture-polish/evidence/visible-review.mjs`. Both views loaded in a normal Chrome window. Window state and observations are recorded in `visible-review.json`, with inspected captures `visible-8174.png` and `visible-8177.png`. The test browser was stopped with `browser-stop`, then both URLs were opened in normal Chrome for continued review. `final-listeners.json` confirms both live servers returned HTTP 200 and retained the expected project roots and object counts. Public-text lint reported no hard violations; JavaScript syntax and final diff checks passed.

The final host executable has SHA-256 `a4f40d058d6035be1a6b230702af2ef1f2d6e258e2b85a1599f395f0394c0733`. Both live hosts use the byte-identical copy `.local/architecture-polish/same-page-atlas-runtime-20260906-final`; static assets load from this worktree. The original binary launches experienced a pre-main delay and eventually started. The exact operating-system cause was not established. Final listener readiness was checked after launch.

## Automated visual audit remains non-green

`node .local/architecture-polish/evidence/audit.mjs` uses the Impeccable browser API with `waitUntil: load`, since the persistent event stream prevents network-idle completion. The final command exits 2. Its raw findings remain in `impeccable-rendered.json`; no rules or gates were suppressed.

The latest scan reports zero tiny-text, overflow, occlusion, and glow findings. It still reports:

- Desktop: four clipping warnings for the page grid, Atlas pane, canvas viewport, and minimap canvas.
- Mobile: five clipping warnings, adding the collapsed minimap, plus hidden content at rest. The detector counts 508 of 1168 text characters as hidden, about 43 percent.
- Advisory hairline-border/shadow style findings remain.

The clipping findings are on intentional camera and minimap boundaries. The hidden text includes node detail suppressed at overview zoom. Final screenshots show the whole-board overview and the readable mobile Focus step. Review provides unscaled text, and opened navigation controls passed viewport and click tests. Those targeted observations do not turn the raw audit green or prove every possible canvas arrangement accessible. The raw audit remains an explicit limitation.

## Scope of the code view

The published map covers five packages and six declared dependency pairs. These measurements come from the manifest helper, the Cargo comparison, and the shared-page read-back above. Arrows represent manifest declarations, including declared dependencies regardless of whether a particular feature or target enables them. They do not establish runtime calls, resolved feature behavior, execution order, unused code, or correctness. Each package claim states only that the cited manifest declares that package. The claims remain open for human review.

Working-file readability does not verify diagram prose. A commit-pinned historical claim does not establish freshness against today's code. Missing trade-offs remain missing rather than being invented. Change impact, failure paths, data ownership, reversibility, operational cost, and proposed-versus-current differences remain useful next review questions, not completed analyses.

The configured in-page model provider remains `none`. Real MCP tools, repository reads, Cargo output, and browser interactions were used. No live model response or new G8 enforcement result is claimed. Implementation and tested interactions are established; human acceptance and a fully green visual audit are not. Preexisting worktree edits were retained. At verification time, no commit or push had been made.
