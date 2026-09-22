# SamePage

SamePage is one running surface where a person and an agent work on shared
state. The agent can author the surface, the person can drag and mark it, and
the host stamps every write with the participant who made it.

The surface describes layout as relations such as "right of" and "top edges
aligned." Coordinates never cross the agent boundary. A rendered report or a
chat preview is not a same page because the person cannot act on the same
artifact or dispute it in place.

Because the host stamps every write, a session is also a record: who put each
thing on the page, who disputed it, and who agreed. Getting on the same page
and being able to prove you were on it are the same mechanism. The stakes rise
in that order wherever agents work beside people: first who contributed what,
then what was decided and by whom, then what was committed and under whose
authority. The record's shape is published early on purpose, as
[`docs/record-shape-v0.1.md`](docs/record-shape-v0.1.md) with a validator
crate, so any surface can adopt it. Trust labels are part of the schema: a
claim the host cannot verify stays labeled as claimed instead of hardening
into fact.

The room example below is one surface built this way. The general case is any
place a person and an agent need to look at the same thing and settle what
they see.

## The surface

- **Agent-authored views:** panes over a typed view vocabulary, no frontend
  build step.
- **Human marks and notes:** a browser-only channel refused to agents by the
  shared action dispatcher.
- **Relational read-back:** the agent reads the surface and its changes
  without receiving browser coordinates.
- **MCP attachment:** an outside terminal agent joins through the runtime's
  authenticated `POST /mcp` endpoint under its own name.

## The record

- **Host-stamped bylines:** human and attached-agent identities are minted or
  admitted by the host, not accepted from action input.
- **Transport-derived authority:** what a request may act as comes from its
  credentials. The request body can narrow that authority, never widen it,
  and ambiguity fails closed.
- **Record shape v0.1:** `crates/ag-ui-record` validates a session record
  against the published shape, including hash-linked settlement receipts.

## Repository layout

```text
crates/
  samepage                 public crate-name placeholder
  ag-ui-core               AG-UI protocol types
  ag-ui-surface            application runtime, actions, identity, and MCP
  ag-ui-record             session-record shape validator
  ag-ui-canvas*            shared canvas state and rendering
  ag-ui-component*         portable component contract and host
  ag-ui-eval               deterministic and real-agent evaluation runner
examples/
  same-page-room           runnable shared room on port 8100
  same-page-atlas          runnable shared map of a project on port 8098
docs/
  ag-ui-surface-spec.md
  ag-ui-extension-architecture.md
  evaluation-layers.md
  record-shape-v0.1.md
  demo-runbook.md
```

The root `Cargo.toml` is the source of truth for workspace members.

## Before you start

