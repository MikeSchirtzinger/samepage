# Same Page Atlas orientation

This example is intentionally example-local. Do not extract the node/edge/mark
vocabulary into the runtime until a second application needs the same shape.

## What this example is for

Proving one property: a human and an agent editing **the same CRDT document
concurrently**, where each side's edits are legible to the other. Everything
here exists to serve that, and anything that quietly weakens it, a
server-authoritative apply path, an agent-only write channel, a projection the
browser computes differently from the host, is a regression even if it
compiles.

## Invariants

- **One vocabulary, compiled twice.** Every mutation goes through
  `same-page-atlas-core`. If the browser needs a new kind of edit, add it to
  the core crate and rebuild `web/pkg`; do not add a browser-only write path.
- **The browser stays a real peer.** It owns a yrs `Doc` and speaks y-sync over
  `/ws`. Do not replace it with "POST an intent, server mutates", that is a
  different product with the same screenshots.
- **The host refuses to start without `web/pkg`.** A read-only page that looks
  collaborative is worse than an error.
- **A source reference is verified, not asserted.** `atlas_place` with a `path`
  reads the file before accepting the write, and a stale line range is an error
  rather than an empty excerpt.
- **The human's edits reach the agent's context.** `atlas_read` must keep
  reporting position, marks, and `LAST-EDITED-BY`. Do not summarize it into
  prose that drops the provenance.
- **Attention is live context, not a trail.** Viewport, selection, and drag
  observations are lease-bound and expire within tens of seconds by design.
  A late-attaching agent may see no **WHERE THE HUMAN IS** section even when
  the channel and shared document are healthy. Read an absent section as
  live-only context that is currently absent, not as breakage. Ask the human
  to select, move, or drag again when their current attention matters.
- **A read-back is a snapshot AND a delta.** `atlas_read` ends with `CHANGED
  SINCE YOUR LAST READ`, phrased in relations with before/after and the intent
  named, never as coordinates, except for a shape that relates to no node and
  never did. The delta lives on the host (`AtlasState::last_read`), NOT in
  `Atlas::describe()` and NOT in the CRDT, for two reasons that both bite:
  `describe()` is served by a generic debug route, and something polling it
  would silently eat the change the agent needed to see; and the browser
  replica computes `describe()` too, which "Do we agree?" compares
  character-for-character. "What you were last told" is session state. Tests:
  `the_second_read_says_what_the_humans_edits_now_mean`,
  `a_plain_describe_does_not_advance_the_read_mark`,
  `a_rebound_arrow_is_reported_as_the_relation_that_changed`.
- **A move that changes no meaning is not a change.** A shape dragged 200px
  that still groups the same cards is not reported; burying the one relation
  that did change under geometry that did not is the failure mode this section
  exists to avoid. `a_move_that_changes_no_meaning_is_not_reported`.
- **A measurement is not an edit.** The renderer writes each card's height back
  through `measure_node`, and each `text` shape's through `measure_shape`,
  which is the same write for the same reason, and neither stamps
  `touched_by`. It is
  a third category of write, not an agent claim, not a human gesture, but a
  derived fact only the side doing layout can know. If a measurement ever sets
  `touched_by`, every node reads `LAST-EDITED-BY=human` and the one signal that
  makes a human edit legible is drowned. `a_measurement_does_not_look_like_a_
  human_edit` is that invariant as a test.
- **Geometry the agent is told is measured or labelled.** `LAYOUT`/`PROBLEMS`
  are computed from real rendered bounds; a height no browser has produced yet
  is marked `est`. Do not silently substitute the estimator for a measurement,
  the point of the section is that the agent can trust it.
- **A drawing is reported as a claim, never as coordinates.** `DRAWING` names
  each shape by what it does to the nodes under it, ENCLOSES, CROSSES-OUT,
  POINTS-AT, STARTS-AT, CONNECTS, SAYS, because a circle a human draws around
  three cards is a sentence, and 60 coordinate pairs are not. If that section
  ever degrades into a point dump, the human's fastest gesture becomes the one
  edit the agent cannot read, and the surface has a private channel on it.
  `a_shape_is_described_as_a_claim_not_as_coordinates` is that invariant as a
  test. Its companion, `an_open_squiggle_encircles_nothing`, is the other half:
  do not infer enclosure from an unclosed stroke, reporting intent the human
  did not express is worse than reporting none.
