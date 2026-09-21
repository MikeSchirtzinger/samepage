# Typed diagrams: live proof

Board `o-typed-diagrams-20260902`, branch `spike/visual-instinct-lab`, from
checkpoint `d0717ed` plus the uncommitted work of phases 1 and 2.

**Verdict: the board passes.** Every deliverable is proven live, in the host
read-back and in the browser, by the two runs recorded here.

This file keeps both runs on purpose. Sections `1` to `7` are task A6 on
2026-09-02, which FAILED: the agent-ink state binding was stored but never
projected, so `atlas_read` never reported the current state and a bound
variable could be driven out of range in silence. Section `8` is task A6b, the
rerun after A5b closed exactly those two holes. The failing run stays because a
verdict is worth less when the evidence behind the earlier "no" has been
deleted, and because the shape of that failure is what A5b was built against.

One deliberate limit survives both runs and is not a failure of shared state:
the browser does not DRAW the current state. It computes it, identically to the
host, and section `8` proves that. Nothing paints it on the page yet. That is a
rendering gap, recorded as such in the README and in AGENTS.md.

**No mock, stub, or fallback was encountered or used at any point in either
run.** Every number came from a command whose output is quoted. One
configuration change was needed to start the server, in both runs, and it is
named in full in "How the server was started".

## Run one, task A6, 2026-09-02: FAILED

Kept as history. Its verdict was overturned by run two in section `8`.

## 1. Validate list from AGENTS.md

Run from `examples/same-page-atlas`, in order, before anything else.

| # | command | result |
|---|---|---|
| 1 | `cargo fmt -p same-page-atlas -p same-page-atlas-core -p same-page-atlas-web -- --check` | exit `0`, no output |
| 2 | `cargo test -p same-page-atlas -p same-page-atlas-core` | exit `0`. `154 passed; 0 failed` (host), `244 passed; 0 failed` (core), `398` total |
| 3 | `cargo clippy -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings` | exit `0`, no warnings |
| 4 | `./build-web.sh` | exit `0`. `Your wasm pkg is ready to publish at examples/same-page-atlas/web/pkg` |
| 5 | `cargo run -p same-page-atlas` | started, see below |

Two extra checks the board's earlier tasks named, run here as regression cover:

| check | command | result |
|---|---|---|
| existing browser test | `node tests/semantic-import/test-segment-picker.mjs` | exit `0`, `segment picker contracts: PASS` |
| no new `innerHTML` or rAF poll | `grep -c` on the changed renderer against `git show d0717ed:...` | `innerHTML` `3` before and `3` after, all three inside comments that say never to use it. `requestAnimationFrame` `7` before and `7` after. This board added neither |

Steps 6 to 18 of the AGENTS.md list are the pre-existing browser checks for the
drawing, marks, import, and segment features. This proof exercised the new typed
paths plus checks 7 and 9 of that list, and did not re-run the drawing and
segmentation checks, which no task on this board touched.

## 2. How the server was started

```bash
AGUI_MCP_TOKEN=proof-a6-typed-diagrams \
AGUI_AGENT_INK=1 \
AGUI_ADDR=127.0.0.1:8106 \
AGUI_ATLAS_STATE=<scratchpad>/atlas-state/atlas.json \
AGUI_BOARD_STATE=<scratchpad>/atlas-state/board.json \
AGUI_ATLAS_MODEL_DIR=/Users/mike/dev/ag-ui-rust-wt-atlas-oracle-import/examples/same-page-atlas/.local/models \
cargo run -p same-page-atlas
```

State is isolated in the scratchpad, so this proof started from an empty
document and wrote nothing into the worktree's own `.local`.

`AGUI_ATLAS_MODEL_DIR` is the one configuration change and it is worth stating
plainly rather than burying. This worktree has no `.local/models`, so the first
start refused:

```
Error: "required semantic-import asset .../mobilesam-vit-t-encoder.onnx cannot be read: No such file or directory (os error 2)"
```

That refusal is the fail-closed asset gate working. It was resolved by pointing
the model directory at the real, sha256-pinned encoder and decoder in a sibling
worktree, which `verify_pinned_asset` then checked by size and digest as usual.
Nothing was mocked, stubbed, or disabled to get past it.

## 3. The agent attach

Recipe from the memory note, confirmed against the server's own startup log.

