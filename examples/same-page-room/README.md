# Same Page Room

A room to meet an agent in, when neither of you knows yet what the meeting is
about.

Every other example in this repository is a *finished* app: the atlas is a map,
the studio projects an understanding, the workbench holds an agreement. Each one
decided in advance what the shared artifact would be. This one does not. The
room is a grid of panes, a pane is a tree of view nodes, and the agent writes
those trees at conversation speed. Reviewing a file, comparing options, filling
in a form, browsing a site, restyling the page — those are not features here.
They are things a pane can *be*.

That makes this the least mature tier of app in the repository, deliberately.
It is for thinking with, not for shipping.

## Run

```bash
cargo run -p same-page-room     # http://127.0.0.1:8100
```

No build step. There is no WASM bundle and no bundler: a pane is data and the
renderer is already compiled in, so nothing you and the agent do together
requires a restart. Editing `static/` and reloading is enough; editing
`prompt.md` and restarting is enough to change the agent's standing
instructions.

`AGUI_PROJECT_ROOT` selects the project the room is *about* — it bounds every
`source` pane and seeds the runnable catalog. It defaults to this repository.

The room opens on **Codex**. The bundled `claude` ACP agent authenticates
against API credits rather than a Code subscription, so on a machine without a
credit balance it dies during its setup prime; every provider remains available
in the ⚙ control.

State lives in `.local/room.json` and survives restarts, including who wrote
each pane.

## The view vocabulary

A pane's `view` is one node. Containers nest; leaves render.

| | |
|---|---|
| layout | `stack`, `row`, `divider` |
| prose | `heading`, `text`, `code`, `list`, `kv`, `table`, `badge` |
| interactive | `button` (sends its `ask` to the agent), `field` (its value is appended to any button in the same pane), `link` |
| host-resolved | `source` (a repo path + line range, **re-read from disk on every render**), `options` (the live catalog of runnable packages, with a real port probe) |
| framed | `image` (same-origin or `data:` only), `embed` (a sandboxed site) |
| drawn | `diagram` (boxes and arrows — the agent sends structure, the host lays it out) |

`diagram` is the one node whose *shape* on screen the agent does not choose. It
sends nodes and directed edges; the host runs `ag_ui_surface::diagram` and
resolves finished geometry into the tree, the same way `source` resolves a file
excerpt. That is deliberate — a model asked to place boxes does it badly and
then spends a round trip fixing it, and the layout is one tested Rust module
rather than a second implementation living in the renderer. Clicking a box asks
about that node, so "this" can be bound to one part of the picture.

`GET /room/vocabulary` returns the complete reference as JSON. The agent is
pointed at it from three places — the `put_pane` schema, `read_room`'s footer,
and `prompt.md` — so it never has to read this file to use the room.

The vocabulary exists because "let the model emit HTML" is not a safe way to
generate UI on demand. Nothing in a pane is ever parsed as markup: the browser
module is the only thing that makes a DOM node, and every string it receives
lands in `textContent` or in a property that module chose. So `<script>` in a
pane is text, and a hostile string in a source file is text. The cost is a
closed vocabulary; the benefit is that "build me an interface for this" is no
more dangerous than "write me a paragraph".

Appearance works the same way. The theme is five validated tokens — surface,
density, accent, radius, text scale — applied as CSS custom properties. The
agent can restyle the whole room and still cannot send a single declaration of
CSS.

## Who can do what

Ten actions over six implementations, mirrored by audience:

| the agent sees | the browser owns | |
|---|---|---|
| `put_pane` | `room_put_pane` | create or rewrite a pane by id |
| `remove_pane` | `room_remove_pane` | take one down |
| `arrange_room` | `room_arrange` | order, span, height, pin |
| `configure_room` | `room_configure` | intent, columns, theme |
| `read_room` | — | the whole room plus the delta since the agent last read |
| — | `room_annotate_pane` | mark `? ! ✓ ✗` and leave a note |

The mirroring is what makes authorship real rather than declared. The runtime
refuses `put_pane` from the browser and never lists `room_*` to the model, so a
log line that says `you` was reached through a name the agent was never shown.
`annotate_pane` has no agent twin at all: a `✓` on a pane is always something
the person put there, and the agent cannot mark its own work as agreed.

A mark survives a rewrite. If you mark a pane `?` and the agent replaces its
contents to answer you, the `?` is still there — the question was yours to
close.

## What is honest here, and what is not

- A `source` pane is read from disk on every snapshot, so it cannot go stale
  and cannot be something the model half-remembered. A path that stops
  resolving says so in place instead of vanishing.
- The `options` catalog is discovered from the workspace manifest, and each
  entry's port is parsed out of that package's own `main.rs`. A green dot means
  a TCP connection to that port succeeded within the last three seconds. Two
  packages that want the same port say so.
- The room cannot start a process. "Start it" asks the agent to run the command;
  the dot turns green when something really answers.
- `expected_revision` is checked on every mutation. A stale write is refused,
  not merged.

## Proving the loop

1. Open the room. Two seeded panes; nothing else is decided.
2. Mark a pane `?` and leave a note on it.
3. Ask the agent to read the room and answer what you marked.
4. Confirm it rewrote *that pane* rather than adding a new one, that the excerpt
   it shows has real line numbers from a real file, and that your `?` is still
   on the pane.
5. Change the surface and column count from **Appearance**. Confirm the agent's
   next `read_room` describes the new appearance.
6. Ask for something the vocabulary cannot express, and confirm it says so
   instead of approximating.

A pane the agent narrated but did not put up does not count. Neither does a
source excerpt pasted as `code` instead of resolved through `source`.
