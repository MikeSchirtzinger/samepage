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
docs/
  ag-ui-surface-spec.md
  ag-ui-extension-architecture.md
  evaluation-layers.md
  record-shape-v0.1.md
  demo-runbook.md
```

The root `Cargo.toml` is the source of truth for workspace members.

## Run the room

From the repository root:

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Open <http://127.0.0.1:8100>. There is no frontend build step.

The room starts without an in-page model by default. Set `AGUI_MCP_TOKEN`
before launch when an outside MCP client will attach. The value is any string
you choose, not a credential issued by anyone. `local-room-token` above is
only a placeholder. The client sends the same value as
`Authorization: Bearer <your value>` to `POST /mcp`. Without the variable the
room mints a random token per process that only its own subprocess can see.

Initialization returns an `Mcp-Session-Id`. Every later MCP request must send
that header and `MCP-Protocol-Version: 2025-06-18`. Omitting the session header
does not borrow another participant's identity. The host signs resulting
writes with the generic `agent` byline.

Read [AGENTS.md](AGENTS.md) for the attachment contract and repository rules.
The room agent's standing contract is
[`examples/same-page-room/prompt.md`](examples/same-page-room/prompt.md).

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
