# Same Page Room

An example-local Phase 2 composition, and the least mature app tier in this
repository: an open surface for thinking with a person, not a finished
application. Read `README.md` for what it is and `workspace/AGENTS.md` for how
to behave while you are in a room. Read this file before changing the example
itself.

## Boundaries

- One extension, `room`. Flexibility here comes from what a pane can *be*, not
  from pinning more extensions. Adding a second one is an architectural
  decision that needs a reason and an ADR, not a convenience.
- The view vocabulary is closed on purpose. A new node kind means: a Rust
  variant with validation, a renderer branch, an entry in
  `vocabulary_reference()`, a line in `VOCABULARY`, and a test that the
  renderer's refusal path still holds. A kind that only half exists makes the
  agent's read-back a lie.
- Nothing in a pane may reach the DOM as markup. `textContent` and properties
  the renderer chose, never `innerHTML`, never an event handler string, never
  `eval`. If a feature seems to need markup, it needs a node kind instead.
- Appearance stays a fixed token set. Do not add a free-text style, class, or
  CSS field to any action, however convenient. The claim "no pane can restyle
  the page around it" has to keep being true.
- **Position is free; the description of it is not.** A pane stores a plain
  rectangle and the person drags it anywhere on the canvas, at any size. What
  is forbidden is a coordinate *crossing the agent boundary* in either
  direction: `arrange_room` refuses a `spot` and takes a relation instead
  (`place: "right of: <id>"`) plus a named `size`; `read_room` reports position
  only through `layout::describe`, which derives relations — `right of`,
  `below`, `overlaps`, `aligned` — from the rectangles at read time. The
  guard is `the_read_back_never_contains_a_coordinate`, which asserts on the
  absence of digits, and `the_agent_cannot_send_a_rectangle` on the way in.

  This boundary replaced one that said the opposite — that storing a pixel
  would force coordinate read-backs, so sizes had to be tokens. That inference
  was wrong and expensive: it capped panes at three widths and four heights and
  made "put that over there" inexpressible, which is the wall the canvas exists
  to remove. A semantic sentence is the describer's job, not the model's. If a
  read-back starts leaking numbers, fix `describe`; do not constrain geometry.

  Filling the page stays the separate thing it always was: a viewing choice, so
  it is browser-local, never persisted and never sent, exactly like flipping a
  `deck` — and it must leave the stored rectangle untouched.
- `annotate_pane` must never gain an agent-visible twin, and `read_room` must
  never gain a human one. That asymmetry is the whole trust model.
- The mirrored action pairs must stay mirrored. If you add an agent mutation,
  add its `room_*` counterpart over the same implementation and pass the
  correct `Author`.
- Host-resolved nodes read files inside `AGUI_PROJECT_ROOT` only. Do not widen
  `TEXT_EXTENSIONS`, `DENIED_DIRECTORIES`, or `DENIED_STEMS` to make a
  particular file visible; a refused path is a working refusal.
- The room does not spawn processes. "Start it" asks; it does not exec. If that
  changes it is a new capability with its own grant, not a quiet addition.
- A resolution failure renders as a visible error on the node. Do not make a
  failing `source` node disappear — a pane that silently empties is worse than
  one that says why.

## Proof

- `cargo test -p same-page-room` covers the view vocabulary's refusals, the
  path rules, port discovery for all three spellings this workspace uses, the
  audience split, mark survival across a rewrite, revision conflict, the read
  delta, and persistence of authorship across a reopen.
- `cargo run -p same-page-room`, then open <http://127.0.0.1:8100> in real
  Chrome. Compiled and tested are separate claims from rendered; rendered is a
  separate claim from *a real model drove it*.
- The last full loop proven live: a human `?` plus note → the agent's
  `read_room` delta → the agent rewriting that pane with a host-resolved
  `crates/ag-ui-surface/src/mcp.rs` excerpt → the mark still present. Codex
  provider, real Chrome, 2026-07-27.
- Static assets are served `no-store`, but Chrome will still hand a reused tab
  a cached stylesheet. When a CSS change appears not to apply, restart the
  browser before you go looking for a bug in the rule.