- **The drawn layer also reads against itself, cards first.** A shape that
  touches no card is read against the other drawn matter: a loop around three
  sketches ENCLOSES them, a caption beside one SAYS its words NEAR it, a
  scribble across one CROSSES it OUT, an arrow tip reaching one POINTS-AT it.
  Cards keep priority: ink is consulted only when no card claims the gesture,
  and connectors are never targets. Do not weaken either half; the priority is
  the same node-over-shape rule the hit test enforces, and without the
  fallback a picture authored as ink reads back as decoration.
  `a_circle_around_sketches_reads_as_enclosing_the_ink` and
  `a_caption_near_a_card_and_a_sketch_reads_the_card_only` are the two halves
  as tests.
- **A drawing can be promoted into a claim, and nothing pointing at it is
  orphaned.** "make this a card" on a selected shape (the human's replica
  write, `lift_shape` in the core crate) moves a labeled shape, or a region
  holding exactly one text, into the node register at the same box. Marks,
  arrow bindings, and constraint memberships are retargeted to the new node;
  the lifted shape and a consumed title text leave the drawn layer. A region
  holding two texts is refused as ambiguous rather than guessed at. The
  promotion is human-authored: the node lands `created_by=human` and the next
  `atlas_read` reports it in the delta. Replay:
  `docs/register-lift/lift.sh`.
- **An ambiguous drawing is reported as ambiguous.** Enclosure is measured as
  *coverage* of a card's box, not tested at its centre: three quarters in
  counts as grouped, and anything between a sixth and three quarters is
  reported separately as covering it only partly, in the sentence and in
  `PROBLEMS`. Resolving that either way for the agent invents an intent nobody
  expressed, which is the same failure as inferring a loop from an open
  squiggle. `a_region_that_half_covers_a_card_says_so_instead_of_guessing`.
- **Structure is declared with five exact operations.** `atlas_constrain` uses
  `group`, `sequence`, `attaches`, `voids`, or `labels`. Members are stable ids
  or unique visible titles. Every relation declares `fore` or `back` salience.
  Refusal beats a plausible guess, so an unknown operation, invalid arity,
  duplicate, missing member, constraint member, or ambiguous title stores
  nothing. `atlas_unconstrain` retracts a constraint even when another agent
  authored it, and names that original author in the confirmation.
- **UNSAT freezes the picture until repair.** A cycle in same-axis `sequence`
  claims moves the whole picture into its diagnostic register. Do not keep
  drawing or silently drop a claim. Read the minimal core in `CONSTRAINTS`,
  repair or retract the conflicting constraints, then continue.
- **The pane's own coordinate system is not optional.** `toWorld` accounts for
  `view.scrollLeft/Top` and a `scroll` listener puts the pane back. The pane is
  `overflow: hidden` and nothing here scrolls it, but focusing an inline input
  near the right edge scrolls an ancestor anyway, and every gesture afterwards
  then lands dozens of pixels from the pointer. It fails silently and looks
  like bad hit-testing.
- **A pointer never highlights something the human is not looking at.** The
  host's attention ring is `position: fixed` at the target's client rect, and
  nothing about that rect knows the atlas pane clipped it, so a card panned
  off the pane used to be ringed wherever those coordinates landed, which was
  on top of the *board column*. A confident ring around the wrong artifact is
  worse than no ring: it is the surface asserting a shared referent that is not
  shared. `clipBoundsFor` in the crate's `semantic-targets.js` intersects every
  clipping ancestor, rings only the visible part, and degrades to an edge label
  with a direction when there is no visible part at all.
- **Pointing preserves the camera; a deliberate reveal may guide it.** Every
  node and drawing shape is a semantic target. Use `semantic_target_point` for
  something already readable. It rings the visible part or leaves a directional
  edge label, without moving the view. Use `semantic_target_reveal` when a
  walkthrough genuinely needs to bring an off-screen or too-small target into
  focus. Atlas saves the person's prior camera, pans and zooms to a readable
  target, and exposes **Return to your view**. Consecutive reveals form one
  walkthrough and keep the original return point. Any human pointer or wheel
  gesture adopts the current view and clears that return point. Avoid reveals
  that add no explanatory value; unnecessary camera motion is disruptive.
