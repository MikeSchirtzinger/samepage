# SamePage Board

An optional, independent capability for assertions, questions, decisions,
relations, evidence and human verdicts. Portable Rust and WebAssembly share
validation, transitions, projection, read-back, delta generation, old-JSON
migration and advisory export. Board has no capability dependencies and does
not link the document engine or retained Atlas packages.

Board state remains a native last-writer-wins `board.json` beside the app's
document. It is not a CRDT record. The host authenticates callers, injects the
clock and participant, saves before broadcasting, and keeps separate reader
cursors in memory. Restart preserves assertions, authors and marks, and clears
reader cursors. Legacy authors survive migration; missing sign-off timestamps
and participants are never invented.

The host retains authenticated participant display names in the Board's
additive `participants` registry. Marker display compares the stored signer ID
with the reader ID: `you` for that reader, the registered name for others.
Missing signer identity or a missing registry entry is reported as unavailable;
names are never inferred from the author or the `Author::You` role. That role
still controls human transitions and fresh sign-off, independently of display.

Agents use `board_read`, `board_revision`, `board_assert`, `board_retract` and
`board_subject`. Humans use `board_compose`, `board_mark`, `board_unmark`,
`board_seen`, `board_clear` and the read-only `board_snapshot`. The server
enforces the audience for both preferred names and aliases. An assertion batch
with a semantic error publishes its valid prefix and reports the failing entry
and what landed. Malformed input is rejected before any writes.

Only a human agreement settles a claim. A rewrite preserves the human mark but
makes its sign-off stale; a new agreement is required. Replacing an agreement
with another mark or unmarking reopens the claim. Retraction removes transitive
dependents, so relations cannot be left pointing at deleted relations.

`board_cement_preview` (alias `atlas_cement_propose`) lets an agent inspect an
advisory bundle. Human-only `board_cement` (alias `atlas_cement`) publishes the
obligations and receipt together in a new directory. Every current entry needs
fresh agreement. These exports retain the old receipt schema and obligation
identifiers for compatibility and never install enforcement gates.

## ABI and host composition

The standard ABI v2 exports are present, including `descriptor`, `project`,
`digest`, `validate`, `changes` and `plan`. A transient `snapshot_view` adapts a
native Board to that interface; the descriptor claims no document kinds or
properties. Query plans return one empty Batch with read-back. Mutation plans
refuse with `native_store_required`, since a document Batch cannot publish a
native JSON store. Additional JSON exports are `transition`, `project_board`,
`validate_board`, `delta`, `migrate`, `readback` and `advisory_preview`.
`project_board_for` and `readback_for` additionally accept a JSON reader ID
(or null); `snapshot_view_for` supplies the same reader to the standard ABI.
The human-only `board_snapshot` includes authenticated viewer-relative read-back
without advancing its cursor. A transition
returns the resulting Board and any semantic-prefix error for the native host
to publish. The browser uses the verified WASM projection, with writes through
authenticated host actions.

`await_board` and the composed `await_input` remain native. The combined wait
wakes for Board writes, other participants' document commits, human semantic
attention, browser validation failures and runtime chat. Notifications are
registered before checking the corresponding state.

The browser loads Board after its first action, opening the panel, or receiving
persisted Board data. Its named unavailable panel leaves Map usable when the
optional artifact is omitted or fails verification. It creates no document
replica.

## Verification

```sh
cargo test --locked -p samepage-board -p samepage-app
cargo clippy --locked -p samepage-board -p samepage-app --all-targets -- -D warnings
cargo clippy --locked -p samepage-board --target wasm32-unknown-unknown --lib -- -D warnings
OUT=/path/to/private-results bash crates/samepage-board/tests/wasm/run-check.sh
```

The equivalence harness compares all export envelopes against native results.
Its ignored integration test runs only when explicitly selected by the script;
it is not counted as a passing test in the normal suite.
