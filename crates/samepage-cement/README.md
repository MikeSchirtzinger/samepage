# Cement

Cement depends on Sources, Lanes and Review. Its ABI v2 WASM owns immutable
decision drafts, the shared commitment mode and the `node.cemented` extension.
Portable logic validates explicit checks, binds the whole selection and shared
context, constructs ADRs, obligations and receipts, and detects changed coverage.
Geometry and edits outside the selection do not invalidate existing coverage.

The native adapter verifies source claims and the Git project root, pins reviewed
repository files, stages file writes, backs up G8 publication files, runs real
`g8` 0.1.5 from PATH, and parses its result. It invokes `init`, `ratify`, `attest`
and `check --enforce --json`. CLI execution/protocol errors and CRDT refusal roll
publication back. Structural failure publishes an `active_blocked` receipt;
it never becomes a met gate. The human CRDT stamp follows successful reviewed
publication of the same document revision and selected content. A consumed
review token requires another human preview.

The retained preview, history and publication actions remain human-only. The
agent drafts and calls `decision_cement_propose`. Mode changes are available to
both participants, matching the retained agent tool and human mode route.
Learning/alignment are shared commitment postures; legacy explain/map/cement
values remain accepted. This does not import another product's timeline or
promotion behavior. Files cited as check evidence must be readable when reviewed.

`map_cement` refuses every open/disputed claim and writes unratified, unattested
anchor obligations. `lane_remove` writes a real removal obligation and persists
its Lanes record. Neither silently ratifies intent. The advisory assertion helper
produces prose-only, inactive drafts for the separate Board integration.

The optional browser module loads on first use, displays mode and explicit human
review, and publishes through authenticated actions. Sources receives raw gate
facts from the native composition and renders each badge and reason together on
its next read, without reloading the page. No gate result comes from browser code.

Select `cement=crates/samepage-cement=apps/samepage/modules/cement.js` along with
Sources, Lanes and Review. Run `tests/wasm/run-check.sh` for equivalence, and
`cargo test -p samepage-cement --test host` for real G8 tests (not ignored).