- **Whether a reveal moves anything is the person's preference, not a rule.**
  The toolbar carries `Camera: follows / asks / held`, persisted per browser.
  Under `follows` a reveal guides the view, except while a hand is already on
  the camera, where it yields rather than jumping mid-gesture, and except when
  the target is already on screen at a readable size. Under `asks` and `held`
  the reveal is declined and degrades to the clickable edge marker, and the
  human clicking that marker is honoured under every policy: asking for it is
  the permission. `atlas_read` reports the current setting in **WHERE THE
  HUMAN IS**, so say "I moved you there" only when the policy says you did.
  Guiding someone through an explanation is a good reason to move the view;
  moving it while they are reading on their own is not.
- **The startup fit is separate and is not a camera grab.** It frames a
  document the human has not touched, re-fitting while the replica is still
  receiving the scene, because a fit that latches onto the first sync message
  frames a fraction of the board. The first human gesture ends it for good.
- **An import reports "changed" and "not imported" as different sentences.**
  `Import` keeps `notes` for anything that landed differently apart from
  `skipped`, which means it did not come across. Imported connectors are bound
  to the new local ids when both target shapes land; only unresolved endpoints
  arrive loose, and that loss is named. A shape that is
  on the board and listed as "NOT imported" is a report the export route
  contradicts one call later; that wording bug shipped once and is why the
  distinction is structural now. `AGUI_ATLAS_IMPORT` boots the server with a
  repository-relative `.excalidraw` already landed, only into an empty
  document, so a restart cannot land it twice.
- **The renderer computes no drawing geometry.** Outline polygons for ink and
  binding-resolved endpoints for arrows come from `Atlas::painting()` in the
  core crate, including the preview under a live pointer (`ink_outline`). A
  second implementation in JavaScript is a second opinion about where the ink
  is; there is exactly one.
- **Points are one scalar string, and that is deliberate.** Every other
  property is per-key LWW so concurrent edits merge. A stroke is authored once,
  as a unit, by one hand, there is no concurrent edit to the middle of
  somebody's scribble to survive, so per-point CRDT entries would buy merge
  semantics for an event that does not happen and cost 200 entries per squiggle.
- **Form picks the layer; `z` orders within it.** Box forms render behind the
  cards they group, ink and arrows in front of what they annotate. Stacking is
  declared in CSS, not left to DOM order: cards are appended as they first
  render, so anything added at startup ends up under every card created later
  and a strike-through becomes invisible.
- **Actions stay `agent_only`.** The human's vocabulary is the CRDT itself; the
  browser module declares `.no_actions()`. If an action becomes human-visible,
  the manifest must list it, the composition check enforces this, do not
  relax it.
- **Semantic import splits authority across the real peers.**
  `atlas_segment_oracle_import` mints the stable object ID and semantic fields
  on the host as the positive control. `atlas_segment_agent_import` accepts
  the same fields only with participant-claimed multimodal provenance bound to
  the pinned source digest and exact semantic output. Atlas validates that
  binding and rejects mock or fallback claims, but cannot independently attest
  to an external model invocation. Only the pinned browser MobileSAM WebGPU
  worker may attach mask pixels. Human movement uses the browser replica.
  Agent movement and regeneration address the Atlas ID, never a mask index.
  Generation, graph digests, source digest, RLE extent, execution location,
  and no-fallback fields fail closed in the shared core. This remains a
  bounded Phase 2 learning test and is not runtime governance.
- **Human image selection is a CRDT write, not a calibration surface.** The
  first trusted positive point produces a real Worker mask. Positive and
  negative refinement points are optional. **Create object** must call the
  shared core through the browser WASM peer, and a rejected materialization
  must leave no partial object. Direct drag and Object control address the
  Atlas-minted ID. A part series stays pinned to one root, keeps accepted masks
  visible, and returns to its selected parent when finished. Parts may contain
  nested parts within the shared depth limit, and `atlas_segment_reparent`
  changes that anatomy without changing mask identity or world position.
  Motion is a separate, validated keyframe program owned by one segment. An
  owner may target itself and any exact subset of its materialized descendants.
  Ancestor motion carries the subtree, while a descendant may add its own exact
  track. One target appears in one track. Motion changes never rewrite saved
  x/y positions. The tree supports Shift-click selection and drag-to-nest, and
  **Describe motion...** carries that exact set into chat instead of exposing a
  timeline editor. Tool success proves persisted state only; visible-motion
  claims still require inspection in a real browser. The Worker receipt belongs
  in diagnostics, never in the human's required workflow.