Both apps compile part of themselves to WebAssembly on first run. Install the
three pieces once:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
cargo install wasm-opt --locked
```

Check they are on your `PATH` with `wasm-pack --version && wasm-opt --version`.
The room starts without them but reports its protocol client as unavailable.
The map refuses to start without both.

## Run the room

From the repository root:

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Open <http://127.0.0.1:8100>. There is no frontend build step: the typed
protocol client builds itself on first run if `wasm-pack` is on `PATH`
(install it with `cargo install wasm-pack`); without it, the room still
starts and the page reports the protocol as unavailable instead of guessing
at it in JavaScript.

The room starts without an in-page model by default. Your own coding agent
joins from outside over MCP, and it needs the room's token to do that.

**The token is a password you make up for this room.** It is not an API key,
you do not sign up for it, and no service issues it. Pick any string, start the
room with it in `AGUI_MCP_TOKEN`, and give the same string to the agent you
want to let in. `local-room-token` above is only a placeholder; any value
works, as long as the room and the agent use the same one.

The agent sends that value as `Authorization: Bearer <your value>` to
`POST /mcp`. If you start the room without `AGUI_MCP_TOKEN`, it makes up a
random one per process that only its own subprocess can see, so no outside
agent can attach. The atlas uses `AGUI_MCP_TOKEN` the same way and prints the
exact attach command, token included, when it starts.

Initialization returns an `Mcp-Session-Id`. Every later MCP request must send
that header and `MCP-Protocol-Version: 2025-06-18`. Omitting the session header
does not borrow another participant's identity. The host signs resulting
writes with the generic `agent` byline.

Read [AGENTS.md](AGENTS.md) for the attachment contract and repository rules.
The room agent's standing contract is
[`examples/same-page-room/prompt.md`](examples/same-page-room/prompt.md).

## Run the map

The room is one shared surface. The atlas is another: instead of panes it
draws a map of a project, agent and person editing the same CRDT document.
From the repository root:

```bash
AGUI_MCP_TOKEN=local-map-token cargo run -p same-page-atlas
```

Open <http://127.0.0.1:8098>. The first run builds the browser replica with
`build-web.sh`, which needs `wasm-pack` and `wasm-opt` from
[Before you start](#before-you-start). Point it at your own project the same
way as the room:

```bash
AGUI_PROJECT_ROOT=/path/to/your/project \
AGUI_MCP_TOKEN=local-map-token cargo run -p same-page-atlas
```

### The map starts empty

On first run the canvas is blank: "0 components · 0 relationships". The map
does not draw your project for you yet. The picture comes from your own coding
agent, attached over MCP with the token you picked, the same way as the room
([AGENTS.md](AGENTS.md) has the attach steps). Once it is attached, ask it:

> Read the map with `atlas_read`. Then draw this project's architecture with
> `atlas_diagram`: one container per major part, cards bound to real files
> with `path` and `lines`, and links for how the parts depend on each other.
> Read the map again and fix every item under PROBLEMS with `atlas_place`
> until PAGE VALIDATION says passed.

The header reads "Page checks passed" when the layout has no overlaps or
unroutable links.

In **Map** and **Cement** mode a strip along the bottom lists "Found in the
code, not in the agreement": every binary, listener, and spawned process the
extractor found that no card claims yet. Its length depends on the project. It
shrinks as cards claim lanes, and **Explain** mode hides it.

The map has three modes, always visible in the header alongside a page-check
status such as "Page checks passed":

- **Explain** is a sketch. The agent lays out what it believes a project looks
  like, with no source binding yet.
- **Map** is where a card earns proof. A verified card is bound to a real
  file, revision, and line range the host checked; an undeclared card is
  a lane the extractor found in the running project (a listener, a spawned
  process) that no card has claimed yet, and cannot be dismissed as long as it
  is unaccounted for.
- **Cement** turns an agreed part of the map into [G8](https://github.com/MikeSchirtzinger/g8)
  obligations: checks that start red and turn green only as the real code that
  satisfies them gets built.

## Point it at your own project

The room is about one project at a time. By default that is this repository.
To get on the same page about a different one, name its root at launch:

```bash
AGUI_PROJECT_ROOT=/path/to/your/project \
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Every `source` pane then reads files under that root and nowhere else, and the
"what this workspace can run" pane lists that project's runnable packages.
Today that catalog understands Cargo workspaces only. A single-crate or
non-Rust project shows no runnable packages until its files are opened through
`source` panes.

## If a coding agent is starting the room for you

The run command above does not return. An agent running it in the foreground
blocks on it. The recipe for an agent is:

1. Start the room in the background and capture its log.
2. Wait for the line `listening on http://127.0.0.1:8100`.
3. Open that address for the person, or tell them to.
4. Attach over `POST /mcp` with the same token, as described in
   [AGENTS.md](AGENTS.md), and call `read_room` before writing anything.

The person looks at the browser. The agent looks at `read_room`. That is the
whole point: two seats, one artifact.

## Proof commands

```bash
cargo check --workspace
cargo clippy --workspace --all-targets
cargo test -p same-page-room
cargo test -p same-page-atlas
cargo test -p ag-ui-record
```

Compiler and test success do not prove browser rendering or a real agent loop.
Those are separate claims and need separate evidence.

## Naming

The enforcement layer is [G8](https://github.com/MikeSchirtzinger/g8),
pronounced "gate". It turns what people and agents agreed on here into
machine-checked obligations that coding agents build against. Cement the
agreement before the code exists. Every gate starts red. Building the code is
the act of turning the gates green, and a gate that goes red again is drift.

The internal crates still carry their `ag-ui-*` extraction names while the
public API consolidates under SamePage.

## License

MIT
