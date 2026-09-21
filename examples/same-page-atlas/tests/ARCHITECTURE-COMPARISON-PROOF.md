# Architecture comparison and project facts

The Compare control opens aligned current and proposed relationship maps.
Changes are shared proposals; each revision retains its baseline and predecessor.
The table supports additions, removals and restoration. Component links locate
the relevant card. A changed current map exposes a stale-baseline warning.

The header shows measured Git file and language counts alongside whole-page
component and relationship counts. Expanding it shows branch, revision, Cargo
manifest count, measurement commands and counting boundaries. A project root
without its own Git repository reports repository facts as unavailable.

## Validation on 2026-09-06

Commands were run from the samepage worktree. Runtime evidence is retained in
`.local/architecture-compare/evidence/`; those local files are not shipped.

| Check | Result |
| --- | --- |
| `cargo test -p same-page-atlas -p same-page-atlas-core` | 171 host tests and 370 core tests passed; one existing host test remained ignored. |
| `cargo clippy -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings` | Passed. |
| `./examples/same-page-atlas/build-web.sh` | Real browser WASM built and optimized. |
| `python3 dev/agent-harness/check.py --json` | Passed. |
| `node .local/architecture-compare/proof.cjs` | 17 browser assertions passed, with no console or page errors. Includes shared read-back, reload, mobile and current-map preservation. |
| `node .local/architecture-compare/final-ui.cjs` | Six assertions passed, including naming, stale baseline, selection followed by Review, Paper theme and mobile text size. |
| `python3 .local/architecture-compare/agent-proof.py` | Four live MCP checks passed: proposal publication, unchanged current graph, stale write refusal and stale-baseline detection. |
| Visible Chrome | Both retained pages loaded in a normal window. The comparison and mobile screenshots were visually inspected. |
| Host restart | Existing cards and decision drafts preserved. Decision arrays were compared by stable id because their iteration order changed on reload. |
| Public-text lint and `git diff --check` | No hard text violations or whitespace errors. |

The shared core tests additionally cover independent CRDT peer merges, retained
concurrent alternatives, cyclic dependency traversal, invalid endpoints, source
graph round trips and reader-specific proposal notifications. Decision drafts now
use a canonical order in the shared projection, with a restored-peer regression
test for the ordering discrepancy found during visible verification.

Formatting checks pass for host/web and the changed core files using their
existing style editions. The whole-core formatting command remains non-green
because the baseline mixes formatting styles. The untouched baseline was
checked separately; formatting drift was not expanded into this feature.

## Evidence boundary

Current means the captured card map. The repository code map records declared
Cargo dependencies. Proposed relationships are hypothetical, not implemented
code changes. Upstream impact follows incoming arrows in both versions and is
potential dependency impact only when those arrows mean depends on.

Component addition/removal, drawn connectors, runtime calls, feature resolution
and code freshness are outside this comparison. Source review remains separate.
No decision was accepted or cemented. This feature's browser checks do not
clear the earlier whole-canvas visual-audit findings or establish human acceptance.