- **An explanation is shared state, not a lesson-specific prompt.** One finite
  `atlas-explanation-flow-v1` graph binds each learner cue to visible evidence,
  stable target actions, and explicit transitions. The agent advances with an
  exact revision after combining the learner utterance with live selection.
  Human continue, choice, and cursor controls write through the browser WASM
  peer to the same CRDT. Stale transitions and invalid targets fail atomically.
  Prepare specialist-model outputs before defining the flow. Playback may use
  only stable ids and stored motion, never a fresh hidden inference. The event
  clock is current. A future speech renderer may release cue-span actions with
  a spoken-character cursor, but interruption must never advance the flow.
- **Cementing preserves the asymmetry.** `atlas_cement_propose` is an
  agent-only, read-only preview. `atlas_cement` is human-only through the same
  dispatcher gate as `board_mark`; a POST route or payload field is never
  accepted as proof of a human. It writes only when every current assertion
  carries a fresh host-stamped `agree` signoff, and it stays example-local,
  no Govern CLI, store, or extractor dependency belongs in the runtime.
- **A diagram's kind is stored, not inferred.** `atlas_diagram` takes `kind`,
  and the container node carries it as `diagram_kind`. The word is separate
  from `Node::kind`, which means rect, ellipse, or diamond: shape is what a
  card is drawn as, `diagram_kind` is what a container is a diagram of.
  Overloading one word would make the read-back ambiguous at exactly the point
  the agent reads it. `data_flow` and `user_flow` are in the vocabulary and
  refused at parse, so a caller who writes one gets a sentence saying when it
  will work rather than an unknown-value error that reads like a typo.
- **Containment nests, and the limit is a refusal rather than a slow picture.**
  `HIERARCHY_MAX_DEPTH` is `6`. A write past it is refused with a message
  naming the constant, the attempted depth, and the chain. Projection,
  `describe_node`, the delta, and the renderer all recurse over containment,
  and a bounded depth makes those walks terminate on a stated bound rather than
  on the assumption that nobody drags a card into a deep chain. `NODES` reports
  the deepest chain against the limit so an agent about to nest one more level
  knows whether it can.
- **`initial` and `terminal` are two properties, not one packed field.**
  Properties are per-key LWW. Two agents concurrently marking one state initial
  and another terminal both keep their edit; one packed `state_role` string
  would let the later write erase the earlier one with neither author told.
- **A transition is named by its event, and two explicit claims that disagree
  are refused.** Inside a `state_machine` an edge with no `event` is refused,
  and an edge carrying both `label` and `event` is refused rather than merged.
  For a transition the event IS the label. This is the same rule `filled`
  against `emphasis` already follows. It was proven on 2026-09-02 by an agent
  that had never read the design note: it authored ten transitions carrying
  both, was refused with the sentence naming the fix, and moved its detail into
  `guard`.
- **A malformed machine is a PROBLEM, never a refused write.** Unreachable
  state, terminal state with an outgoing transition, two unguarded transitions
  on one event from one state, no initial state, two initial states, and a
  transition crossing two machines are all `PROBLEMS` entries. A CRDT merge can
  produce any of them, and freezing the document over one would hide the
  machine rather than show the fault. The duplicate-event rule is about guards,
  not counts: two transitions on one event are how a branch is written, and
  only the unguarded pair is ambiguous.
- **Collapsing a container is a view state, not a write.** The per-container
  collapse control hides children and shows a count, and it lives in browser
  storage. Verified 2026-09-02: clicking it hid three children while the
  replica's `describe()` and the host's `/atlas/describe` both stayed
  byte-identical. If collapse ever becomes a CRDT write, one person tidying
  their own view edits what everyone else reads.
- **A transition carries no evidence, so cementing stays one claim at a time.**
  Edges have no `path` and no `lines`. To cement something about a transition,
  attach a card to the state it leaves. A transition that could carry evidence
  would be a second kind of assertion the board has no entry shape for.
- **The current state is shared state, computed once.** `atlas_state_bind`
  writes `state_variable` onto the machine node, and `StateMachineView::describe`
  in the shared core emits the `CURRENT STATE` line, so BOTH replicas derive it
  from one implementation and "Do we agree?" compares it like everything else.
  It is not a message the host sends. The sentence names the transition only
  when exactly one connects the two states, says how many when several do, and
  drops the clause when none does, because picking one would invent a history
  nobody recorded. A bound variable's value is enforced in the shared setter,
  not only at bind time: a fractional or out-of-range write is refused there,
  so the document cannot hold a machine whose cursor points at nothing. Proven
  live in `fixtures/typed-diagrams/PROOF.md` section `8`.
