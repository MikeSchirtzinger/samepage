# SamePage repository guide

SamePage is one running surface shared by a person and an agent. Both act on
the same state. The host stamps writes with unforgeable bylines, the person can
mark or dispute what the agent placed, and the agent reads layout back as
relations rather than browser coordinates.

A rendered summary, status page, or report is not a same page. It is a one-way
artifact without shared mutation, host-stamped attribution, or in-place
disagreement.

## Start with the real tree

The root `Cargo.toml` is the authority for workspace membership. The important
paths are:

- `crates/samepage`: Placeholder for the public crate API.
- `crates/ag-ui-core`: AG-UI protocol types and fixtures.
- `crates/ag-ui-surface`: Runtime, action dispatch, identity, provider, and
  authenticated MCP behavior.
- `crates/ag-ui-canvas*`: Shared canvas state, renderer, and browser or server
  hosts.
- `crates/ag-ui-component*`: Portable component contract and host.
- `crates/ag-ui-eval`: Evaluation runner and receipt types.
- `examples/same-page-room`: The runnable SamePage application in this
  repository.
- `examples/same-page-atlas`: A second runnable surface: a shared map of a
  project instead of a pane layout, on port 8098.
- `docs/ag-ui-surface-spec.md`: Runtime design and implementation record.
- `docs/ag-ui-extension-architecture.md`: Extension and composition contract.
- `docs/evaluation-layers.md`: Separation between conformance, real-agent
  evaluation, and run scoring.

Do not infer a package from a leftover directory. Check the workspace member
list and that package's `Cargo.toml`.

## Run and validate

Start the room from the repository root:

```bash
AGUI_MCP_TOKEN=local-room-token cargo run -p same-page-room
```

Open <http://127.0.0.1:8100>. The room has no frontend build step. Static
assets are served by the Rust process, and panes are data interpreted by the
already compiled renderer.

The command does not return. If you are an agent, run it in the background
with its output captured, wait for `listening on http://127.0.0.1:8100`, then
open that address for the person or tell them to. To make the room about a
different project, set `AGUI_PROJECT_ROOT` to that project's root before
starting.

Use these repository gates for code changes:

```bash
cargo check --workspace
cargo clippy --workspace --all-targets
cargo test -p same-page-room
cargo test -p same-page-atlas
```

Compilation, tests, browser rendering, a real model action, and a durable
receipt are separate claims. Report only the layers actually proven.

## Run and validate the atlas

`examples/same-page-atlas` is the second surface: agent and person edit one
CRDT map of a project rather than a pane layout.

