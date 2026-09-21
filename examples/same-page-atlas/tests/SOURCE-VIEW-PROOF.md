# Code controls and rendered visual audit

Cards and container headers have a code control. It opens the host's current
source in a viewport-bounded reader with line numbers, paging, and file
browsing. Package manifests offer readable Rust entry files; their linked
manifest remains available in the file selector. Live claim references are
included, withdrawn and unrelated claims are excluded, and missing sources
remain explicit.

The reader uses one verbatim code text node and a separate nonselectable line
number gutter. Close and Escape return keyboard focus to the originating
control. Opening files changes neither the camera nor the shared document.

Canvas clipping now belongs to paint containment on the drawing surfaces.
Surrounding layout containers let controls escape. This also prevents focus
from scrolling the canvas's DOM independently of its camera. Closed menus have
no positioned layout. Overview details retain their measured space while their
accessibility and keyboard state match their actual disclosure state. Mobile
navigation grows to fit wrapped controls instead of covering them with canvas.

## Evidence

Commands ran in the `codex/samepage-understanding` worktree. Local run evidence
is retained in `.local/source-view/` and is not published with this document.

| Check | Result and command |
| --- | --- |
| Host tests | `cargo test -q -p same-page-atlas`: 180 passed, one existing ignored test. The eight source tests also passed after strengthening the large-file fixture. |
| Strict Rust lint | `cargo clippy -q -p same-page-atlas --all-targets --no-deps -- -D warnings`: passed. |
| Host build | `cargo build -q -p same-page-atlas`: passed. |
| Reference selection | `node --test examples/same-page-atlas/tests/source-references.test.mjs`: two passed. |
| Browser source reader | `node examples/same-page-atlas/tests/visual-audit/source-view-proof.mjs --url 'http://127.0.0.1:8177/?view=code-map-20260906061455720664'`: real source bytes, all five package cards at both sizes, paging, file browsing, four themes, focus restoration, unchanged document and camera, and no browser errors. |
| HTTP reads and refusals | `python3 .local/source-view/host-proof.py`: 11 checks passed, including filesystem byte comparison, missing paths, stale ranges, traversal and protected directories. |
| Canvas behavior | `.local/source-view/audit-camera-final-proof.json`: trusted wheel zoom, background pan, clipped off-canvas hit targets, inline editor focus without DOM scrolling, and unchanged semantic state. |
| Visible shared Chrome | `node .local/source-view/visible-proof.cjs`: trusted code-button click in the existing visible tab, exact source bytes, unchanged document and camera, and page checks passed. The desktop and mobile screenshots were visually inspected. |
| Resource graph | `python3 dev/agent-harness/check.py --json`: passed. |

The final detector command scans both viewport sizes. It first requires a
healthy app under its normal security policy and saves that pristine view.
The scanner needs inline style probes, which the app's policy correctly
rejects. Only its disposable audit tab is then reloaded with permission for
those probes. The application's CSP and the user's Chrome session are
unchanged. No detector rules are disabled, and probe errors fail the run.

```bash
node examples/same-page-atlas/tests/visual-audit/run-audit.mjs \
  --url http://127.0.0.1:8174/ \
  --url 'http://127.0.0.1:8177/?view=code-map-20260906061455720664' \
  --expected-nodes 31,5 \
  --allow-audit-style-probes true \
  --output .local/source-view/audit-full-final
```

The initial baseline below did not execute the style probes. Enabling them
later exposed additional real contrast failures in the zoom percentage and
toolbar shortcut labels. The open reader also exposed faint disabled paging
labels. The final audit includes those probes and their contrast corrections.
Counts are observations of the stated runs, not a
benchmark of identical instrument coverage.

| Surface | Viewport | Before | After |
| --- | --- | --- | --- |
| Walkthrough on 8174 | 1440 by 1000 | 5 | 0 |
| Walkthrough on 8174 | 390 by 844 | 6 | 0 |
| Package map on 8177 | 1440 by 1000 | 5 | 0 |
| Package map on 8177 | 390 by 844 | 10 | 0 |

The open code reader also returns zero non-advisory findings at both sizes.
All six final scans retain the detector's advisory findings: 13 per desktop
scan and 14 per mobile scan. Green means zero non-advisory findings, not zero
findings of every severity.
Deliberately clipped interactive content and substantial invisible text still
trigger their respective rules at both sizes, returning exit 2. These controls
are fixtures that test the detector, not product functionality evidence.
A deliberately incorrect expected node count returns exit 1, preventing a
partially loaded board from receiving a passing result.

The raw findings are `audit-baseline-raw.json`, `audit-full-final-raw.json`,
`audit-source-full-final-raw.json`, and `audit-control-full-final-raw.json`.
Matching receipts record the detector hash, screenshots, and cleanup. Browser
tests preserve existing tabs and isolate local preferences from the user's
Chrome session.

## Bounds

Source files are bounded to 2 MiB, with each excerpt bounded to 400 lines and
24,000 characters. Paging ends at complete lines when possible; an oversized
single line is explicitly reported as clipped. The existing 512 KiB
whole-document import limit is unchanged. Repository path and text checks
remain enforced by the same reader used by `repo_read`.

Existing diagrams remain authored snapshots. Opening current code does not
refresh a captured manifest digest, prove runtime relationships, or supply a
human verdict. Automated visual-audit success and human acceptance remain
separate.