```bash
SID=$(curl -s -D - -o /dev/null -X POST http://127.0.0.1:8106/mcp \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -H "Authorization: Bearer $AGUI_MCP_TOKEN" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"typed-diagrams-proof","version":"1"}}}' \
  | grep -i 'mcp-session-id' | tr -d '\r' | awk '{print $2}')
```

Every subsequent call carried `MCP-Protocol-Version: 2025-06-18` and
`Mcp-Session-Id: $SID`. The byline held: the server logged

```
agent attached over /mcp participant=agent-6fe94233cf6e485e95e4e87767e4e4b2 label=typed-diagrams-proof
```

and every node the agent authored reads back `created_by=typed-diagrams-proof`.

The agent was given `prompt.md` and one request, nothing else:

> show me the explanation cursor as a state machine and the atlas tool catalog
> as a hierarchy

It was told not to read `docs/` or `specs/`, so it never saw the A0 design note.
It discovered the typed vocabulary from the tool schema alone.

## 4. Every tool call the agent made

| # | call | outcome |
|---|---|---|
| 1 | `tools/list` | `51` tools |
| 2 | `tools/list`, `atlas_diagram` schema | read the typed schema |
| 3 | `tools/list`, schemas for `repo_*`, `atlas_read`, `atlas_explanation_*` | read |
| 4 | `atlas_read {}` | baseline, `0 nodes` |
| 5 | `atlas_diagram` state machine, edges carrying both `label` and `event` | **refused** |
| 6 | `atlas_diagram` state machine, `label` dropped | `4 state(s), 10 transition(s), and one container` |
| 7 | `repo_read {"path":"examples/same-page-atlas/src/board.rs","lines":"321"}` | verified a citation |
| 8 | `repo_read {"path":"crates/ag-ui-surface/src/semantic_targets.rs","lines":"890-893"}` | verified a citation |
| 9 | `atlas_diagram` hierarchy, `13` containers, `51` cards | `51 node(s), 0 link(s), and 13 nested container(s)` |
| 10 | `atlas_read {}` | `PROBLEMS (1)`, a stranded card |
| 11 | `tools/list`, `atlas_place` schema | read |
| 12 | `atlas_place {"id":"...2f","x":1958,"y":942}` | landed, problem persisted |
| 13 | `atlas_read {}` | problem still reported |
| 14 | `atlas_place {"id":"...2f","x":1550,"y":1123}` | landed |
| 15 | `atlas_read {}` | `PROBLEMS` absent, layout clean |

Call `5` is the refusal that matters. The agent wrote an explanatory `label`
alongside `event` on every transition and the write was refused whole:

```
diagram edge 1 carries both `label` and `event`; a transition must use `event`
```

An agent that had never read the design note hit the exact rule the design note
specified, got a sentence naming the fix, and applied it by moving the extra
detail into `guard`. Refusal beat a plausible guess, live, on the first contact
with the feature.

Calls `12` to `15` are the second finding. The agent's first fix reduced the
stranded distance from `470px` to `440px` but did not clear the problem, because
the container's centre moves with the card. Only isolating the card onto its own
row cleared it. `PROBLEMS` did not accept a partial repair.

## 5. The read-back

Final `atlas_read` after the agent finished: `69 nodes, 10 links`.

The NODES header now reports containment depth against the limit:

```
NODES (deepest containment: 2 of 6)
```

Containers carry their kind, and only where they have one:

```
- [001c2f628e07dba5-00000000] "Explanation cursor (ExplanationFlowState.status)" tone=concept status=open diagram_kind=state_machine box=(83,120 924x1168) created_by=typed-diagrams-proof CONTAINS 4
- [001c2f628e07dba5-00000012] "Atlas MCP tool catalog (51 tools)" tone=concept status=open diagram_kind=hierarchy box=(1103,120 3405x1523) created_by=typed-diagrams-proof CONTAINS 12
```

The STATE MACHINE section, in the grammar the design note specified:

