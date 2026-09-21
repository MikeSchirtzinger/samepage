# Visual audit

`run-audit.mjs` is the portable runner: it takes one or more `--url`
arguments, scans each at desktop (1440x1000) and mobile (390x844), and checks
both impeccable's own antipattern rules and a repository-specific
word-wrap-collapse rule (a text run whose box is narrower than its own
longest word, or a short string that wraps past four lines). It fails on an
incomplete audit rather than reporting a false green. `recorded-audit.mjs` is
a smaller single-file alternative that only runs impeccable's built-in rules
against one URL, useful for a quick manual check.

Neither script starts a browser or the atlas itself. Start both first.

## Start and reproduce

Read the repository and example `AGENTS.md` files first. `build-web.sh`
builds the browser WASM replica and requires `wasm-pack` and `wasm-opt`.
Then run the atlas:

```bash
cargo run -p same-page-atlas
```

Open a real Chrome with remote debugging (the Browser skill's
`browser-start`, or `chrome --remote-debugging-port=9222`), then run the
portable audit against it:

```bash
node examples/same-page-atlas/tests/visual-audit/run-audit.mjs \
  --url http://127.0.0.1:8098/ \
  --output .local/visual-audit/current
```

`IMPECCABLE_ROOT` points either script at an impeccable checkout; without it
they resolve a global `npm install -g impeccable`. `recorded-audit.mjs` also
reads `AUDIT_URL`, `BROWSER_URL`, and `AUDIT_OUTPUT` (all optional, defaulting
to this app's own port, `9222`, and `.local/visual-audit/recorded-audit.json`).

A new checkout needs its own runtime state; a fresh empty board will not
reproduce any specific finding from an earlier polish pass; run the audit
fresh against whatever the board actually holds.

## What counts as green

Diagnose each finding, preserve necessary camera clipping (the canvas and
minimap boundaries intentionally clip their contents; node notes and source
paths can be hidden at overview zoom, which is not itself a finding), and
prove actual visibility and interaction. Do not obtain green by globally
disabling rules, removing useful information, or weakening shared-state
invariants.

## Preserve these behaviors

- All four themes persist locally without changing shared-document semantics
  or semantic category colors.
- Workspace, theme choices, Review, Decisions, source details, and zoom
  controls stay reachable and legible on mobile.
- Whole diagram opens undimmed. Focus step and Next object deliberately enter
  focus. Whole diagram restores the overview.
- Review and MCP `atlas_review` agree on source evidence, claim state, and
  recorded decision trade-offs. Source readability and human acceptance
  remain separate.
- Human edits remain browser-CRDT writes. Camera, theme, and navigation do
  not become semantic edits. Preserve source-reference validation and the
  measurement provenance contract.

Finish with raw before/after audit JSON, real desktop/mobile interaction
evidence, visually inspected screenshots, relevant compiler/tests, and an
explicit list of anything still unproven. Human acceptance remains separate
from a green automated audit.
