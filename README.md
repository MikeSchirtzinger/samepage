# SamePage

A shared surface where a person and an agent work on the same page.

The agent authors the UI. The person marks it. Every mark carries an
unforgeable byline, and the surface describes itself back in *relations* —
"sits right of", "top edges aligned" — never coordinates.

This is not a chat window with a preview pane, and it is not a rendered report.
It is one running surface with shared state, attribution, marks, and read-back.

## The primitives

**Talk & presence**
- `conversation` — transcript + composer, as a component
- `presence` — who is on the page, by name, live
- `activity feed` — host-owned record with an unforgeable byline

**Shared artifacts**
- `panes` — agent-authored UI over a typed view vocabulary
- `html sandbox` — arbitrary UI, no build step; pointed clicks read back
- `deck` — flip through alternatives one at a time
- `diagram` — structure only; the host does layout
- `source excerpts` — file slices that cannot go stale
- `runnable catalog` — what the workspace can run, with a live port

**Agreement & authority**
- `marks & notes` — the human-only channel, refused to agents at dispatch
- `semantic pointing` — "this one" resolves to meaning, not pixels

**Reach**
- `/mcp door` — any terminal agent attaches under its own name
- `server-rendered routes` — the no-JS, Rust-only lane

## Layout

```
crates/
  samepage              name placeholder; the public API will land here
  ag-ui-core            protocol types
  ag-ui-surface         the app runtime: App + Surface, /mcp, extensions
  ag-ui-canvas*         CRDT canvas, renderer, web + server halves
  ag-ui-component*      WASM component host
  ag-ui-eval            evaluation harness
examples/
  same-page-room        the open tier — a live room you can drag, mark, and
                        attach an agent to
```

## Running the room

```
cargo run -p same-page-room     # http://127.0.0.1:8100
```

Then attach any MCP-capable agent to `/mcp`. See [AGENTS.md](AGENTS.md) for the
rules an agent is expected to follow, and `examples/same-page-room/AGENTS.md`
for the room's own vocabulary.

## Status

Early. The crates still carry their `ag-ui-*` extraction names; the public
surface is being consolidated under SamePage.

## License

MIT
