# trace

Optional ABI v2 diagnostic spans, independent of the document journal. No
capability depends on trace, and trace depends on no document capability.

`Ring` retains 256 typed records with monotonic sequences, exact overflow
counts, namespaced span identities, parent links and recursive credential-key
redaction. Each record is bounded to 16 KiB. `operate` is the same record,
snapshot, clear and benchmark implementation in native Rust and WASM.
Benchmark validates 1..10000 iterations and 1..30 samples before any mutation;
its clock is injected for deterministic equivalence and monotonic in runtime.

The ABI `plan` export returns one empty Batch for `trace_read`, `trace_clear`
and `trace_benchmark`. These affect observational state, never CRDT records.
The native adapter performs the validated operation on its process-local ring;
`trace_operation` performs it on the separately loaded browser ring. The shared
`ActivitySink` hook collects native spans; the host-provided `__samepageTrace`
hook collects each independently compiled WASM artifact. Span IDs carry native
or WASM origin and capability namespace. Cross-artifact collection needs no
WASM-local subscriber or second Yrs document.

Host operational events use the existing durable ActivityFeed, separate from
span rings. The drawer preserves its timeline, filtering, coverage, export and
recorded-span timing; the WASM view includes every retained capability. User
Timing marks and measurements use span identifiers and stable names, never
record fields. Rings start empty on process/page restart. Disabled trace creates
no ring or activity file, refuses activity routes with 404, and displays a drop
counter. The always-on document journal and host author stamps are unaffected.

Run `tests/wasm/run-check.sh` for native/WASM equivalence and
`node tests/wasm/cross-artifact.mjs <selected-artifacts>` for a real doc/map/trace
cross-artifact capture.