```
STATE MACHINE "Explanation cursor (ExplanationFlowState.status)" (4 states, 10 transitions, cyclic)
- state "paused"
- state "active" INITIAL
- state "completed"
- state "stopped"
- from "active" on pause when status==active (atlas_explanation_control) -> "paused"
- from "paused" on resume when status==paused (atlas_explanation_control) -> "active"
- from "active" on stop when status active or paused (atlas_explanation_control) -> "stopped"
- from "paused" on stop when status active or paused (atlas_explanation_control) -> "stopped"
- from "active" on advance (terminal beat) when atlas_explanation_advance; landed beat advance.mode==terminal, no next beat -> "completed"
- from "active" on advance (next beat) when atlas_explanation_advance; landed beat non-terminal, current_beat moves forward -> "active"
- from "paused" on restart when atlas_explanation_control; resets current_beat to definition.start -> "active"
- from "completed" on restart when atlas_explanation_control; resets current_beat to definition.start -> "active"
- from "stopped" on restart when atlas_explanation_control; resets current_beat to definition.start -> "active"
- from "active" on restart when atlas_explanation_control; re-enters start beat -> "active"
```

The HIERARCHY section, geometry stripped, first three containers of twelve:

```
HIERARCHY
- Atlas MCP tool catalog (51 tools)
  - Read & wait
    - atlas_read
    - await_atlas
    - board_read
    - atlas_variables_read
    - await_board
    - await_input
    - chat_read
    - semantic_targets_read
  - Draw & place
    - atlas_diagram
    - atlas_draw
    - atlas_place
    - atlas_link
    - atlas_sketch
    - atlas_import
    - atlas_point
    - atlas_remove
  - Explanation flow
    - atlas_explanation_define
    - atlas_explanation_advance
    - atlas_explanation_control
```

Two independent findings about the machine itself are worth keeping, because
both are the surface doing its job:

- The agent marked no state `terminal`, and said why: `restart` has no status
  guard, so it escapes `completed` and `stopped` as readily as `active`. A
  `terminal` state with an outgoing transition would itself be a `PROBLEMS`
  entry. The A0 design note reached the same conclusion independently.
- The agent's citations are correct against the current tree. `paused` cites
  `core/src/lib.rs:8889-8890`, which is the `pause` and `resume` match arms;
  `completed` cites `8664-8670`, which is `explanation_status_for_beat`. It
  verified each one before authoring, and no card was rejected for an
  unresolved path.

## 6. The browser, in real headless Chrome

Chrome was driven through the repository's own `browser-tools` (real Chrome plus
CDP on port `9222`, headless, retina capture). Commands: `browser-start`,
`browser-nav http://127.0.0.1:8106`, `browser-eval`, `browser-screenshot`,
`browser-stop`.

### What renders

```
{"frames":14,"states":69,"transitions":10}
{"states":5,"roles":["","","INITIAL","",""],"backEdges":5,
 "labels":["pause when status==active (atlas_explanation_control)", ...],
 "frameTitle":"Explanation cursor (ExplanationFlowState.status)","frameCount":"4 inside"}
```

Five of the ten transitions are routed as back edges
(`.atlas-transition-back-edge`), so the cycles are visible rather than drawn on
top of the forward path. The initial state carries an `INITIAL` badge. Event and
guard are the edge label. Screenshots:

- `proof-both-diagrams.png`, both typed diagrams fitted on one page: the state
  machine with its curved back edges on the left, the twelve nested containers
  of the tool catalog on the right.
- `proof-state-machine.png`, the machine at reading zoom: the `INITIAL` badge on
  `active`, transition labels carrying event and guard, dashed back edges, and
  the semantic pointer ring with its `Return view` control.
- `proof-compare-replicas.png`, the agree dialog, below.

### Container collapse is browser-local

Clicking the real `.atlas-frame-collapse` button on the `Semantic targeting`
container, with the replica read-back captured either side of the click:

```
{"countBefore":"3 inside","countAfter":"3 hidden","visibleNodesBefore":55,"visibleNodesAfter":52,
 "replicaDigestIdentical":true,"collapsed":["001c2f628e07dba5-0000001e"]}
host_describe_sha_before=9d4d4ee5aa44d257c57bf43efe109fea6e4c8050
host_describe_sha_after=9d4d4ee5aa44d257c57bf43efe109fea6e4c8050
HOST DIGEST UNCHANGED
```

Three children hidden, the count switched from `inside` to `hidden`, and both
the browser replica's `describe()` and the host's `/atlas/describe` came back
byte-identical. Collapse is a view state, not a CRDT write, which is what the
task required and what a second person watching the same document needs.

### A human drag, read back as a relation

The `active` state was dragged with pointer events dispatched at the pane, the
path the real pointer takes. It moved from client `(437,238)` to `(691,498)`.
The next `atlas_read` in the same session reported:

