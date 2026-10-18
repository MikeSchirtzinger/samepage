# Sketch

Sketch owns ordinary shapes, pen strokes, captions and bound arrows in the shared document. Its pure Rust planners and geometry run under `samepage.capability.v2` in native code and WASM. It depends on Map and has no document, filesystem, process or identity dependency.

`sketch` (`atlas_sketch`) creates or revises ordered drawings. Earlier `shape-N` references may be used by later arrows and frames. A valid prefix commits once; the result names the refused element and every later element that was not attempted. `sketch_read` reports ENCLOSES, CROSSES-OUT, SAYS NEAR and binding relations without positions. `sketch_export` exports the drawn layer; `sketch_import` (`atlas_import`) preserves supported Excalidraw styles, groups, frames and curved arrow bindings and names unsupported or loose elements. `shape_remove` detaches surviving bindings.

`shape_lift` creates a Map card at the drawing's box, consumes its label or its single enclosed text, retargets arrows and constraint members, and deletes the consumed drawings in one batch. Map applies its normal size limits. Connectors, wordless drawings and ambiguous regions are refused. The native Sketch adapter adds the existing Review mark-retarget plan while holding the document lock; Review is optional and an affected mark requires its owner. There was no legacy lift action alias: the old browser called lift directly.

Shared writes carry host-stamped identities. Drawing tools keep pointer previews and selection local, then submit Sketch plans. A lifted card receives the person's byline and the local selection. Historical unsupported forms remain visible as named unsupported drawings and cannot be silently revised into supported forms.

Run `cargo test --locked -p samepage-sketch` for the retained and composition tests. Run `tests/wasm/run-check.sh` with wasm-bindgen 0.2.126 for 38 native/WASM envelope comparisons, including import, lift, export, readback, deletion with prior binding geometry and local measured-card projection. The ignored harness only counts as proven when that script is run.
