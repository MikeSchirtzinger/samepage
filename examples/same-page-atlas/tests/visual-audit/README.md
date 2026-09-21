# Visual audit

The source-view pass is recorded in `../SOURCE-VIEW-PROOF.md`. The portable
runner is `run-audit.mjs`; the original capture below remains historical
baseline evidence. The current runner retains raw findings, screenshots,
detector identity, and tab cleanup, and fails on an incomplete audit.

Continue from the Same Page implementation in this branch. The goal is a green rendered visual audit while preserving real browser collaboration, readable source views, and the verified interactions.

The saved `baseline.json` is the raw audit captured during the September 6 polish pass. `recorded-audit.mjs` is the exact script that produced it, with its original machine paths preserved. It is a recorded command, not a portable runner. Update the Impeccable/Puppeteer import paths, target URL, and output path when running on another machine. See `../ARCHITECTURE-POLISH-PROOF.md` and `../UI-POLISH-PROOF.md` for the associated measurements and limitations. Private runtime state, credentials, host logs, and browser caches are excluded from this branch.

## Historical baseline failures

The recorded command was `node .local/architecture-polish/evidence/audit.mjs`. It returned exit 2 at desktop 1440 by 1000 and mobile 390 by 844.

- Desktop: four `clipped-overflow-container` warnings, on the page grid, Atlas pane, canvas viewport, and minimap canvas.
- Mobile: five clipping warnings, adding the collapsed minimap. One `content-hidden-at-rest` error counts 508 of 1168 text characters as hidden, about 43 percent.
- Hairline-border/wide-shadow findings remain advisory.
- The baseline contains no tiny-text, overflow, occlusion, or glow findings.

Canvas and minimap boundaries intentionally clip their contents. Node notes and source paths can be hidden at overview zoom. These observations explain where to investigate; they do not waive the failing audit. Diagnose each finding, preserve necessary camera clipping, and prove actual visibility and interaction. Do not obtain green by globally disabling rules, removing useful information, or weakening shared-state invariants.

## Start and reproduce

Read the repository and example `AGENTS.md` files first. `examples/same-page-atlas/build-web.sh` builds the browser WASM replica and requires `wasm-pack` and `wasm-opt`. Then use `cargo run -p same-page-atlas` and the example's documented environment variables. A new checkout needs its own runtime state and data. The ignored state used for the recorded baseline is not shipped here, so a fresh empty board does not reproduce that baseline.

On the original machine, the walkthrough is at `http://127.0.0.1:8174/` and the separate repository view is at `http://127.0.0.1:8177/?view=code-map-20260906061455720664`. Verify these are still running before using them. The worktree is `/Users/mike/dev/ag-ui-rust-wt-samepage`; its walkthrough state is `.local/understanding-walkthrough/atlas-proof-v2.json`, and its repository-view state is `.local/architecture-polish/code-atlas.json`.

For an independently reproducible source view, start an Atlas host about the checked-out repository and run:

```bash
python3 dev/samepage-code-map --package same-page-atlas --package same-page-atlas-web --depth 1 --url http://127.0.0.1:8098
```

Use the returned diagram URL and receipt. This is a current manifest-dependency view, not a runtime-call audit. Keep its scope distinct from the original walkthrough baseline.

Attach the detector to real Chrome. Use `createBrowserDetector({browser, waitUntil: "load", settleMs: 1500})`, scan both viewport sizes, save every finding, and return nonzero for non-advisory findings. The single-URL CLI previously timed out waiting for network idle on the persistent event stream and misleadingly returned zero. That timeout is a failed audit, not a pass. Disconnect the client after the scan and clean up only browser processes and tabs owned by the run.

## Preserve these behaviors

- All four themes persist locally without changing shared-document semantics or semantic category colors.
- Workspace, theme choices, Review, Decisions, source details, and zoom controls stay reachable and legible on mobile.
- Whole diagram opens undimmed. Focus step and Next object deliberately enter focus. Whole diagram restores the overview.
- Review and MCP `atlas_review` agree on source evidence, claim state, and recorded decision trade-offs. Source readability and human acceptance remain separate.
- Human edits remain browser-CRDT writes. Camera, theme, and navigation do not become semantic edits. Preserve source-reference validation and the measurement provenance contract.

Finish with raw before/after audit JSON, real desktop/mobile interaction evidence, visually inspected screenshots, relevant compiler/tests, and an explicit list of anything still unproven. Human acceptance remains separate from a green automated audit.