<!-- credo-lint:allow-fenced quoting the tool's real output verbatim, including its existing em dash -->
```
CHANGED SINCE YOUR LAST READ (1) — what the human did while you were thinking
- [001c2f628e07dba5-00000001] "active" is now left of "stopped" — it was left of "Explanation cursor (ExplanationFlowState.status)"
```

A relation with both halves stated, not a coordinate pair, for a state inside a
typed machine.

### Compare replicas

Clicking **Compare replicas** in the page:

```
{"verdict":"Identical. Your replica and the agent's read-back are the same text, character for character.",
 "cls":"verdict-same","identical":true,"mineLen":14966,"theirsLen":14966}
```

`14966` characters each, identical. Both replicas compute
`NODES (deepest containment: 2 of 6)`, `diagram_kind=state_machine`, the STATE
MACHINE grammar, and the HIERARCHY tree from the same core, so the typed
sections converged rather than being computed twice with two opinions. This is
the answer to the "Do we agree?" criterion and it holds for the fixture machine.

## 7. The agent-ink state binding

Commands and their exact replies:

```
atlas_mode_set {"mode":"learning"}
  agent-ink mode is now "learning"

atlas_variable_create {"name":"cursor","value":0,"state":"scrubbing"}
  created variable "cursor"

atlas_state_bind {"machine":"Explanation cursor (...)","variable":"cursor"}
  bound variable "cursor" to state machine "Explanation cursor (...)"
  CURRENT STATE "paused"

atlas_variable_set {"name":"cursor","value":0}
atlas_state_bind {"machine":"...","variable":"cursor","state":"active"}
  bound variable "cursor" to state machine "Explanation cursor (...)"
  CURRENT STATE "active" (was "paused", 2 transitions connect them)
```

The sentence is right, including the ambiguity case: `paused` reaches `active`
on both `resume` and `restart`, so the reply names the count instead of picking
one transition to blame. Two refusals hold:

```
atlas_state_bind {"variable":"other"}
  state machine "..." is already bound to variable "cursor"; a second variable is refused

atlas_state_bind {"state":"Nonexistent"}
  state "Nonexistent" is not in machine "..."; its states are "paused", "active", "completed", "stopped"
```

## What did not hold in run one

Both of these were closed by A5b and are re-tested in section `8`. They are
left here in their original words because run two is only meaningful against
what run one actually said.

Two failing checks, both in the A5 slice. Neither is a mock or a fallback; both
are behaviour that was specified, is partly built, and does not reach the shared
read-back.

**1. `atlas_read` never reports the current state.** A5's acceptance criterion
is that `atlas_read` says `CURRENT STATE Pending (was Idle, via Submit)`. It
does not. The sentence exists only as the return value of
`same_page_atlas_core::living_ink::bind_state`
(`examples/same-page-atlas/core/src/lib.rs:1035-1094`), which the
`atlas_state_bind` tool hands back once. The binding itself is persisted to the
CRDT as the `state_variable` property on the machine node
(`core/src/lib.rs:904` and `:1006`), but that property is never projected onto
`Node` and `StateMachineView::describe` (`core/src/lib.rs:681-715`) never emits
a `CURRENT STATE` line. Verified live: after binding and stepping,

```
$ atlas_read | grep -c "CURRENT STATE"
0
```

The consequence is not cosmetic. The browser replica cannot render the current
state either, since it reads the same projection, so a bound machine looks
identical to an unbound one on the page and in every later read. The one thing
the binding is for does not reach the shared document.

**2. A bound variable can be driven out of range with nothing reported.**
`bind_state` validates the index at bind time
(`bound variable "cursor" must read a non-negative integer from 0 to 3; got 9`),
but `atlas_variable_set` does not consult the binding, so both of these are
accepted silently on a variable bound to a four-state machine:

```
atlas_variable_set {"name":"cursor","value":1.5}   ->  set "cursor" to 1.5
atlas_variable_set {"name":"cursor","value":9}     ->  set "cursor" to 9
```

The core has the sentence for exactly this case,
`CURRENT STATE unresolved: bound variable ... reads N and the machine has M states`
(`core/src/lib.rs:1054`), and it is unreachable from `atlas_read` for the same
reason as the first failure. The document can hold a machine whose bound cursor
points at nothing, and no read-back says so.

## Also worth fixing, outside A5