- **The renderer paints only the shared current-state projection.** The browser
  WASM adapter resolves the shared core's canonical `CURRENT STATE` sentence to
  paintable ids. JavaScript never derives a state from variable values or node
  order. The selected state carries a `CURRENT` badge and distinct border. A
  transition is highlighted only when the sentence has a unique `via` clause,
  and an unresolved binding places an `UNRESOLVED` badge on the machine frame.
  These marks are derived view state and write nothing to the CRDT. A refused
  invalid variable write leaves the last valid current state painted.
- **Never render model text as markup.** Every DOM node in
  `static/extensions/atlas/index.js` is built with `createElement` +
  `textContent`. No `innerHTML`, no model-authored URLs, CSS, or handlers.
- **No `requestAnimationFrame` polling.** Rendering is event-driven with a
  visibility-aware fallback; a rAF poll paints nothing in a background or
  headless tab, which is exactly where an agent verifies its own work.

## Known gaps (do not paper over)

No undo, no persisted attention trail, no multi-user awareness, throttled
whole-document persistence, O(document) validation per inbound frame,
loopback-only with no identity model.
Browser TTS is not connected to explanation playback yet, its VAD is unfinished,
and the multimodel browser scheduler is not implemented here. The speech cursor
and stable-id preparation boundary are integration seams, not proof of those
systems.
Nested states and swimlanes do not exist; a `state_machine` call declaring
`containers` is refused. A transition carries no `path` or `lines`, so nothing
about one can be cemented.
`atlas_diagram` uses the shared structure-driven layout engine, and a state
machine's back edges get a distinct route so cycles read, but edges are
otherwise straight segments rather than obstacle-routed paths. A height measured by a
browser that has since closed goes stale silently.

Drawing is primary: `atlas_sketch` is where an agent composes the picture, and
cards are for claims with a source and a verdict. `atlas_diagram` supports
ellipse, diamond, and rectangle roles, labelled groups, semantic colour, and
`normal`, `primary`, or `hero` visual size for the case where the picture is a
set of such claims. Fan-out, convergence, hierarchy, and sequence come from the
graph there; everything else is drawn.

On the drawn layer specifically: a stroke's *shape* can be moved and scaled but
not re-drawn point by point, so tweaking one wobble in a squiggle still means
drawing it again. A `text` wraps at whatever the browser decides, so a replica
with a different font metric would measure a different height and the last one
to render wins. `.excalidraw` import brings the drawn layer only; an imported
rectangle lands as a rectangle, and promoting it into a claim is a separate,
deliberate human gesture ("make this a card") rather than something import
does on its own. A connector binding is preserved only when its target also imports
as a bindable shape. Colour is lossy inbound: their palette is open and ours is
six names, so a stroke snaps to the nearest one. These are listed in the README.
Fix them or leave them; do not describe them as solved.

## Attach from Claude Code

Set the tool timeout before starting a client that will make long
`await_input`, `await_atlas`, or `await_board` calls:

```bash
export MCP_TOOL_TIMEOUT=240000
```

The value is milliseconds and lets a wait stay open for up to four minutes
instead of the client cancelling at its shorter default. A serving agent
should read both surfaces, then park in `await_input`; the lane-specific waits
remain available when only one surface matters.

## Serving protocol

Read this before serving a human here. Every rule cost a measured delay on
2026-08-19; each carries the incident so you can judge if your case differs.

1. **Open with a real read of every inbound surface, then baseline.** Call
   `atlas_read`, `board_read` and `chat_read` before waiting on anything. A
   baseline answers "did anything change since I started looking", which is the
   wrong question when something is already waiting.
   Incident: a board question sat 7m10s behind a baseline that swallowed it, and
   the same mistake cost 5m34s an hour later.

2. **Park only in `await_input`.** Use the lane-specific waits only when one
   surface is genuinely all that matters. `await_board` does not wake for canvas
   marks, `await_atlas` does not wake for chat, and a human does not know or care
   which lane they just used.
   Incident: a `?` on a card sat through five consecutive `await_board` polls
   until the human typed to ask whether anyone had seen it.

