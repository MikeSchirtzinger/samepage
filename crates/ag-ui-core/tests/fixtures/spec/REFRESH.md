# Refreshing the vendored EventType snapshot

`event_types.snapshot.json` is a checked-in copy of the current AG-UI spec's
canonical event-type names, sourced from the upstream TypeScript SDK's
`EventType` enum:

    sdks/typescript/packages/core/src/events.ts  (ag-ui-protocol/ag-ui, upstream/main)

It exists so `tests/spec_conformance.rs`'s drift-alarm test
(`event_type_matches_canonical_spec_snapshot`) can run in ordinary Rust CI
without a TypeScript toolchain. The test compares the Rust `EventType` enum
(minus the 5 intentional TimeTravel extension variants — see that test's
`TIME_TRAVEL_EXTENSIONS` allow-list) against this file.

## When to refresh

Whenever the upstream AG-UI spec's `events.ts` changes — periodically, or
whenever doing a spec catch-up pass on `ag-ui-core` (see `event.rs`'s
"spec catch-up" doc comments for the last one).

## How to refresh

1. In a checkout of `ag-ui-protocol/ag-ui` (e.g. `~/dev/ag-ui`, which has
   `upstream` wired to the canonical repo), fetch and note the commit:

   ```
   git fetch upstream
   git rev-parse upstream/main
   ```

2. Extract the `EventType` enum's member names:

   ```
   git show upstream/main:sdks/typescript/packages/core/src/events.ts \
     | sed -n '/^export enum EventType {/,/^}/p' \
     | grep -oE '"[A-Z_]+"' | tr -d '"' | sort -u
   ```

3. Replace `event_types.snapshot.json`'s `event_types` array with that list,
   and update `upstream_commit` / `fetched`.

4. Run `cargo test -p ag-ui-core --test spec_conformance`.
   - **Fails** → `EventType` (and likely `event.rs`'s structs) have drifted
     from the spec; that's the alarm doing its job. Do a real catch-up pass —
     add/rename/remove the affected `EventType` variants, `Event` variants,
     and structs, add fixtures for anything new, then rerun.
   - **Passes** → the crate is already in sync; just commit the refreshed
     snapshot.

## Design note

This file is intentionally not fetched live in CI: `ag-ui-core` has no
TypeScript toolchain dependency, and a live network fetch inside a test isn't
reproducible. Committing the snapshot turns drift into a *visible, deliberate*
refresh step instead of a silent, invisible one — which is the point of this
test existing at all.