**The A0 fixture's line citations are stale.** Phase 1 added `1762` lines to
`core/src/lib.rs`. `fixtures/typed-diagrams/explanation-cursor.json` still cites
the pre-phase-1 ranges (`7126-7132` for `explanation_status_for_beat`, now at
`8664-8670`), and so does `docs/design-typed-diagrams-v0.md`. `verify_source`
bounds-checks a range rather than reading it for meaning, so those citations are
accepted and would put four cards on the page pointing at unrelated code. The
blind agent's own citations, written against the current tree, are correct; only
the checked-in fixture is stale. Both files are outside this task's write list,
so they are reported here rather than edited.

**This proof wrote two agent-ink variables into the shared document.** The
authoring agent noticed and flagged them, correctly, as objects it had not
created and could not account for. That is the surface behaving well, and a
reminder that a proof harness sharing a document with an agent under observation
is itself visible in the evidence.

## Run two, task A6b, 2026-09-02: PASSED

The rerun of the slice that failed, on the tree A5b left behind.
`core/src/lib.rs` is `15534` lines at this run. A5b moved `CURRENT STATE` out
of the tool reply and into `StateMachineView::describe`, which is in the shared
core, so both replicas now compute it from one implementation.

### 8.1 Validate list, again

| # | command | result |
|---|---|---|
| 1 | `cargo fmt -p same-page-atlas -p same-page-atlas-core -p same-page-atlas-web -- --check` | exit `0` |
| 2 | `cargo test -p same-page-atlas -p same-page-atlas-core` | exit `0`. `154 passed` host, `250 passed` core, `404` total, up from `398` in run one |
| 3 | `cargo clippy -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings` | exit `0` |
| 4 | `./build-web.sh` | exit `0`, `Your wasm pkg is ready to publish` |
| 5 | `cargo run -p same-page-atlas` | started on `127.0.0.1:8107` |

Server started exactly as in run one, with isolated state in the scratchpad and
the same `AGUI_ATLAS_MODEL_DIR` pointing at the real pinned models, on port
`8107` and token `proof-a6b`. Attached over `/mcp` with the same recipe, as
`a6b-rerun`. The document began empty: `Shared atlas: 0 nodes`.

### 8.2 The fixture loads as checked in

The fixture file was fed to the tool unmodified:

```bash
m.sh atlas_diagram "$(jq -c . fixtures/typed-diagrams/explanation-cursor.json)"
```

```
drew state machine "explanation cursor" with 4 state(s), 10 transition(s), and one container
```

### 8.3 (a) CURRENT STATE with the was and via clause

```
atlas_state_bind {"machine":"explanation cursor","variable":"cursor"}
  bound variable "cursor" to state machine "explanation cursor"
  CURRENT STATE "paused"
```

The line is now in `atlas_read` itself, not only in the tool's reply. After
stepping the bound variable from index `1` to `2`, the host read-back opens its
machine section with:

```
STATE MACHINE "explanation cursor" (4 states, 10 transitions, cyclic)
CURRENT STATE "completed" (was "active", via advance)
- state "paused"
- state "active" INITIAL
```

Both clauses are present: `was "active"` and `via advance`. `advance` is named
because exactly one transition connects `active` to `completed`. The ambiguity
case still behaves as designed: stepping `0` to `1` reads
`CURRENT STATE "active" (was "paused", 2 transitions connect them)`, because
both `resume` and `restart` connect that pair and picking one would invent a
history nobody recorded. A pair with no transition at all drops the clause
entirely, as `CURRENT STATE "stopped" (was "completed")` shows.

### 8.4 (b) A bound variable refuses an invalid write

Both refusals come from the shared setter now, not from the bind call:

```
atlas_variable_set {"name":"cursor","value":1.5}
  could not set variable "cursor": bound variable "cursor" must read a non-negative integer from 0 to 3; got 1.5

atlas_variable_set {"name":"cursor","value":9}
  could not set variable "cursor": bound variable "cursor" must read a non-negative integer from 0 to 3; got 9
```

The next `atlas_read` still names the last valid state, and the variable still
holds the last valid index:

```
CURRENT STATE "stopped" (was "completed")
- cursor is shown at 3, state=scrubbing (gesture-owned), LAST-EDITED-BY=second-writer (changed after a6b-rerun created it)
```

This is the exact pair that run one recorded as accepted in silence.

### 8.5 (c) The state change reaches the delta

