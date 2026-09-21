# Same Page room

Same Page Room is one running surface shared by a person and an agent. Both
work on the same state. The host stamps every write with its caller, the person
can mark work in place, and the agent reads layout as relations instead of
browser coordinates.

The room does not assume what the shared artifact should become. An agent can
rewrite or add panes while the person drags, resizes, marks, and annotates the
same panes.

## Run

From the repository root, cold start is one command:

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Open <http://127.0.0.1:8100>. There is no frontend install, bundler, or separate
build command. The Rust process serves the browser assets and the pane
renderer. Starting from a fresh clone therefore takes two shell steps: clone
and enter the repository, then run the command above.

The room starts without an in-page model. Attach an agent through `POST /mcp`,
or set `AGUI_ROOM_PROVIDER` to a configured provider id when an in-page agent
is wanted. An outside MCP client must use the same `AGUI_MCP_TOKEN` value as
the server. Pick any string; `local-room-token` is only a placeholder.

State lives in `examples/same-page-room/.local/room.json` by default and
survives restarts. `AGUI_ROOM_STATE` can point a demo or test at isolated
state. `AGUI_PROJECT_ROOT` selects the project shown by `source` panes and the
runnable catalog.

## What a pane can be

A pane contains a validated tree of view nodes. The browser owns every DOM
element. Pane strings reach `textContent` or another property chosen by the
renderer, never `innerHTML`, an event handler string, or `eval`.

The vocabulary is:

| Purpose | Nodes |
|---|---|
| Layout | `stack`, `row`, `deck`, `divider` |
| Text and data | `heading`, `text`, `code`, `list`, `kv`, `table`, `badge` |
| Interaction | `button`, `field`, `link` |
| Host evidence | `source`, `options` |
| Framed content | `image`, `embed`, `html` |
| Diagrams | `diagram` |

`GET /room/vocabulary` returns every field and constraint as JSON. A `source`
node rereads a file inside `AGUI_PROJECT_ROOT` on every render. An `options`
node discovers runnable packages from the workspace manifest and probes every
declared port before showing it as live.

Appearance is also bounded. The room accepts fixed tokens for surface,
density, accent, radius, and text scale. An agent cannot send CSS or restyle
the page around a pane.

## Authority is enforced at dispatch

The agent and browser receive different action catalogs:

| Agent action | Browser action | Result |
|---|---|---|
| `put_pane` | `room_put_pane` | Create or rewrite a pane by id |
| `remove_pane` | `room_remove_pane` | Remove an unpinned pane |
| `arrange_room` | `room_arrange` | Place, resize, or pin a pane |
| `configure_room` | `room_configure` | Set intent and appearance |
| `read_room` | None | Read state plus the change delta |
| `await_room` | None | Wait for another participant to change the room |
| None | `room_annotate_pane` | Set a human mark or note |

The dispatcher checks the caller again when an action runs. Hiding an action
from discovery is not the security boundary. A mark has no agent-visible twin,
so an agent cannot approve its own work. A mark also survives when the agent
rewrites that pane.

Every mutation includes `expected_revision`. A stale write is refused. Pane
placement crosses the agent boundary as a relation such as `right of: summary`
and a named size. Browser rectangles never appear in `read_room`.

## Prove the shared loop

1. Attach an agent through `/mcp` and keep the returned `Mcp-Session-Id` on
   every later request.
2. Ask the agent to call `read_room`, then create a pane with `put_pane`.
3. Mark that pane `?` in the browser and add a note.
4. Ask the agent to call `read_room` again. The mark and note must appear in
   `CHANGED SINCE YOUR LAST READ`.
5. Have the agent rewrite the same pane id. The human mark and note must remain.

An accepted action proves a state change. It does not prove the rendered pane
looks right. Browser proof requires opening the real room in Chrome.

## Boundaries

- The room reports commands but never starts a process.
- A portless command-line package remains runnable without pretending it serves
  HTTP.
- `source` nodes stay inside the selected project root and reject private,
  binary, credential-shaped, or oversized files.
- A mock, fallback response, or copied receipt does not count as proof.

`prompt.md` is the room agent's standing contract. Restart the Rust process
after changing that file. Browser asset edits need only a reload.
