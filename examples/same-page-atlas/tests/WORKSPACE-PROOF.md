# Workspace interaction and tracing

The workspace keeps the shared picture visible while exposing discussion, exploration,
claims, and activity on demand. A person can review a walkthrough step locally or use
its explicit controls to advance the shared cursor. Presentation and search are local
views of the existing document.

The activity drawer reads the real host journal. Action flame graphs show measured
lookup, authorization, validation, construction, and execution phases. Atlas contributes
its opaque CRDT state vector to the before/after evidence. The browser flame graph
shows instrumented WASM calls nested inside the synchronous page render, using
monotonic start times and durations.

These clocks are separate. The drawer does not infer an end-to-end causal trace.
Aborted dispatcher futures lack a completion record. The bounded browser ring reports
eviction and resets on reload. Existing journal events without timing remain untimed.
Elapsed time includes waits and is not sampled CPU utilization.

## Real interaction proof

Use a recent Node runtime with native WebSocket support and the repository's real
Chrome browser tools. Build the native host and WASM from the same checkout, start
the host with the intended state paths, then open a dedicated browser tab.

The proof expects the imported agent-tool-call fixture and its named
`agent-tool-call-workflow` exploration already on the page. That exploration is in the
local review document. The fixture remains under `fixtures/archify/`; its claims retain
their original evidence status. This test does not manufacture a replica or intercept
host responses.

```sh
cargo build -p same-page-atlas
examples/same-page-atlas/build-web.sh
browser-start
eval "$(browser-tab)"
browser-nav http://127.0.0.1:8099/
# AGUI_MCP_TOKEN must match the running local host.
node examples/same-page-atlas/tests/workspace-proof.mjs .local/workspace-proof/review
```

The test temporarily drags the imported Trace Log card through trusted Chrome input
while an actual MCP call changes its note. It reads both changes back, restores the
card's authored position and note, sends explicitly labeled verification messages
through the real conversation, overlaps two MCP sessions to verify caller attribution,
and detaches both MCP sessions. The test messages remain
in the transcript. Existing marks, claims, and drawings must remain equal.

The output includes screenshots, concurrent-edit evidence, a real rejected action,
conversation pickup, host/browser trace snapshots, source hashes, and a receipt.
`humanAcceptance` remains false: browser automation does not establish human UX
acceptance. A failed run remains failed, and a later run gets a new output directory.

## Focused checks

```sh
cargo test -p ag-ui-surface -p same-page-atlas-core -p same-page-atlas
cargo test -p same-page-atlas-web --lib
cargo clippy -p ag-ui-surface -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings
cargo clippy -p same-page-atlas-web --target wasm32-unknown-unknown --no-deps -- -D warnings
node --test examples/same-page-atlas/tests/activity-model.test.mjs
python3 dev/agent-harness/check.py --json
```

The activity model tests are unit tests of timing arithmetic, distinct from the live
browser/MCP proof. Routing tests check obstacle avoidance, moved endpoints, invalid
geometry, and explicit unavailable routes. Unavailable routes remain visibly dashed
and are reported as layout problems; they do not count as successful routing.