3. **Act on the wake payload, then verify. Never re-read before acting.** The
   wake already carries the message and its semantic target. A second read of the
   same surface is not a cache lookup, it is a fresh wait.
   Incident: a redundant `chat_read` blocked 60s on a message already in hand and
   turned a 35s answer into 102s.

4. **Ledger a perceived act before responding to it.** Record what arrived,
   through which tool, and the stamp, then answer. Recording costs seconds and
   reconstructing costs a round. Keep the record because it is also your evidence:
   check it before conceding a challenge, since agreeing to something you did not
   do puts a false claim on a shared page.
   Incident: a host was stopped without recording the 22 node board it was
   serving, and settling what the human had actually seen took a full adjudication.

5. **The human clock is the product clock.** Measure from their act to the moment
   they can see the answer, never to your tool returning success. Read `LAYOUT`
   and `PROBLEMS` after every authoring call and fix the picture before announcing
   it.
   Incident: `atlas_diagram` returned success in 48s and the human saw nothing for
   9 minutes, because the result was 5607px wide and off their screen.

6. **Form carries meaning, and the meaning is yours to state.** The node
   register has four style words and each answers a different question:

   - **weight is importance.** `emphasis` strong for the few things the
     argument turns on, muted for context the reader may skip. This is the
     answer to "make the riskiest part stand out". Do not answer it by making
     a card physically bigger; that is `size`, which is footprint.
   - **enclosure is ownership.** A `parent`, or a `group` in `atlas_diagram`,
     puts a card INSIDE a container that owns it. Do not draw a box around
     cards to mean the same thing: a drawn box is geometry a human undoes by
     dragging one card, and containment survives the drag.
   - **colour is category.** `color` groups cards that are the same KIND of
     thing. It is not severity, not status, and not importance unless you say
     so, because `status` and `emphasis` already answer those.
   - **shape is what a thing is.** `kind` rect for a step, diamond for a
     decision, ellipse for a boundary, by convention rather than by rule.

   The atlas enforces the vocabulary and never the interpretation. Nothing in
   it decides that amber means risk, so a picture whose scheme you never said
   out loud is a picture only you can read. **Say what your colours and your
   emphasis mean in the same turn you author them**, and expect the human to
   re-use your scheme rather than guess it.

   Every style word comes back in `atlas_read`, and only where it differs from
   the default, so a page with one emphasised card reads as one emphasised
   card rather than as forty restatements of `normal`.

   This payload is the shape of a served answer. It is executed by
   `the_serving_protocol_example_payload_lands_the_meaning_it_claims`, so it
   cannot drift away from what the tools actually do:

   ```json
   {
     "nodes": [
       { "id": "ask", "label": "Human ask", "shape": "ellipse", "group": "Seat" },
       { "id": "route", "label": "Pick a lane", "shape": "diamond", "group": "Seat" },
       { "id": "author", "label": "Author the picture", "color": "blue", "group": "Engine" },
       { "id": "verify", "label": "Verify against LAYOUT", "color": "blue", "group": "Engine" },
       { "id": "drift", "label": "Silent drift", "color": "red", "emphasis": "strong", "group": "Engine" },
       { "id": "seen", "label": "Human sees it", "shape": "ellipse", "size": "hero", "group": "Seat" }
     ],
     "edges": [
       { "from": "ask", "to": "route" },
       { "from": "route", "to": "author" },
       { "from": "author", "to": "verify" },
       { "from": "verify", "to": "drift" },
       { "from": "verify", "to": "seen" }
     ]
   }
   ```

   Read it as the rules above: two containers own the cards (enclosure),
   blue is one category and red another (colour), `drift` is the one thing
   emphasised (weight), and the ellipses are the boundaries of the loop
   (shape). Then say that out loud to the human, because none of it is
   self-evident from the picture alone.

## Validate

```bash
cargo fmt -p same-page-atlas -p same-page-atlas-core -p same-page-atlas-web -- --check
cargo test -p same-page-atlas -p same-page-atlas-core
cargo clippy -p same-page-atlas -p same-page-atlas-core --all-targets --no-deps -- -D warnings
./build-web.sh
cargo run -p same-page-atlas
```

Then open <http://127.0.0.1:8098> in real Chrome and prove, separately:

