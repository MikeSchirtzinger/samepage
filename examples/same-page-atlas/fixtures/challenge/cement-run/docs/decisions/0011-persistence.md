---
id: adr-0011
status: accepted
date: 2026-09-04
scope: same-page-atlas
tags: [atlas, cement, g8]
supersedes: []
---

# Persistence and Activity journal

## Context

- [0013c9b84b30dc73-00000001] "Persistence" tone=concept status=agreed
- [0013c9b84b30dc73-00000002] "Activity journal" tone=concept status=agreed
- [0013c9b84b30dc73-00000003] "Persistence" -> "Activity journal" : stores events in

## Decision

- [0013c9b84b30dc73-00000004] basis=verified. "The activity journal keeps at most 800 events in memory" Source: crates/ag-ui-surface/src/activity.rs:28@37dbb17a09005b3ca9ae4ea6309c6964a48a693d.
- [0013c9b84b30dc73-00000005] basis=inferred. "Persistence and the activity journal form one operational boundary" Source: crates/ag-ui-surface/src/activity.rs.
- [0013c9b84b30dc73-00000006] basis=assumed. "The retained event cap is sufficient for this example" Source: none.

## Consequences

- ATLAS-PERSISTENCE-01 watches claim 0013c9b84b30dc73-00000004 on "Persistence" at crates/ag-ui-surface/src/activity.rs:28@37dbb17a09005b3ca9ae4ea6309c6964a48a693d.
