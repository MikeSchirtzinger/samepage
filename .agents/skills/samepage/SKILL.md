---
name: samepage
description: Orient to the SamePage repository, work on the room, explain the shared-surface contract, or run a real same-page walkthrough. Use when a request mentions SamePage, same page, room, pane, surface, read-back, mark, attribution, MCP attachment, extension, composition, or shared state. In this repository, same page names the running application family and never a written summary, status page, report, or published artifact.
---

# SamePage repository

Start with `AGENTS.md` and the workspace members in `Cargo.toml`. Those files
describe the repository that exists. Do not assume a sibling checkout or an
unlisted package is available.

## Same page means the app, not a summary

In this repository, a same page is a running surface where a person and an
agent act on one shared state. The host stamps every write with attribution.
The person can mark or dispute the agent's work, and the agent reads the
surface before claiming that a write landed.

### The disqualifying test

A rendered summary, status page, slide, report, screenshot, or published
artifact is a one-way projection. It is not shared state, nobody can dispute it
in place, and it carries no host-enforced action boundary. Producing one in
answer to a same-page request fails the request.

When asked to get on the same page, use the running room. If it is not running,
say so and offer to start it. Never silently substitute a document.

## What exists here

Same Page Room is the runnable application at `examples/same-page-room`. It is
a free canvas of panes authored through a typed view vocabulary. The person can
drag, resize, mark, note, and point. The agent receives relations such as
"right of" and "below," never coordinates.

The generic runtime is in `crates/ag-ui-surface`. It owns transport, action
dispatch, identity, providers, and the authenticated MCP endpoint. The room
extension owns room state, pane actions, relational read-back, and the
human-only annotation channel.

## Route by task

- Repository orientation: Read `AGENTS.md`, `README.md`, and `Cargo.toml`.
- Room behavior: Read `examples/same-page-room/AGENTS.md`, then the relevant
  file under `examples/same-page-room/src`.
- Agent behavior inside the room: Read
  `examples/same-page-room/prompt.md` completely. It is the standing contract.
- MCP attachment or bylines: Read `crates/ag-ui-surface/src/mcp.rs`,
  `crates/ag-ui-surface/src/runtime_state.rs`, and the MCP handler in
  `crates/ag-ui-surface/src/lib.rs`.
- Action authority: Inspect `ActionAudience`, the shared dispatcher, and the
  room action definitions. Tool discovery alone is not enforcement.
- Extension design: Read `docs/ag-ui-extension-architecture.md` and one real
  extension implementation.
- Evaluation claims: Read `docs/evaluation-layers.md` and keep conformance,
  real-agent evaluation, and run scoring separate.

Recheck file existence and the current diff before repeating a documentation
claim as live fact.

## Start and attach

From the repository root:

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Open <http://127.0.0.1:8100>. An outside MCP client initializes at `POST /mcp`
with the bearer token. It must echo the host-issued `Mcp-Session-Id` and send
`MCP-Protocol-Version: 2025-06-18` on later requests. Without the session id,
writes receive the generic `agent` byline.

Once attached, call `read_room` first. Work through panes, preserve the
person's marks and layout, and read again before claiming completion.

## Proof vocabulary

Keep these claims separate:

- **implemented:** The code path exists.
- **compiled:** The relevant Cargo command completed.
- **tested:** The relevant test command passed.
- **running:** The process bound its documented address.
- **opened:** The real surface was inspected in Chrome.
- **agent-proven:** A real attached agent completed the named loop.
- **receipt-proven:** A machine-readable receipt records that exact loop.

Mocks, fallbacks, disabled features, copied receipts, and narrated actions do
not prove the path.

Use `cargo test -p same-page-room` for the room's deterministic contract. Use
real Chrome for rendering claims and a real MCP client for attachment claims.
