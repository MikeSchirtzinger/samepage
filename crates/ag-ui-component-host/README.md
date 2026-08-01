# ag-ui-component-host

Wasmtime-backed adapter between an `agui:component/extension-component`
component and the existing `ag-ui-surface` runtime.

This first host is deliberately deny-by-default:

- the expected descriptor is policy-authorized before component compilation;
- the component's exported descriptor must exactly match that expectation;
- component imports are pinned to the minimal WASI 0.2.6 interfaces required
  by the Rust guest; socket, random, HTTP, and undeclared custom imports fail;
- WASI receives no arguments, environment, preopened directories, or network
  grants, and its standard streams are closed or discarded;
- fuel, linear memory, instance count, wall time, and output size are enforced;
- action input continues through `ag-ui-surface`'s shared JSON Schema gate;
- component output JSON is size-bounded and schema-validated;
- failed actions roll back to a pre-invocation snapshot, and query actions are
  snapshot-checked for state purity;
- a trapped/poisoned Wasmtime instance is recreated and restored before the
  host accepts another action;
- undeclared returned events fail closed until a typed host event adapter exists;
- snapshot and restore pass through the same bounded call machinery.

It does not yet implement filesystem, network, secret, model, peer, or event
brokers. Supplying a capability grant to this host is rejected rather than
pretending the capability is available.

The current `ActionDef` closure does not carry turn context, so the component's
`context-json` input is `{}` in this adapter. A later runtime context envelope
must be typed and bounded before that field receives live data.