One thing had to be got right to test this honestly. `already_seen` in the core
discounts the reader's own writes, so an agent that moves the cursor itself is
correctly told nothing: it does not need reporting to itself. The change has to
come from the other peer, which is also the only case that matters.

A second `/mcp` session attached as `second-writer` and moved the cursor from
`2` to `3`. The first session's next `atlas_read` ended with:

```
CHANGED SINCE YOUR LAST READ (2) - these are writes by second-writer - not the human
- the cursor increased from 2 to 3 (by 1)
- [0004aad95f3d8404-00000000] state machine "explanation cursor" current-state relation changed to CURRENT STATE "stopped" (was "completed"); it was CURRENT STATE "completed" (was "active", via advance)
```

A relation with both halves stated and the author named. The hyphens in the
header line above are this document's; the tool prints an em dash there, which
is pre-existing wording this board did not author.

### 8.6 (d) Compare replicas after all of it

Clicking **Compare replicas** in real headless Chrome, with the binding made,
stepped three times, and twice refused:

```
{"verdict":"Identical. Your replica and the agent's read-back are the same text, character for character.",
 "identical":true,"mineLen":2822,"theirsLen":2822,"bothHaveCurrentState":true}
```

`2822` characters each, and both sides contain the `CURRENT STATE` line. That
is the load-bearing result of this rerun: the current state is not a message
the host sends, it is shared state both replicas derive from one core.

Reading the browser replica directly agrees:

```js
window.atlas.doc.describe().match(/CURRENT STATE[^\n]*/)
  -> "CURRENT STATE \"stopped\" (was \"completed\")"
```

### 8.7 (e) The renderer does not draw it, and that is a rendering gap

Stated plainly because it is the one thing this rerun does not deliver.

```js
{"paneMentionsCurrentState":false,"currentStateBadgeEls":0,
 "stateRoleBadges":["INITIAL"],
 "stoppedNodeClasses":"node | data:{\"id\":\"...00000004\",\"tone\":\"concept\",\"status\":\"open\",\"color\":\"red\"}"}
```

`grep -rn "current_state\|currentState\|CURRENT STATE"` over
`static/extensions/atlas/index.js`, `static/styles.css`, and `web/src/lib.rs`
returns nothing. The `INITIAL` badge renders; there is no badge, class, or data
attribute for the current state, so `stopped` looks like any other state on the
page. `proof-a6b-current-state.png` is that screenshot: the green `INITIAL`
badge on `active`, and `stopped` unmarked.

**This is a rendering gap, not a shared-state gap.** The browser HAS the fact:
its own replica computes the identical `CURRENT STATE` line from the shared
core, proven in `8.6`, and Compare replicas is byte-identical. What is missing
is paint. A person reading the page cannot see which state is current without
opening the read-back, and until a badge exists, do not tell them the page
shows it.

### 8.8 What run two used, in full

```bash
cargo fmt -p same-page-atlas -p same-page-atlas-core -p same-page-atlas-web -- --check
cargo test -p same-page-atlas -p same-page-atlas-core
cargo clippy -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings
./build-web.sh
AGUI_MCP_TOKEN=proof-a6b AGUI_AGENT_INK=1 AGUI_ADDR=127.0.0.1:8107 \
  AGUI_ATLAS_STATE=<scratchpad>/atlas-state2/atlas.json \
  AGUI_BOARD_STATE=<scratchpad>/atlas-state2/board.json \
  AGUI_ATLAS_MODEL_DIR=<sibling worktree>/.local/models \
  cargo run -p same-page-atlas
# two /mcp sessions: a6b-rerun and second-writer
atlas_diagram <the checked-in fixture, verbatim>
atlas_mode_set {"mode":"learning"}
atlas_variable_create {"name":"cursor","value":0,"state":"scrubbing"}
atlas_state_bind {"machine":"explanation cursor","variable":"cursor"}
atlas_read {}                                   # baseline
atlas_variable_set {"name":"cursor","value":1}   # paused -> active
atlas_variable_set {"name":"cursor","value":2}   # active -> completed, via advance
atlas_variable_set {"name":"cursor","value":3}   # as second-writer, for the delta
atlas_variable_set {"name":"cursor","value":1.5} # refused
atlas_variable_set {"name":"cursor","value":9}   # refused
browser-start; browser-nav http://127.0.0.1:8107; browser-eval; browser-screenshot; browser-stop
```

**No mock, stub, or fallback was encountered or used in run two.** The server
was stopped by the PID it was started with, not by name.
