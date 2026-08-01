# ag-ui-component

Portable data contracts for running an extension under browser WebAssembly,
WASI, or the trusted native host.

This first slice provides:

- a target-neutral component descriptor;
- required and optional capability requests;
- scoped host grants with subset enforcement;
- required execution limits;
- a deny-by-default policy evaluator;
- query-versus-mutation action metadata for host dispatch;
- the initial `agui:component@0.1.0` WIT export contract.

The companion `ag-ui-component-host` crate now adapts this contract to
`ag-ui-surface::Extension` through Wasmtime. This data crate remains independent
of that executor so browser workers and other hosts can reuse the same policy.
The WIT contract parses, this crate builds for native,
`wasm32-unknown-unknown`, and `wasm32-wasip2`, and both companion probes execute
as real WASI components without ambient host filesystem or environment access.

The capability policy is independent of Wasmtime so a browser worker and a
native/WASI host can make the same grant decision before instantiating
target-specific execution machinery.
