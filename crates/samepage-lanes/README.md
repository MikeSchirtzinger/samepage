# Lanes

Lanes depends on Sources. The native adapter runs `samepage-extract` and
computes the retained stable ids from path, line and kind. Portable WASM
reconciles the scan with primary and extra bindings, matches within five
lines, retains explicit claims and removals, groups findings, and produces
bounded strip read-back. Static findings describe source patterns; they do
not prove that a process is running.

`lane_claim` (legacy `atlas_claim_lane`) adds a host-stamped `binding` record
without replacing the card's primary source. Both `node_id` and the legacy
`card_id` input are accepted; contradictory ids are refused. The native verifier rescans and
checks its source excerpt, file hash and baseline before the one Batch lands.
The browser includes the inspected hash so a changed finding requires a new
scan. Sources receives extra bindings and pins as raw native facts, keeping
the dependency graph acyclic.

`lanes_read` scans on demand, preserving the old host route's fresh-scan
behavior. The optional browser strip is collapsed initially, retains the
unresolved count, provides group selection and 20-item pages, reads each
finding's current excerpt, and claims it with the selected card. A disappeared
finding persists as an unresolved `SOURCE VANISHED` observation. An actual
published removal obligation uses `REMOVE met` or `REMOVE unmet` from gate
facts; disappearance alone never reports a met gate.

Select `lanes=crates/samepage-lanes=apps/samepage/modules/lanes.js` alongside
Sources. It loads on the first lane action, strip opening or stored binding.
Run `tests/wasm/run-check.sh` for explicit native/WASM equivalence.
