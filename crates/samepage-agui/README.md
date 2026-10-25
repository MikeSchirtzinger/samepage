# samepage-agui

Rust types for the AG-UI protocol as SamePage uses them: events, messages,
state, tools, context, IDs and interrupts, all serialized with `serde`.

* [Event types](src/event.rs)
* [Stream assembly](src/assembly.rs): the event lifecycle state machine,
  compiled natively for tests and to wasm32 for the browser
* [Message types](src/types/message.rs)
* [State trait bounds](src/state.rs)
* [Input types](src/types/input.rs)
* [Tool type](src/types/tool.rs)
* [Context type](src/types/context.rs)
* [Interrupt type](src/types/interrupt.rs)
* [ID newtypes](src/types/ids.rs)

`tests/spec_conformance.rs` compares the `EventType` enum against a snapshot
of the upstream spec; `tests/fixtures/spec/REFRESH.md` says how to refresh it.

## Origin and license

This crate is derived from the `ag-ui-core` crate of the AG-UI Rust SDK
(<https://github.com/ag-ui-protocol/ag-ui>, `sdks/community/rust/crates/ag-ui-core`).
It carries a different name because `ag-ui-core` on crates.io belongs to that
SDK. The upstream MIT notice and this repository's MIT notice are both in
[LICENSE](LICENSE).
