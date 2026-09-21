# Same Page Atlas

A shared map of a real project that a human and an agent draw on **at the same
time, in the same document**. One CRDT, two writers, no privileged side.

The other "same page" examples in this repo get the agent to *publish* its
understanding into a host-owned surface the human can annotate. This one asks a
narrower and harder question: what if the human can edit the artifact with the
same authority the agent has, concurrently, and the agent has to notice?

## Turn an image into an object

Choose **Add mask**, then click inside the object you want. Atlas prepares the
pinned MobileSAM model automatically and shows the real mask from that point.
Use **Add to mask** or **Remove from mask** only when an edge needs correction,
name the selection, and choose **Create object**.

Select the object and choose **Add part mask** when it has several related
pieces. The picker stays on that object after each accepted part, keeps earlier
masks highlighted on the source image, and advances numbered names such as
`Claw 1`, `Claw 2`, and `Claw 3`. Choose **Finish parts** when the set is
complete. Select a foot and choose **Add part mask** again to add claws beneath
that foot, or drag an existing claw onto its foot in the object tree. Shift-click
any exact set of parts, then choose **Describe motion...** and say what they
should do. Ancestor motion carries its nested parts, so a foot can lift with its
three claws while each claw adds a staggered tap. Two wings can flap on mirrored
pivots without moving any saved object position.

The result is not a temporary polygon or a browser-only overlay. The browser
calls the shared Rust core through its WASM replica. Atlas mints the stable ID,
validates the Worker mask and receipt, and stores the textured object in the
CRDT. Drag the object directly on the canvas to move it. With it selected, use
**Describe motion...** for translation, rotation, scale, opacity, timing,
stagger, easing, loops, or a sequence in ordinary words. The agent turns that
request into a bounded, validated 2D keyframe program. **Stop motion** clears
the program. Atlas stays out of full animation-software territory: there is no
timeline, rig, or 3D model to manage.

The first selection waits for the model graphs to load and for one image
encoding. Refinement points reuse that encoding and run only the prompt decoder.
No alternate execution provider, mock mask, or renderer fallback is configured.

## What is actually shared

One `ag_ui_canvas::scene::Scene`, a yrs document, holding three object kinds:

| kind | what it is |
|---|---|
| `node` | a claim about a real thing: a label, an optional `path`/`lines` into the project, a note, a tone, a status, a position |
| `edge` | a labelled relationship between two nodes |
| `mark` | the human flagging a node or drawing shape: `?` I don't follow, `!` I think this is wrong, `*` this matters, with the agent's answer attached in place |
| `shape` | something *drawn* rather than stated: a pen stroke, a box, an ellipse, a diamond, a line, an arrow, plain text, or a named frame that holds other drawn shapes |

Object properties are **flat scalar keys**, so a human dragging `x`/`y` and an
agent writing `note` on the same node concurrently both survive the merge.
`core/src/lib.rs`'s `concurrent_human_drag_and_agent_note_both_survive` is that
property as a test: two replicas, edits made blind to each other, both edits
present on both sides afterwards.

## The part that matters

