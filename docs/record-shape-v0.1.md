# Record shape, version 0.1

The neutral record of who did what under whose authority. This document is the
versioned shape. `crates/ag-ui-record` is the serde model and the validator
that enforce it.

## Why a record exists

The runtime already writes an activity journal and a CRDT board, but neither
answers the record's central question on its own. The journal keeps a
bounded ring of events (the `MAX_EVENTS` constant in
`crates/ag-ui-surface/src/activity.rs`) whose actor is a caller category only,
with no participant id, label, or responsible party. The CRDT layer signs
authorship as participant-claimed: the browser replica hardcodes
`Author::Human`, and no websocket identity gate existed when this record was
written. The identity registry is in-memory, so participant ids inside durable
records have no resolvable referent after a restart. Transcripts are never
persisted, so the record cites what was done, not what was said.

This record assembles the durable pieces into one shape and labels how much of
each the host actually verified. Honesty is encoded in the schema, not
asserted in prose.

## The trust rule

Every field group carries exactly one trust label:

- `host_verified`: a host route stamped the value from an identity it resolved.
- `participant_claimed`: a replica asserted the value; the CRDT browser replica
  hardcodes `Author::Human`.
- `unverifiable`: no durable writer exists, so the field is left absent rather
  than invented.

Two labels are fixed by what the field is. CRDT authorship is
`participant_claimed`. Route-dispatched writes are `host_verified`. The two
document scopes name exactly those two cases: `atlas`, the CRDT document, is
`participant_claimed`; `board`, the route-dispatched document, is
`host_verified`.

A field group the host could not verify is omitted, not defaulted. The
`responsible` field of an attached agent is always absent on MCP attach today;
the record does not auto-default it to the sole local person, because the
record must not vouch for what the host did not verify.

## The sections

The record is one JSON object with these top-level fields:

- `record_version`: the string `"0.1"`.
- `session`: `journal_id`, `observer_id`, `started_at_ms`, `trust`. All
  `host_verified`; the observer mints them.
- `actors`: an array. Each entry has `actor_id`, `kind`, `label`,
  `participant_id`, `principal_key`, `responsible`, `trust`. The first three
  are `host_verified`, stamped from the resolved caller. The last three are
  optional and absent in 0.1 records.
- `events`: an object with `trust`, `coverage`, and `events`. The `trust` is
  `host_verified` and is inherited by every event. `coverage` is the
  per-category coverage the activity snapshot reports. `events` is the
  ActivityEvent schema 1 list, verbatim.
- `documents`: an array of `scope`, `revision`, `trust`. `scope` is `board`
  (a u64 revision) or `atlas` (a yrs state vector, URL-safe base64).
- `settlements`: an array of `receipt`, `obligations_source_sha256`, `trust`.
  `receipt` is the cement receipt as the cement step wrote it.

A minimal skeleton:

```json
{
  "record_version": "0.1",
  "session": { "journal_id": "activity-...", "observer_id": "host-...", "started_at_ms": 0, "trust": "host_verified" },
  "actors": [ { "actor_id": "surface-human", "kind": "human", "label": "Human", "trust": "host_verified" } ],
  "events": { "trust": "host_verified", "coverage": [], "events": [] },
  "documents": [
    { "scope": "board", "revision": 4, "trust": "host_verified" },
    { "scope": "atlas", "revision": "AQIDBA", "trust": "participant_claimed" }
  ],
  "settlements": []
}
```

### The events list

The `events.events` list is ActivityEvent schema 1 as the runtime writes it
(`crates/ag-ui-surface/src/activity.rs`): `sequence`, `at_ms`, `category`,
`kind`, `actor`, `observer`, `correlation`, `outcome`,
`state_revision_before`, `state_revision_after`, `detail`. Field names are
camelCase. Category and outcome values use the journal's snake_case
vocabulary. A real session's events slot in unchanged.

### The settlement receipt

`receipt` is the cement receipt schema as it exists: `schema_version`, `kind`,
`cemented_at`, `atlas_document_revision`, `board`, `assertions`.
`obligations_source_sha256` is the sha256 of the canonical receipt bytes, the
same value the obligations document carries in `meta.source_sha256`. Canonical
bytes are the receipt JSON pretty-printed with two-space indent and sorted
keys, followed by one trailing newline. The validator recomputes this from the
stored receipt object, so the link between the two artifacts is re-verified
rather than trusted.

## The validator

`crates/ag-ui-record::validate` runs these checks in order and reports at most
one violation per check:

1. Sequence monotonicity: event `sequence` values strictly increase.
2. at_ms sanity: `started_at_ms` is positive, and every event's `at_ms` is
   positive and not before the session start.
3. Actor referential integrity: every event's `actor.id` appears in
   `actors[].actor_id`.
4. Correlation event id uniqueness: no two events share a
   `correlation.event_id`.
5. Coverage honesty: a category marked `uncovered` carries no events and an
   `observed_events` of zero.
6. Settlement re-verification: for each settlement, the recomputed receipt
   sha256 equals `obligations_source_sha256`; every assertion is `settled`,
   signed off `by` "you" with `mark` "agree"; and the signoff `board_revision`
   does not predate the authored `board_revision`.
7. Trust label presence: every field group has a trust label, and the two
   document scopes carry their fixed labels.

## The witness sidecar

The `witness` binary carries this validator across a process boundary for hosts
that do not embed the Rust library:

```text
witness validate <path/to/record.json> [--json]
```

It exits 0 when a record parses at version 0.1 and has no violations. It exits
1 when validation reports any violation. It exits 2 for command usage, file
read, JSON parse, or unsupported-version errors that never reach validation.
With `--json`, stdout contains `record_version`, `ok`, and `violations`. Each
violation contains `check`, using the validator's existing check slug, and
`message`.

A host must supply this floor:

- `record_version` set to `"0.1"`.
- A session with `journal_id`, `observer_id`, a positive `started_at_ms`, and a
  trust label.
- Actors with `actor_id`, `kind`, and a trust label. Every event actor id must
  name one of these actors. Unverified `participant_id`, `principal_key`, and
  `responsible` values stay absent.
- Events with a trust label and an ActivityEvent schema 1 list. Sequence values
  strictly increase, timestamps are positive and no earlier than the session,
  and correlation event ids are unique. An uninstrumented host may supply empty
  events and coverage. A category marked `uncovered` has no events and an
  `observed_events` value of zero.
- Documents with `scope`, `revision`, and a trust label.
- A settlements array. A foreign host may supply an empty array.

Two limits are accepted in v0. The fixed scope table constrains trust only for
`atlas` and `board`; every other scope falls through without added trust
discipline. Settlement verification uses this lab's literal `"you"` and
`"agree"` vocabulary, so nonempty settlements remain lab-only in v0.

## Fixtures

The validator's tests read every JSON file in
`crates/ag-ui-record/tests/fixtures/`. A file named `neg-<check-slug>.json`
must fail exactly the check whose slug it names. Any other file must pass.
Checked in now: one synthetic valid fixture, one synthetic negative fixture
per check, and `real-session-2026-08-13.json`, a record assembled from a real
session (its raw sources sit beside it under `real-session-sources/`,
including a human-settled cemented claim whose receipt hash the validator
re-verifies). The real record passed with zero validator changes. Any further
session record lands the same way: drop one file into that directory, with no
code changes.

The synthetic receipt hash was produced by the same canonical serialization the
validator uses. The checked-in valid fixture's `obligations_source_sha256` is
`8a44dbf180045743f9cd42b5465ae99a944743939baed9ceda69ad354543f886`.
