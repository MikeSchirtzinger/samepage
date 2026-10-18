# SamePage diagrams

A portable capability that plans typed hierarchies and state machines over Map
records. The native host and the independent WebAssembly artifact run the same
parser, layout, routing, projection and read-back code.

`diagram_draw` (legacy alias `atlas_diagram`) returns one ABI v2 batch. Map
creates the nodes and edges and writes geometry, styles and containment;
Diagrams writes diagram kinds, independent initial/terminal roles, stable state
order, events, guards and transition order. Container references are resolved
before publication. Typed transitions always create fresh edges, including
self-loops and parallel transitions. Ordinary Map links retain their distinct
endpoints and pair-upsert policy.

Drawing appends to the map by default, matching the retained editable diagram
action. Explicit `clear: true` replaces Map nodes and edges in the same validated
batch. A refusal preserves the existing document. Map constraints that lose a
member retain their existing broken-membership diagnostic policy.

The app loads this capability on demand after a diagram appears or a diagram
action is used. Its renderer paints roles and routes transitions from the WASM
projection. The `project_with_bounds` export re-routes against the actual local
card rectangles after role labels render; those measurements never become
shared writes. The document and Map remain shared; the module creates no replica.
An omitted or unavailable Diagrams artifact produces a named notice while
ordinary Map operations remain available.

## Source boundary

For this D1 composition, Diagrams provisionally claims `node.path` and
`node.lines`. The app registers a native verifier that checks their effective
binding on every write route, including lines-only edits and browser frames,
before a commit. Source reads stay native, project-relative, private-path
filtered, UTF-8 and bounded by the requested range. The portable validator
refuses source writes without matching host facts.

D3 must transfer those property claims and the verifier to the sources
capability before composing it with Diagrams. They cannot be claimed by both.
Reading a range verifies the reference; it does not verify the card's prose.

## Build and verification

```sh
cargo test -p samepage-diagrams -p samepage-map -p samepage-app
cargo clippy -p samepage-diagrams -p samepage-map -p samepage-app --all-targets -- -D warnings
cargo clippy -p samepage-diagrams -p samepage-map --target wasm32-unknown-unknown -- -D warnings
OUT=/path/to/private-results bash crates/samepage-diagrams/tests/wasm/run-check.sh
```

The equivalence script builds the artifact, calls each export in Node, then
compares the complete response with native output for each shared case. Its
explicitly selected integration test is separate from the default test run.

The pure layout/parser and routing modules retain the repository's diagram and
routing tests. Host tests cover atomic source-backed drawing, nested hierarchy,
styles, role independence, exact transition/problem/change sentences,
self/parallel transitions, and Map compatibility.