**One mutation vocabulary, compiled twice.** `same-page-atlas-core` defines the
projection and every write. The host links it natively (the agent's actions call
it); `same-page-atlas-web` compiles the *same code* to wasm and the browser links
it (the human's pointer calls it). Neither side can drift into its own private
shape of the artifact, and there is no server-side "apply the model's intent"
translation step where meaning quietly changes.

**The browser is a real peer, not a view.** The page owns its own yrs `Doc` and
speaks y-sync over the runtime's `/ws`. The human's drag is an edit to the
document, not a request that the server edit it. That is why the host refuses to
start when `web/pkg` is missing: a read-only page that looks collaborative would
be a lie.

**Human edits are in the agent's next context.** `atlas_read` reports position,
status, marks, and `LAST-EDITED-BY=human` per target. Moving a node the agent
drew is a message it will read, not an invisible local preference.

**And the question at the top of a turn is not "what is on the page".** It is
"what did they do while I was thinking", which a snapshot answers only by
making the reader diff two long documents from memory. The thing hardest to
miss on screen is the thing easiest to miss in a snapshot: a stroke that used
to be decoration and now groups two cards reads *identically* in both, because
the sentence that changed is buried among forty that did not. So every read
after the first ends with the difference, in the same vocabulary:

```
CHANGED SINCE YOUR LAST READ (2): what the human did while you were thinking
- [00026807...0002] agent's rect now ENCLOSES "the read path", "the write path". It was touching no
  node. Agent is grouping them.
- [00026807...0001] "the write path" is now disputed. It was open.
```

Relations, before and after, with the intent named, not `(93,834)`. A shape
that moved 200px and still groups the same two cards is *not* in this section:
its meaning did not change, and reporting the move would bury the one that did.
Coordinates survive in exactly one line, a shape that relates to no node and
never did, because there they are the only thing there is to say.

This is host session state, not part of the document: it is "what **you** were
last told". `Atlas::describe()` stays identical on both replicas, which is what
keeps **Do we agree?** meaningful, and a debug `GET` of the state read-back
deliberately does not advance the mark, otherwise an HTTP probe would eat the
delta and the agent's next turn would be told nothing happened.

**The agent is told what the page actually looks like.** It picks `x`/`y`; it
does not pick height, because a card is as tall as the text on it. So the
browser measures each rendered card and writes the height back into the shared
document, the one fact about the page only the side doing layout can know,
and `atlas_read` ends with a `LAYOUT` section computed from it: the extent of
the page, and a `PROBLEMS` list naming cards that overlap, links that run under
an unrelated node, nodes stranded far from everything else, and, now, what is
wrong with the *drawing*: a region that half-covers a card, a stroke lying
across one it turns out to say nothing about, two scribbles stacked on the same
spot. Without it the model writes coordinates into the dark and is told only
the coordinates back. A height nothing has rendered yet is marked `est`, so a
guess is never passed off as a measurement.

**A drawing arrives as a sentence, not as coordinates.** Some things are faster
to draw than to type: "these three belong together" is one loop of the pen and
a whole paragraph. The trouble is that a drawing is the *least* legible thing
you can put on a surface whose premise is that both sides read everything, 60
coordinate pairs say nothing to a reader who is not looking at the screen. So
every shape is reported by what it does to the nodes underneath it:

```
DRAWING (marks made on top of the map; each shape is reported by what it does to the nodes under it)
- [00176c...0000] human's ink ENCLOSES "2b. /mcp tools/call passed twice", "2c. openai adapter passed". Human is grouping them.
- [00176c...0001] human's ink CROSSES-OUT "2a. /mcp tools/list passed". Human is striking it through.
- [00176c...0002] human's arrow CONNECTS "3. dispatch_tool, one door" -> "4. The asymmetry"
```

The interpretation is geometry in the core crate, not a guess: enclosure is
point-in-polygon against the actual stroke, and an *unclosed* squiggle is
reported as enclosing nothing, because inferring a grouping the human did not
draw is worse than inferring none. Select any shape and the inspector shows you
the same sentence the agent will read, so you never have to take that on trust.

**And a drawing that is ambiguous is reported as ambiguous.** Enclosure is
measured as *coverage*, sixteen samples across the card's own box, three
quarters inside to count as grouped, rather than tested at the card's centre.
The centre test gave one answer to two very different drawings: a loop that
clipped a card's corner and a loop that caught only its middle both came back
"inside". A card between a sixth and three quarters covered is now its own
sentence, in the read-back and in `PROBLEMS`:

```
- [00176c...0000] human's ink ENCLOSES "2b. /mcp tools/call passed twice", "2c. openai adapter passed". Human is grouping
  them and only partly covers "1. ActionAudience, the intent", which reads as neither in nor out.
```

Which is the honest answer, and a question the human can settle in one drag.

The agent draws with the same vocabulary through `atlas_sketch`, its primary
tool, and is told what its shape turned out to mean rather than that it was
written, a box it expected to group three nodes and which actually groups one
is a mistake it can only fix if the write says so.

**A box's words live inside it.** The label on a rectangle, ellipse, or
diamond wraps to the box's width and sits in its middle, the way Excalidraw
binds text to a container, so a label longer than the box is visible as a
spill rather than a line run into the neighbouring box, and `PROBLEMS` names
the spill with the height the words need. A shape can carry a `ref` for the
rest of its `atlas_sketch` call, so the arrows that join a row of boxes bind
to them in the same write instead of a second pass.

**A stroke can say how sure it is.** Every shape carries Excalidraw's
attributes as well as its form: `stroke_width`, `stroke_style` (solid, dashed,
dotted), `opacity`, `roundness`, `font_size`, and `angle`, plus `groups` and a
`frame`. A `frame` is a form of its own, a named region that holds other drawn
shapes (a before and an after, three stages, a legend), and frames do not nest.
The palette offers width and dash beside the ink colour, and a click on either
restyles the selected shape. The read-back reports only what is not the
default, so a dashed, faded box reads as `rect (dashed, 40% opacity)` and a
plain one reads as `rect`. Thickness, dash, and fade are meaning: the heavy
line is the main path, the dashed one is proposed, the faint one is context.
A drawn frame is distinct from the frame a container card draws around the
cards it owns; that one is derived from its members and is not a shape.

**An arrow stays attached to what it means.** Drag from one card to another and
the arrow binds to both (Excalidraw's `startBinding`/`endBinding`, minus the
editor). Move either card afterwards and the arrow follows, clipped to the card
edges so the head lands *on* the box rather than under it. Links between nodes
now do the same and finally have arrowheads. Drag an arrow's endpoint onto a
different card to re-bind it, or onto empty space to let go, a binding you
cannot undo is a one-way door.

**What you drew, you can change.** Select anything drawn and it takes handles:
corners resize a region, corners *scale* a stroke's points, one edge sets where
a text wraps, and the two ends of a line are its bindings. Drag the body to
move it. Every one of those gestures is an ordinary `place_shape` patch, the
same call the agent makes, and the same one that created the shape. Nothing new
was needed in the shared model to make a drawing editable; the browser simply
was not offering the gesture, so a shape could only be deleted and drawn again.

**Drawing and naming are one gesture.** Drag out a box and the caret is already
in it, type, press Enter, and the box is labelled. Double-click anything drawn
to rename it, or select it and press Enter. `T` places plain text: click where
the words go and write them. A text is nothing but its words, so an empty one
is refused rather than left on the page as an invisible object, and its height
is *measured* by the browser and written back, the same deal a card's height
is on, because in both cases only the side laying it out knows the answer.

**A claim that names a source is verified before it lands.** `atlas_place` with a
`path` is rejected unless the host can actually read that file and range,
`Effect::Reject`, which the runtime turns into a retryable tool error. A stale
line range fails loudly instead of silently rendering an empty excerpt. The
inspector then renders the *host-read* bytes, so what the agent quotes and what
the human sees are the same text.

**Pointing goes both ways.** The human clicks a node and says "this" (the
extension posts `/semantic atlas.focus`, the runtime binds the referent). The
agent calls `semantic_target_point` and a visible node or drawn shape lights up
without moving the camera. During a walkthrough, the agent can deliberately
call `semantic_target_reveal` to pan or zoom an off-screen or too-small target
into a readable view. Atlas saves the prior camera and shows **Return to your
view**. A human pan, zoom, or pointer gesture adopts the current view and ends
the walkthrough. Camera movement is useful when it carries the explanation;
otherwise it is unnecessary disruption.

**"Do we agree?" is a real check, not a claim.** The button compares the read-back
the *browser replica* computes with the read-back the *host* hands the model. If
they are character-for-character identical, the two of you are provably looking
at the same document.

## Typed diagrams

A card is the unit of a claim, and it is the wrong unit for a system. An
architect reviewing a codebase wants two more readings before agreeing to
anything: what contains what, and what the machine does under which event.
`atlas_diagram` takes a `kind` for exactly that.

`hierarchy` is the default, and it is what `group` always did: members land
inside a real container node that owns them. What is new is depth. A
`containers` array declares a tree, a container may own containers, and
`HIERARCHY_MAX_DEPTH` (`6`) bounds it. The read-back nests to match and states
the depth against the limit:

```
NODES (deepest containment: 2 of 6)
```

`state_machine` needs a title, an `event` on every transition, and optionally a
`guard`. States take `initial` and `terminal` roles. Cycles are allowed and
expected: the read-back names a machine `cyclic` and the renderer curves back
edges so a loop is visible rather than drawn over the forward path.
`atlas_read` gains a section per machine, in one sentence per transition:

```
STATE MACHINE "explanation cursor" (4 states, 10 transitions, cyclic)
- state "active" INITIAL
- from "active" on pause when status==active -> "paused"
```

`PROBLEMS` reports an unreachable state, a terminal state with an outgoing
transition, two transitions on one event from one state where neither carries a
guard, no initial state, two initial states, and a transition crossing between
two machines. None of those refuses the write, because a CRDT merge can produce
any of them and freezing the document would hide the machine rather than show
the fault.

What is refused, at the writer, is an edge carrying both `label` and `event`.
For a transition the event is the label, and guessing which of two explicit
claims was meant is the silent normalisation this surface exists to avoid.

`data_flow` and `user_flow` are in the vocabulary and refused at parse. They
name where this is going without pretending it arrived.

A container renders as a nested box with a collapse control that hides its
children and shows a count. Collapsing is browser-local and writes nothing to
the CRDT, so it never appears in anyone else's read-back.

A bound state machine paints the shared core's current-state projection. The
selected state has a `CURRENT` badge and distinct border. When the canonical
read-back names one transition with a `via` clause, that transition is
highlighted. An unresolved binding places an `UNRESOLVED` badge on the machine
frame. The browser does not recalculate state from variable values or node
order, and these visual marks write nothing to the CRDT. A refused fractional
or out-of-range variable write leaves the last valid state painted.

### Limits of typed diagrams

- **Nested states and swimlanes do not exist.** A `state_machine` call that
  declares `containers` is refused. Hierarchy nests; machines do not.
- **A transition is not a claim.** Edges carry no `path` and no `lines`, so
  nothing about a transition can be cemented. Attach a card to the state it
  leaves and say in the card which transition it is about.
- **Layout still does not route around obstacles.** Back edges get a distinct
  route so cycles read, which is not the same as a router that bends a new edge
  around an unrelated box.

## Cementing

Cementing is the Atlas exit. Once every assertion has a fresh human `agree`, the
human names a new output directory in the board and **Cement** writes two files
there as one publish: a draft `obligations-v0.1.json` with one prose-only,
advisory obligation per assertion, and `cement-receipt.json` with the exact
assertions, author bylines, human signoff, UTC timestamps, board revisions, and
the Atlas CRDT document revision. The receipt is SHA-256-pinned from every draft
obligation.

The agent-visible MCP tool `atlas_cement_propose` returns those two JSON objects
but writes nothing. `atlas_cement` is human-only through the same dispatcher
audience gate as `board_mark`; an agent or companion is refused before the
action runs. An open, disputed, unsigned, legacy-without-timestamps, or
rewritten-after-signoff assertion refuses the entire operation with every
unsettled id named, and no output directory is created. The target must be new
so both files can publish by one atomic directory rename.

This is a Govern-shaped review draft, not a gate and not runtime governance.
The Atlas does not call or depend on the Govern CLI, store, or extractor.

## Run it

```bash
./examples/same-page-atlas/tests/semantic-import/prepare-mobilesam.sh
./examples/same-page-atlas/build-web.sh     # once, and after editing core/ or web/
cargo run -p same-page-atlas                # http://127.0.0.1:8098
```

The MobileSAM preparation is required on this bounded learning-test branch.
It verifies pinned graph digests and installs the pinned browser runtime. The
human image picker, oracle control, multimodal-agent substitution, replay
steps, and receipt contracts live in
`tests/semantic-import/README.md`.

To open an existing `.excalidraw` diagram where both of you can touch it, one
command boots the server with the drawing already on the board:

```bash
AGUI_ATLAS_IMPORT=path/to/diagram.excalidraw cargo run -p same-page-atlas
```

The path is relative to the project the atlas is about (`AGUI_PROJECT_ROOT`,
default: this repository). The import lands only into an empty document, so a
restart with the variable still set does not double the diagram. The page fits
the camera to the content on first paint; any pan, zoom, or stroke cancels
that for good. On start the log prints the exact `/mcp` attach recipe, token
included, so a terminal agent that did not launch the process can still join
the same board instead of handing over a file path.

Pick a provider in the header (any managed coding agent, or an OpenAI-compatible
connection, the `repo_list`/`repo_read` actions mean a plain chat model can
still ground its claims). Then:

1. Ask: *"Get us on the same page, map this repository on the atlas with real
   source references, and link them."*
2. While it draws, **drag its nodes around** and put a `?` on one.
3. Ask it to continue. It reads your layout and your mark, and answers the mark
   in place.
4. Hit **Do we agree?** to confirm both replicas read back identically.

| gesture | effect |
|---|---|
| drag a node | moves it in the shared document (writes at ~15/s while dragging) |
| drag the background / wheel | pan and zoom (local view only, not shared) |
| double-click empty space | new node |
| double-click a node | rename |
| `?` `!` `*` on a selected node | leave a mark |
| `⌫` / `Delete` | remove the selected object |
| click a node | select it, load its source, and make "this" mean it |
| click a shape | select it; drag to move it, drag a handle to resize or repoint it |
| double-click a shape, or `Enter` | name it |
| drag an arrow's end onto a card | bind it there; drop it on empty space to detach |
| `V` | select tool (the gestures above) |
| `P` | pen, loop around cards to group them, scribble over one to strike it out |
| `R` `O` `D` | box, ellipse, diamond, drag out a region, then type to name it |
| `A` `L` | arrow, line, drag *between two cards* to bind it to both |
| `T` | text, click where the words go, then type |
| `Esc` | deselect and return to the select tool |

**Export** writes the whole page as a `.excalidraw` document; **Import** brings
one in. The forms, the point conventions, the arrow bindings, the stroke and
text attributes, groups, and frames were kept compatible with Excalidraw's
element schema from the start, so this is a field rename rather than a
translation, see `core/src/excalidraw.rs`, which is short on purpose. Frame
membership is reconnected after landing the same way arrow bindings are; a
member whose frame did not come across lands free and the import says so. An import lands as *your* edits, through your own replica, because
it is something you did; the agent's half of the same door is `atlas_import`,
which takes a repository-relative path to a `.excalidraw` file already in the
project. Imported connector bindings are reconnected to the new local shape ids
when both targets land. Curved connector points remain curved while their ends
follow those shapes. Neither direction is silent about what it could not bring
across.

Drawing is primary. An agent puts an idea on the page with `atlas_sketch`,
the free-form tool with Excalidraw's vocabulary and the agent's own positions,
and its write comes back with the measured PROBLEMS that involve what it just
drew, so a box that landed on a card is heard about before the next read.
Cards are for claims: a statement with a source and a verdict the human can
agree with or dispute. When the picture is a set of such claims, `atlas_diagram`
accepts semantic nodes and edges in one call, validates every endpoint before
writing, uses the shared layout engine, and creates editable
Excalidraw-compatible rectangles, ellipses, diamonds, labelled groups, text, and
bound connectors. Nodes accept `normal`, `primary`, or `hero` size, so the
visual anchor gets real space instead of becoming another equal card.

## The workspace dock moves

The map remains primary while the Board, Inspect, and Chat views share one
supporting workspace dock. Switch views with the dock tabs. Drag its seam to
resize it, and use the chevron on the seam, or `Enter` on a focused seam, to
collapse the dock to a labelled rail you can reopen. Arrow keys on a focused
seam nudge by 16px (`Shift` for 64). `Home` or a double-click restores the
default width.

The layout is stored per browser in `localStorage`, deliberately **not** in the
CRDT. Dock width and the active supporting view are not things the other side
should be able to read back, disagree with, or change. On narrow screens the
dock becomes a bounded bottom sheet so the shared map never disappears. See
`static/panes.js`.

The pen stays armed so you can keep drawing; every other tool is a one-shot and
hands you back to select. A stroke is written to the CRDT once, when the pen
lifts, unlike a node drag, which streams, because a stroke is a single
gesture rather than a position that keeps being true.

## Configuration

| variable | default | meaning |
|---|---|---|
| `AGUI_ADDR` | `127.0.0.1:8098` | listen address |
| `AGUI_PROJECT_ROOT` | this repository | the project the atlas is *about* |
| `AGUI_AGENT_CWD` | the project root | working directory for a managed coding agent |
| `AGUI_PROVIDER` | `claude` | selected managed agent or connection id |
| `AGUI_ATLAS_STATE` | `.local/atlas.json` | persisted document |
| `AGUI_PROMPT` | `prompt.md` | standing instructions, read at startup (edit + restart, no rebuild) |
| `AGUI_PROMPT_APPEND` | unset | optional lesson or evaluation instructions appended to the standing prompt |

The built-in Claude entry means the signed-in Claude subscription used by the
Claude CLI. Atlas removes inherited Anthropic API-key, proxy, and cloud-routing
variables from that subprocess, then checks `claude auth status` before it can
report ready. API billing is a separate explicit connection. Every provider
must finish its first real request before the selector closes; the dialog keeps
the actual failure visible if it cannot.

## Interactive explanation flows

Atlas can store one finite guided explanation over any materialized visual. An
authored beat separates five things that topic-specific prompts used to blur
together: the learner cue, the teaching intent, visible evidence, stable target
actions, and allowed transitions. The cursor and last learner evidence live in
the shared CRDT. Agent actions and human browser controls use the same core and
an exact revision, so a stale transition is refused without changing the flow.

The browser panel projects the current beat and can point, reveal, or replay
stored segment motion. Animation is tied to the information flow, not a generic
pulse. Pause, resume, restart, stop, continue, and choice controls change the
shared cursor. An agent-mode beat sends the learner to chat, where the managed
agent resolves the utterance together with any selected semantic target before
choosing a transition.

Model preparation and lesson playback are deliberately separate. Segmentation
or another specialist model creates stable Atlas objects first. The explanation
then references those ids and does not need the model resident while the learner
moves between beats. The default event clock runs visual actions on beat entry.
A browser speech renderer can instead report a spoken character cursor against
optional cue spans. An audio interruption never advances the lesson.

`dev/atlas-evidence-loop` is the second-topic falsifier. It segments a different
generated infographic with real browser MobileSAM, authors a new flow as data,
and drives the same panel through Observe, Segment, Act, and Verify. It proves
trusted pause, resume, and continue inputs, stale-transition refusal, speech
cursor gating, one-shot visual motion, no specialist model resource load during
playback, host and browser equality, and restart persistence.

## Epsilon hypersphere lesson gate

Run the complete isolated lesson with one command:

```sh
dev/atlas-epsilon-lesson
```

The gate begins with a generated worked-example infographic, then creates six
objects from trusted browser clicks through the real MobileSAM worker. The
original source crop stays sharp beneath the segmented animation layers. Three
panel regions receive a one-shot subtract, measure, compare sequence. Each
region finishes hidden, no track loops, and a human can replay the sequence
from Object control.

Before the conversation begins, the driver stores a finite explanation flow
whose evidence and visual actions reference those six segment ids. The global
agent prompt contains only the topic-independent flow protocol. The signed-in
managed provider teaches the current beat, accepts `yes` when the human is
pointing at `YES -> Inside`, advances to a new transfer problem, contrasts L2
with L-infinity, and finishes with a teach-back. The driver proves every cursor
transition, samples every animation track in real Chrome, reads Atlas through
the backend, and checks replica equality. It then restarts the host and proves
that the six object identities, generated source, motion program, and completed
explanation cursor survive. Conversation history is session-local, so
transcript persistence is explicitly not claimed.

Every run writes its receipt, screenshots, diagnostics, and isolated state
under `.local/atlas-experiment/same-page-atlas-epsilon-lesson-*`. A failed run
stays available as evidence and never becomes a baseline pass.

For a repeatability check, run the command twice without editing between runs.
The second receipt must report `status: pass` and classify every gate and both
browser phases as `unchanged-pass`.

## Known limits

Stated plainly, because a collaborative surface that hides these is worse than
one that doesn't have them:

- **No undo.** `⌫` removes an object from the CRDT for both of you immediately.
- **No presence.** You cannot see the agent's cursor or a live selection; yrs
  awareness is not wired up here (or anywhere in this repo yet).
- **Persistence is a throttled whole-document snapshot** (an estimated 1.5 seconds of edits can be
  lost on a hard kill). An update log is the real answer.
- **Automatic diagram layout does not route around obstacles.**
  `atlas_diagram` computes layered placement and `PROBLEMS` reports overlaps
  and crossing links. Connectors remain straight or preserve their imported
  curve, but no router bends a new edge around an unrelated shape.
- **A stroke can be moved and scaled, but not re-drawn.** Handles transform the
  whole thing; there is no editing an individual point, so fixing one wobble in
  a squiggle still means drawing it again.
- **A very large loop groups everything it spans.** Coverage fixed the
  half-covered card; it does not fix a lasso thrown around the whole map, which
  genuinely does enclose everything and says so.
- **A text wraps at whatever the browser decides.** Its height is measured and
  written back, so two replicas with different font metrics would measure
  different heights and the last one to render wins. Nothing detects that.
- **`.excalidraw` round-trips the drawing, not the claims.** A node is a claim
  about a place in this repository, a path, a line range, a status, an author
 , and an Excalidraw document has nowhere to keep that. Exported, a card is a
  rectangle with words in it; imported, a rectangle is a rectangle, never
  promoted back into a claim nobody made. Colour is lossy inbound (their
  palette is open, ours is six names, so a stroke snaps to the nearest), and
  connector bindings are preserved only when both target elements import as
  bindable shapes. An unresolved endpoint lands loose and is reported. All of
  it is explicit, none of it is silent.
- **Heights are only as fresh as the last render.** A card measured by a browser
  that has since closed keeps its old height in the document; if the agent
  rewrites the note while nothing is watching, the geometry it reasons about is
  stale and does not say so (only never-rendered nodes are marked `est`).
- **The host re-clones the whole document to validate every inbound frame**
  (`handle_payload_validated`), so drag throughput is O(document size) per
  message. Fine at this scale, wrong at scale.
- **Loopback only, single human.** Two browsers on the same host converge
  correctly, but there is no identity, authorization, or invitation model.
- **`window.atlas` is exposed for hacking**, the live replica, deliberately
  reachable from the console. That is a development affordance, not a
  production one.
