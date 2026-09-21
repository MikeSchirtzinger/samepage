# Same Page interaction polish

The live surface at `http://127.0.0.1:8174/` serves this worktree. The changes condense workspace controls, make the walkthrough collapsible, replace the scope form with a hoverable funnel, and open object contents and immediate connections on double-click. Details and provenance are contextual. Prepared decisions open directly into review, with the actual G8 specification, optional editing, and human confirmation.

The local evidence is in `.local/ui-polish-proof/evidence/`. Independent browser evidence is in `.local/ui-polish-qa/qa-report.json`. These directories are intentionally untracked. This report records browser verification, not Mike's acceptance of the design.

The final visible review used Mike's existing Chrome tab on port 9222. It caught and repaired a shared-question text rendering issue after the independent pass. The refreshed question is visibly rendered in `evidence/user-tab-final.png`. Temporary hosts on ports 8175 and 8176 and test browsers on ports 9333 and 9334 were stopped; the live page and Mike's Chrome remain open. See `evidence/cleanup.json`.

## Real user interaction checks

Independent real-Chrome checks passed at 1440 by 1000, 900 by 700, and 390 by 844. The commands and observed state are recorded in `qa-report.json`.

- Scope hover preserves the CRDT revision. Deliberate scope and subject changes reach the host with human provenance. Camera movement preserves conversational scope.
- Node and frame double-click open the selected component. Tool Router opens Agent Planner, Tool Router, and Approval Gate. Nested navigation and Back restore the prior view.
- The walkthrough collapses, expands, follows actual branches, and preserves the chosen target and collapsed preference across reloads.
- Toolbar buttons and shortcuts 1 through 8, marquee selection, Space-drag, wheel pan, and modified-wheel zoom work. A labeled rectangle and an arrow bound to Tool Router appear in host read-back.
- Details and provenance open on demand. Closing the dock restores the full canvas width. Keyboard navigation follows the currently visible tabs.
- Compact tooltips, View and Ink menus, the Frame action, and presence controls remain within the tested viewport.

`python3 /tmp/samepage-polish-20260906/live-proof.py` additionally drove the live surface. Its saved copy is `evidence/live-proof.py`; results are `live-proof.json` and `live-nav-delta.json`. Navigation, separate diagrams, prepared review, the actual G8 record, and the mobile decision dialog passed. The complete shared snapshot before and after the navigation sequence was identical. The live proposal remains unconfirmed.

The live page now has separately owned policy, evidence, and lifecycle-state diagrams. Their content comes from the open project's `workflow.json` and `state.rs`, with tool receipts in `authored-view-receipts.json`. The state view makes no transition, initial-state, or terminal-state assertion. Specific user permission rules and implementation behavior are not defined by those source files. The open project is the existing isolated walkthrough project, not an architecture audit of the whole AG-UI repository.

## Decision and enforcement checks

The disposable project on port 8175 was used for confirmation testing. `python3 /tmp/samepage-polish-20260906/decision-confirm.py` prepared a real review, inspected the G8 record, edited the title, verified that editing invalidated consent, reviewed again, and confirmed. The actual G8 adoption completed with `active_passing` and exit code 0. See `decision-proof.json` and `g8-adoption.json`.

A negative control added an unreviewed enum variant to the disposable `state.rs`. `g8 check --enforce --json` refused it. Restoring the exact reviewed bytes returned exit code 0. See `g8-negative.json` and `g8-restored.json`. Earlier drafts whose page or scope had changed were refused during review.

## Code and visual audit status

- `node --test examples/same-page-atlas/tests/explore-graph.test.mjs`: four tests passed. They cover descendant and immediate-neighbor boundaries, missing targets, ancestor exclusion, immutability, and refusal to advertise a broad tour as detail for every object.
- `node --check` passed for the changed JavaScript entry points. `git diff --check` passed.
- `python3 dev/agent-harness/check.py --json`: `ok: true`, no errors. This is configuration evidence, not compilation evidence.
- `credo-lint` on the rewritten scope and decision files reported no hard violations. Its warnings identify JavaScript negation operators.
- `impeccable detect examples/same-page-atlas/static/workspace-polish.css --json`: clean source scan.
- The rendered Impeccable audit is **not green**. Its single-URL CLI timed out waiting for network idle on the persistent event stream and misleadingly exited 0. The supported browser API with `waitUntil: load` completed both viewport scans and retained the findings in `impeccable-rendered.json`. Findings include undersized text in hidden controls, clipped canvas containers, closed-menu occlusion, and hidden content at overview zoom. The Workspace menu was separately opened and its visible buttons verified by hit testing. No audit rules or project gates were suppressed.

The tested controls do not establish complete Excalidraw parity. Eraser and undo are not implemented by this change. The configured model provider remains `none`; this work uses real shared-page tools and browser actions and makes no claim of a live model response. Preexisting worktree changes were retained. At verification time, no commit or push had been made.