Prerequisites, once per machine. Check with
`wasm-pack --version && wasm-opt --version` before starting, and install what
is missing:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
cargo install wasm-opt --locked
```

```bash
AGUI_MCP_TOKEN=local-map-token cargo run -p same-page-atlas
```

Open <http://127.0.0.1:8098>. The atlas needs a real browser replica, not an
optional one: it builds `web/pkg` itself with `build-web.sh` the first time
that directory is missing, and refuses to start without `wasm-pack` and
`wasm-opt` rather than serve a collaborative-looking page with no CRDT peer.
Point it at a different project with `AGUI_PROJECT_ROOT`, the same as the room.

**The first-run map is empty, and drawing it is your job.** The host does not
generate a picture of the project. After attaching (see below):

1. Call `atlas_read`.
2. Call `atlas_diagram` with `kind: "hierarchy"`: one container per major part
   of the project, cards bound to real source with `path` and `lines` (the host
   verifies every range before anything lands), and directed links for
   dependencies.
3. Call `atlas_read` again. The browser measures card heights after the first
   layout, so fresh cards can overlap. Fix every entry under `PROBLEMS` with
   `atlas_place` (`x`, `y`, `w`) until `PAGE VALIDATION` reports
   `"status":"passed"`.
4. Tell the person what your colours and emphasis mean. The atlas stores them
   and never assigns meaning.

The header names three modes and a page-check status:

- **Explain**: the agent's sketch of a project, not yet bound to source.
- **Map**: a claimed card is a sketch; a verified card is bound to a real
  file, revision, and line range the host checked against git; an undeclared
  card is a lane the extractor found running (a listener, a spawned process)
  that no card claims and that cannot be dismissed while it stays that way.
- **Cement**: writes [G8](https://github.com/MikeSchirtzinger/g8) obligations
  for an agreed part of the map. Obligations start red and turn green as the
  code that satisfies them gets built.

## Attach an agent through MCP

An outside MCP client needs `AGUI_MCP_TOKEN` set before the room starts. The
runtime still requires a bearer token when it generates one automatically, but
only managed provider adapters receive that generated value. A terminal client
therefore needs a value both it and the room know. Any string works;
`local-room-token` is a placeholder, not an issued credential.

The attachment sequence is:

1. Send `initialize` to `POST /mcp` with `Authorization: Bearer <token>` and a
   `clientInfo.name`.
2. Read the host-issued `Mcp-Session-Id` response header. The display name is a
   proposal. The host mints the participant id and disambiguates duplicate
   names.
3. Send `MCP-Protocol-Version: 2025-06-18` on every request after
   initialization.
4. Echo `Mcp-Session-Id` on every request after initialization to keep the
   attached byline and presence lease.

Dropping `Mcp-Session-Id` does not impersonate or resume the named participant.
The call keeps agent permissions, but any resulting write is signed with the
generic `agent` byline. An unsupported or missing post-initialize protocol
version is refused before dispatch.

The endpoint is loopback-only in normal use. A presented browser `Origin` must
also be loopback, which prevents a remote page from reaching the local action
surface through DNS rebinding.

## Human and agent authority

Every action declares `ActionAudience::Human`, `ActionAudience::Agent`, or
`ActionAudience::Both`. The runtime compiles every input schema at startup and
validates every call at the single dispatcher before the action runs.

The agent-visible room actions are `put_pane`, `remove_pane`, `arrange_room`,
`configure_room`, `await_room`, and `read_room`. Browser actions use the
`room_*` names and are absent from MCP discovery. `room_annotate_pane` has no
agent-visible twin, so an agent cannot create a human mark or note.

Hiding a tool from discovery is not the security boundary. The shared
dispatcher checks the caller against the action audience again. Unknown or
ambiguous caller identities receive no audience.

## The room contract

`examples/same-page-room/prompt.md` is the room agent's standing contract. The
server reads it at startup. `AGUI_PROMPT` may point to another file, but a
checked-in behavior change belongs in the default prompt.

The contract requires the agent to:

- call `read_room` before writing
- act on the change delta instead of guessing from chat
- reuse pane ids when revising work
- preserve the person's marks, notes, and layout
- place panes by relation and named size, never by rectangle
- use `source` nodes for live file evidence
- claim browser proof only after opening the real surface

Read `examples/same-page-room/AGENTS.md` before changing the room itself. Read
`examples/same-page-room/workspace/AGENTS.md` when working as an agent inside a
running room.

## Repository boundaries

- Never commit `.local`, logs, `target`, or workspace scratch.
- Do not add a process-spawning capability to the room as a convenience. Its
  runnable catalog reports commands and live HTTP status but does not execute
  them.
- A runnable package may omit a default HTTP port. Keep a declared port exact,
  and represent a command-line-only package as portless.
- Host-resolved source panes stay inside `AGUI_PROJECT_ROOT` and retain the
  text-type, private-directory, credential-name, and size limits in
  `examples/same-page-room/src/catalog.rs`.
- Keep human-only marks absent from every agent schema and execution path.
- Preserve host-stamped participant ids and bylines. Names are display text,
  never authorization input.
- Never make a fallback, mock, copied receipt, or disabled feature count as
  proof.

The enforcement layer is [G8](https://github.com/MikeSchirtzinger/g8),
pronounced "gate". It turns what people and agents agreed on here into
machine-checked obligations that coding agents build against. Cement the
agreement before the code exists. Every gate starts red. Building the code is
the act of turning the gates green, and a gate that goes red again is drift.
