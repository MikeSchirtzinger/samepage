# Panes

An optional `samepage.capability.v2` leaf. Panes uses the shared document and has
no capability dependencies. Its WASM package contains layout, closed content
validation, safe projection, legacy migration and relational read-back. Native
workspace and source facts arrive as data; WASM cannot access files or spawn
processes.

Select it alongside the document and Map:

```sh
apps/samepage/build-capabilities.sh /absolute/artifact/output \
  doc=crates/samepage-doc/web \
  map=crates/samepage-map=apps/samepage/modules/map.js \
  panes=crates/samepage-panes=apps/samepage/modules/panes.js
```

The selected module and its scoped stylesheet load on the first pane record,
Panes tool, or opening the Panes view. A missing or corrupt selected artifact
fails visibly; an omitted optional Panes leaves Map available.

`panes_read`, `panes_await`, `pane_put`, `pane_remove`, `panes_arrange`,
`panes_configure`, `pane_annotate` and `panes_import` are preferred names.
The retained room aliases dispatch to the same definitions and are journaled
under the preferred name. Human-only `panes_present` supplies fresh native
source and catalog facts to the verified browser projection. It is absent
from agent discovery, as is `pane_annotate`.

All mutations take the shared document's string `expected_revision`, obtained
from `panes_read`. Agents place panes by relation (`right of: topic`, `below:
topic`, `start`, `end`) and named size. Rewriting content preserves existing
geometry, marks, notes and points. Pointer gestures plan `pane_put` in WASM
with human geometry or raise; the server rejects these changes from agents,
untrusted batches and replica frames. The agent input schema excludes these
human fields. Local fill and deck navigation create no shared writes.

The capability owns separate `pane`, `pane-layout`, `pane-annotation`,
`panes-settings` and `panes-history` records. Each annotation slot is its own
record, so the host's automatic stamps preserve independent content, layout,
mark, note and point bylines. Participant ids decide viewer-relative "You";
names are display text. Unknown records and properties survive edits and
reopening.

`panes_import` accepts the JSON contents of a copied legacy `room.json`, not a
filesystem path. The immutable history stores every original JSON field and
log entry alongside the migrated document. Numbers and key order use a
canonical JSON representation, not original file formatting. Historical
bylines remain historical until their specific aspect receives a new write.
The imported author's identity is never used as caller authority. Importing
again changes no document bytes, revision or journal entry. Conflicting pane
ids, existing Panes settings and exceeding the retained 24-pane limit refuse
the first import atomically.

Sources retain the room's root, text-type, private-directory, credential-name,
range and size limits. Deleted sources render and read back a named error.
Runnable catalog entries report exact declared default ports and live HTTP
status; portless commands remain portless. They never execute commands.
External embeds retain the room's explicit human Load gate. Sandbox HTML is
served by this app at `/panes/html`; iframe points retain the transient browser
activation gate. The gate does not prove which frame element was clicked.

Verification:

```sh
cargo test --locked -p samepage-panes -p samepage-app
OUT=/absolute/private/results bash crates/samepage-panes/tests/wasm/run-check.sh
cargo clippy --locked -p samepage-panes -p samepage-app --all-targets -- -D warnings
cargo clippy --locked -p samepage-panes --target wasm32-unknown-unknown --lib -- -D warnings
```

The equivalence harness is explicitly invoked after building real WASM. Its
ordinary ignored status is never counted as a pass.
