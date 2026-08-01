# You are in a room with someone

This is the working directory for an agent running *inside* Same Page Room. You
share a page with a person. The page is a grid of panes; you write panes, they
rearrange and mark them, and neither of you owns it.

`prompt.md` in the example root is your standing instruction set and takes
precedence over this file. This file is the reference you come back to.

## The loop

```text
read_room                     ← always first; the delta is the part to act on
  ↓
put_pane / arrange_room / configure_room
  ↓
their marks, notes, drags     ← appear in your next read_room delta
```

## Actions

| action | what it does |
|---|---|
| `read_room` | the whole room, their marks and notes, and what changed since your last read |
| `put_pane` | create a pane, or rewrite one in place by reusing its `id` |
| `remove_pane` | take a pane down; a pinned pane is refused |
| `arrange_room` | move a pane beside another (`place`), resize it (`size`), or `pinned` |
| `configure_room` | `intent` and the appearance `theme` |

Every mutation takes `expected_revision`. A conflict means they changed
something while you were thinking: read again and reconcile.

## Where a pane goes

The room is a free canvas, not a grid. Panes sit wherever they were put, they
can be any size, and the person moves them by dragging — so the arrangement you
find is usually one they made on purpose.

You place a pane by naming a neighbour, never by coordinate:

```json
{ "id": "retries", "title": "Where the retry lives", "view": { ... },
  "place": "right of: turn-loop", "size": "wide" }
```

`place` takes `right of: <id>`, `left of: <id>`, `below: <id>`, `above: <id>`,
`near: <id>`, or `start` / `end` for the top or bottom of everything. `size` is
a shape — `small`, `medium`, `wide`, `tall`, `large`. If the spot you asked for
is occupied, the host slides the pane clear rather than refusing.

Two things follow from this that are easy to get wrong:

- **Omitting `place` on a rewrite is the right default.** Reusing an id leaves
  the pane exactly where the person dragged it. Passing `place` every time
  quietly undoes their arrangement.
- **`read_room` describes position in the same words you write it in** — "sits
  right of X, top edges aligned", "on its own at the bottom left". When they
  move something, the change reads as a new relation, not a new number. You
  never receive coordinates, and you cannot send them; if you catch yourself
  wanting to, the vocabulary above is what to reach for instead.

## Building a pane

A `view` is one node. `GET /room/vocabulary` is the complete reference; this is
the shape of the thing:

```json
{ "kind": "stack", "children": [
  { "kind": "heading", "level": 2, "text": "Where the retry lives" },
  { "kind": "text", "text": "One place, and it is not where you'd guess." },
  { "kind": "source", "path": "crates/ag-ui-surface/src/turn_loop.rs",
    "from": 120, "to": 158 },
  { "kind": "table", "columns": ["case", "retried?"],
    "rows": [["429", "yes"], ["schema error", "no"]] },
  { "kind": "row", "children": [
    { "kind": "button", "label": "Show me the ACP side",
      "ask": "Open the ACP adapter beside this and point at the difference." },
    { "kind": "button", "label": "This is wrong",
      "ask": "I don't think that's where retries happen. Re-check." }
  ]}
]}
```

Choosing well matters more than the syntax:

- a comparison is a `table`, not three paragraphs
- a status readout is `kv`
- a claim about this codebase is a `source` node, never a `code` node you typed
  out — `source` is re-read from disk, so it cannot be stale or invented
- a decision is `button`s; a question you need answered is `field`s plus a
  `button`
- "what can this run" is the `options` node, never a list from memory

## What you cannot do

- send HTML, CSS, JavaScript, or an event handler — anywhere, in any field
- set a mark or a note; those are the person's channel and have no agent action
- remove a pinned pane
- read outside `AGUI_PROJECT_ROOT`, or inside `.git`, `.local`, `target`,
  `node_modules`, `pkg`, or anything credential-shaped
- start a process; ask, and the catalog's dot turns green when it really answers
- claim a pane was verified in a browser you did not drive

When the room cannot do what they asked, say so and name the nearest thing it
can do. This room is early and small on purpose; a missing primitive named out
loud is more useful to them than one faked.
