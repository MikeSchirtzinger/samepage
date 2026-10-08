# Architecture

Optional ABI v2 capability depending on Map. Its independent WASM artifact owns
`architecture-proposal` records and projects comparisons; it never links Yrs or
the native app. Native and WASM planners return one Batch. There is no native
half: graph capture, semantic SHA-256, bounded validation, upstream BFS and
read-back are portable functions of the document View.

Call `architecture_read` first. `architecture_propose` captures the current graph
or revises `previous` against its exact `baseline_revision`. Every revision is a
separate record, retaining concurrent alternatives and host-stamped authors.
Geometry, sizing and author stamps do not change the semantic baseline. Labels,
source paths, containment and relationships do. Typed events include guards.

`architecture_decide` records a human accepted/rejected verdict. Server authority
protects actions, batches, replica frames and unchanged-value attempts. Captured
definitions and proposal history cannot be overwritten or deleted. A verdict
never applies the graph or activates a gate. Map removal preserves captured
snapshots and makes their baseline stale.

Comparison colours retain the Atlas meaning: green + added, red − removed,
amber direct endpoints, blue potential upstream impact. Incoming-arrow paths
are potential dependency impact only when arrows mean depends on. Source
freshness and runtime behavior require separate checks.

The old decision-draft restored-peer ordering test uses schema-only test support;
Cement owns those records and validation. Review's architecture slot remains
null pending composition by the integration lane; Review does not depend upward
on Architecture.

Validate with `cargo test --locked -p samepage-architecture`, then
`bash crates/samepage-architecture/tests/wasm/run-check.sh` for real export
comparison, including plan, validation, comparison and read-back.
