# SamePage

SamePage is one running surface where a person and an agent work on shared
state. The agent can author panes, the person can drag and mark them, and the
host stamps every write with the participant who made it.

The surface describes layout as relations such as "right of" and "top edges
aligned." Coordinates never cross the agent boundary. A rendered report or a
chat preview is not a same page because the person cannot act on the same
artifact or dispute it in place.

## What is here

- **Panes:** Agent-authored interfaces over a typed view vocabulary.
- **Human marks and notes:** A browser-only channel refused to agents by the
  shared action dispatcher.
- **Host-stamped bylines:** Human and attached-agent identities are minted or
  admitted by the host, not accepted from action input.
- **Relational read-back:** The agent reads the surface and its changes without
  receiving browser coordinates.
- **Live workspace catalog:** Runnable packages come from the workspace
  manifest. HTTP ports are probed when declared, while command-line programs
  honestly report that they have no default port.
- **MCP attachment:** An outside terminal agent can join through the runtime's
  authenticated `POST /mcp` endpoint under its own name.

## Repository layout

```text
crates/
  samepage                 public crate-name placeholder
  ag-ui-core               AG-UI protocol types
  ag-ui-surface            application runtime, actions, identity, and MCP
  ag-ui-canvas*            shared canvas state and rendering
  ag-ui-component*         portable component contract and host
  ag-ui-eval               deterministic and real-agent evaluation runner
examples/
  same-page-room           runnable shared room on port 8100
docs/
  ag-ui-surface-spec.md
  ag-ui-extension-architecture.md
  evaluation-layers.md
```

The root `Cargo.toml` is the source of truth for workspace members.

## Run the room

From the repository root:

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Open <http://127.0.0.1:8100>. There is no frontend build step.

The room starts without an in-page model by default. Set `AGUI_MCP_TOKEN`
before launch when an outside MCP client will attach. The client sends that
value as `Authorization: Bearer local-room-token` to `POST /mcp`.

Initialization returns an `Mcp-Session-Id`. Every later MCP request must send
that header and `MCP-Protocol-Version: 2025-06-18`. Omitting the session header
does not borrow another participant's identity. The host signs resulting
writes with the generic `agent` byline.

Read [AGENTS.md](AGENTS.md) for the attachment contract and repository rules.
The room agent's standing contract is
[`examples/same-page-room/prompt.md`](examples/same-page-room/prompt.md).

## Proof commands

```bash
cargo check --workspace
cargo clippy --workspace --all-targets
cargo test -p same-page-room
```

Compiler and test success do not prove browser rendering or a real agent loop.
Those are separate claims and need separate evidence.

## Naming

The enforcement layer currently called Govern will likely ship under a
different name because its crates.io name is taken, but its machine-checked
agreements still keep people and agents aligned while stopping project drift.

The internal crates still carry their `ag-ui-*` extraction names while the
public API consolidates under SamePage.

## License

MIT
