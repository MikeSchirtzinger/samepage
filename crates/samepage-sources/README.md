# Sources

Sources owns `node.path` and `node.lines` over Map. Its ABI v2 `plan` returns
one Batch, including the Map creation and Sources binding entries for a
`card_place` coordinator. The native host checks every attempted binding
before committing, including updates that change only the line range.

The portable projection takes raw readability, current hashes, pinned
baseline hashes, extra bindings and gate facts. It computes primary-first
deduplicated bindings and the retained tier precedence: proposed, gate state,
unbound or unreadable claimed, drifted, verified. Source readability is not
verification of a card's prose or arrows.

Repository access, canonical path policy, Git reads and baseline persistence
stay in `apps/samepage/src/host/sources.rs`. The optional browser module loads
on the first binding, source action or viewer opening. It uses the verified
WASM to compute chips and numbered source read-back from fresh host facts.

Select `sources=crates/samepage-sources=apps/samepage/modules/sources.js` in
the app build. When Diagrams is also selected, the build enables its
`integrated-sources` descriptor so Sources becomes the sole binding owner.
The standalone Diagrams artifact and its existing equivalence cases remain
unchanged.

`source_list` and `source_read` accept legacy `repo_list` and `repo_read`
aliases. `sources_read` reads fresh facts and portable tiers without changing
the document. Run `tests/wasm/run-check.sh` for the explicit native/WASM
equivalence harness; its ignored Cargo test is not a substitute for that run.
