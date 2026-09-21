# Deterministic page validation proof

The page computes layout diagnostics in the same Rust core used by the host.
Agent writes return a tool error when the resulting page fails validation.
Late measurements, browser exceptions, and extension startup failures wake
`await_input` with a tool error. The header exposes the current result.

## Checked behavior

| Check | Evidence |
| --- | --- |
| Native regression suites | `cargo test -q -p same-page-atlas -p same-page-atlas-core`: 177 host and 381 core tests passed; one existing host test ignored. |
| Strict compiler lint | `cargo clippy -q -p same-page-atlas -p same-page-atlas-core --all-targets -- -D warnings`: passed. |
| Native and browser builds | `cargo build -q -p same-page-atlas` and `./examples/same-page-atlas/build-web.sh`: passed. |
| Real Chrome and MCP | `node .local/architecture-compare/validation-proof.cjs`: 17 assertions passed against an isolated copy on port 8180. |
| Existing visible page | `node .local/architecture-compare/validation-handoff.cjs`: saved positions, relationships, proposal and minimap preserved; header disclosure opened with pointer input; no browser exceptions. |
| Agent read-back on the visible page | `python3 dev/samepage call atlas_validate --url http://127.0.0.1:8177 --args '{}'`: passed with a current browser render, zero unmeasured objects and no errors. |

The isolated browser test intentionally creates an overlap through MCP, repairs
it, then creates an overlap from the browser while a real MCP wait is parked.
Both paths return `isError: true` with affected ids. The authoring error also
returns `operation_applied: true`. The test then injects actual JavaScript
exceptions, including a caught extension activation failure, and verifies both
error delivery and successful reload recovery. These faults were confined to
the isolated copy. Missing browser proof returns pending with a tool error.

Native tests also cover stale and out-of-order reports, active browser error
retention, expired browser capacity, text-shape measurements, and content
identity after deletion. Expired observations cannot certify a page.

## Local evidence

The run artifacts are under `.local/architecture-compare/evidence/`:

- `validation-receipt.json` retains actual MCP responses and browser assertions.
- `validation-live.json` retains the before/after shared-page comparison.
- `validation-live-mcp.json` retains the current page's tool result.
- `validation-source-hashes.json` identifies the source and running artifacts.
- `validation-overlap.png`, `validation-exception.png`,
  `validation-startup.png`, and `validation-live.png` show the actual page.

These checks establish the named geometry and error-feedback behavior. They
do not establish human acceptance or replace visual review for readability.