1. an agent `atlas_place` appears in the page without a reload;
2. a human drag appears in the next `atlas_read` with `LAST-EDITED-BY=human`;
3. a `?` mark is answered in place by `atlas_answer`;
4. `atlas_place` with a nonexistent path or stale range is rejected;
5. the inspector's source excerpt is the host's bytes, not model prose;
6. clicking a node makes "this" resolve to it in the next question;
7. **Do we agree?** reports identical read-backs;
8. a node created by double-clicking empty space actually lands, the whole
   `askFor` vocabulary (new node, rename, mark) commits through one keydown
   handler, and an exception anywhere in it looks exactly like success because
   the input closes either way;
9. `atlas_read` ends with a `LAYOUT` section whose sizes are `measured`, and
   drag a node onto another to see `PROBLEMS` report the overlap;
10. press `P` and draw a loop around two cards, `atlas_read` reports
    `ENCLOSES` those two by name, not a list of points, and scribbling over one
    card reports `CROSSES-OUT` it;
11. press `A` and drag from one card to another, the arrow binds, and still
    touches both after you drag either card somewhere else;
12. **click** the tool buttons rather than using the shortcuts. The palette
    lives inside the pane, so its buttons are on the canvas pointer handler's
    path; capturing the pointer for a pan retargets the following `click` to
    the pane and the button never sees it. It fails with the keyboard still
    working, which reads as "the palette is decorative" rather than as a bug;
13. draw a box and type immediately, the caret is already in it. Then
    double-click any shape to rename it, drag it to move it, and drag a corner
    to resize it. Drag an arrow's endpoint onto a different card to re-bind it
    and onto empty space to detach it;
14. press `T`, click, and type, the text appears, `atlas_read` reports it as
    `SAYS "..."`, and its height comes back `measured` rather than estimated;
15. **Export**, then **Import** the file you just got back, the shapes land
    again, and anything that could not come across is named in the hint rather
    than dropped quietly. A connector between two imported shapes remains bound;
16. create a mixed-shape graph with `atlas_diagram`, move either endpoint, and
    verify the connector follows it and the export contains both bindings;
17. point at a visible imported shape with `semantic_target_point` and verify
    the camera does not move. Reveal a distant shape with
    `semantic_target_reveal`, verify it becomes readable, then click **Return to
    your view** and verify the previous camera is restored;
18. select a labeled drawn shape and click **make this a card**. The shape
    leaves the drawn layer, a card with the same words appears at the same
    place and stays selected, and the next `atlas_read` reports it as a NEW
    node with `created_by=human`. A `?` mark or bound arrow added to the shape
    beforehand must still point at the card afterwards.
19. author a `state_machine` with `atlas_diagram`, giving one transition both
    a `label` and an `event`, and verify the whole write is refused with a
    sentence naming the fix. Re-send it with `event` alone, then check
    `atlas_read`: a `STATE MACHINE` section reads each transition as
    `from X on EVENT when GUARD -> Y`, the initial state is marked `INITIAL`,
    a machine that loops is called `cyclic`, and `PROBLEMS` is empty. In the
    page, the initial state carries an `INITIAL` badge, every edge label is the
    event with its guard, and a back edge is drawn as a curve so the cycle is
    visible;
20. author a `hierarchy` with nested `containers` at least three levels deep.
    `atlas_read` opens `NODES (deepest containment: N of 6)`, nests the tree to
    match, and prints a geometry-free `HIERARCHY` section. Try one more level
    past the limit and verify the refusal names `HIERARCHY_MAX_DEPTH`. Then
    **click** a container's collapse control in the page: its children hide,
    the count switches from `inside` to `hidden`, and both the replica's
    read-back and `/atlas/describe` stay byte-identical, because collapse is a
    view state and not a write. Finish with **Compare replicas** and verify the
    two read-backs are identical character for character.

Mocks, fallback text, and DOM-only claims do not satisfy these checks.

## Page validation feedback

`atlas_validate` runs the shared deterministic checks. It returns a tool error
for overlaps, routing errors, invalid relationships, browser errors, or pending
browser verification. A pass requires measured geometry and a fresh browser
render matching the current document. Read the affected ids in the result.

Atlas writes that fail validation after applying return `operation_applied:
true` and the original operation result. Repair those existing objects instead
of repeating creation. `await_input` and `await_atlas` also return validation
failures, including late measurements and exceptions from the browser. The
header shows the same status. Screenshot review remains necessary for visual
quality, but it is not the overlap checker.
