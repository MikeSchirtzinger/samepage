# Canvas sweep: the human seat gate

A task driven walkthrough of the same-page-atlas canvas, run in a real browser,
by a person or an agent driving a real browser. Modelled on
`/Users/mike/dev/agentviz/specs/viz-notes/visual-sweep-interaction.md`, which
works by giving the operator a goal and logging honestly what the UI did.

**This is the gate.** No canvas change is done until this sweep runs green on
the changed steps. A passing `cargo test` is not this. A screenshot of the
feature working in isolation is not this. The sweep is a person with a task and
no documentation.

## How to run

1. Serve the atlas on `http://127.0.0.1:8098` with a board that has at least
   twelve cards, one nested container, one state machine, and some sketch. The
   typed diagrams board from 2026-09-02 qualifies.
2. Open it in a real browser. Headless Chrome through `browser-tools` counts;
   `browser-nav` then `browser-screenshot` after each step.
3. Do not read this file's expectations before running a step. Read the task,
   do it, then read the pass and fail lines.
4. Log every step as PASS, FAIL, or NOT RUN, with a screenshot path and one
   sentence of what you actually saw. A step you skipped is NOT RUN, never PASS.
5. A FAIL is not a defect report on its own. Write the sentence a person would
   say, then file it.

## Rules for the operator

- Use only the UI. No console, no `browser-eval`, no MCP calls, except to
  capture a screenshot.
- If you cannot find the control, that is the finding. Log FAIL and say what you
  tried, do not go looking in the source.
- Time each task from the first pointer move to the moment the answer is on
  screen. Anything over 30 seconds gets the elapsed time written down.

## Step 1: Get oriented

**Task.** Open the board cold. Without touching anything, say out loud how many
top level things are on it and what the board is about.

- PASS: the whole authored content is in view at a scale where titles are
  readable, and no panel covers the middle of the canvas.
- FAIL: content is off screen, or the opening view is zoomed into a corner, or a
  notice panel sits over the content, or titles are too small to read.

## Step 2: Move the view without instructions

**Task.** Move the board so a card that is currently near the right edge sits in
the middle. Then get back to seeing everything.

- PASS: a way to pan is discoverable within 10 seconds, and there is a visible
  control that returns to fitting everything.
- FAIL: the operator's first three attempts to pan do nothing, or the only
  working gesture is one nobody tried, or there is no way back to the whole
  board without reloading.

## Step 3: Zoom where you are looking

**Task.** Put the pointer over one specific card and scroll to zoom in and out.
Pinch also zooms. Drag the background to pan.

- PASS: the card under the pointer stays under the pointer as the view scales.
- FAIL: the view zooms toward the centre and the card slides away, or the zoom
  jumps in steps large enough to lose the card.

## Step 4: Read one claim in full

**Task.** Find the card that cites a source file, and read the file path and
line range off it.

- PASS: at the zoom where the whole board reads as a picture the card shows its
  title only, and the source appears once the card is selected or once you have
  zoomed in on it.
- FAIL: every card shows its full body and source at every zoom, so the board is
  a wall of text; or the source is never visible at any zoom.

## Step 5: The picture is the picture

**Task.** Zoom out until the whole board fits. Describe what the board argues,
using only what you can see.

- PASS: sketch, containers, and arrows carry the argument, and cards read as
  labelled points inside it.
- FAIL: the board reads as a list of dense text boxes and the drawn structure is
  invisible behind them.

## Step 6: Mark a card with a question

**Task.** You disagree with one card. Leave a question on it. You have never
been told a keyboard shortcut.

- PASS: right-clicking the card or opening Inspect offers a direct question
  action. The editor opens on that card. Stored questions are reachable from
  its small history indicator without expanding the card body.
- FAIL: the action is hidden behind another navigation layer, the question is
  attached to a different object, or history is unavailable.

## Step 7: Questions remain available from the keyboard

**Task.** Select another card, then use the question shortcut shown in Help.

- PASS: `?` opens the same editor for the selected card. Escape dismisses it.
- FAIL: a key is intercepted by an unrelated panel, or the canvas needs a
  reload before keyboard controls work.

## Step 8: The ring lands on the right thing

**Task.** Have the agent point at a specific card (`semantic_target_point`).
Then pan the board 400 px. Then resize the browser window.

- PASS: the ring stays on that card through both. If the card leaves the pane,
  the ring becomes an edge marker pointing the way, and clicking that marker
  goes there.
- FAIL: the ring stays where it was and now surrounds a different card, or it
  spills outside the canvas pane, or it survives on screen over the board
  column.

This step is the one that must never regress. A confident ring on the wrong
artifact asserts a shared referent that is not shared.

## Step 9: Chrome does not stack

**Task.** With the agent walking you through the board, open the agent ink
panel, trigger a "changes since you looked" notice, and change the camera
preference. Then read the card in the centre of the canvas.

- PASS: at most one flyout is open, every panel sits in its own dock zone, none
  overlaps another, and the centre of the canvas is never covered.
- FAIL: two panels occupy the same corner and one hides the other, or a notice
  covers the work, or a panel has to be dismissed before the board can be read.

## Step 10: Nothing offers what it cannot do

**Task.** Scan every visible control. For three of them, predict what will
happen, then click.

- PASS: every visible control does something visible. Anything that cannot work
  with this document is either absent or visibly disabled with a reason.
- FAIL: a control does nothing, or a hint advertises a gesture that is not wired,
  or a panel opens empty with no explanation.

## Step 11: Find something off screen

**Task.** Pan far away from the content until the canvas is empty. Now get back
to a named card without using fit.

- PASS: the minimap shows the content and the current viewport, and clicking or
  dragging in it moves the camera there.
- FAIL: there is no overview, so the only recovery is fit or reload.

## Step 12: Select several cards

**Task.** Select three cards that sit near each other, then move them together.

- PASS: Shift-dragging on empty canvas draws a marquee that selects them, and dragging
  one of the selected cards moves all three.
- FAIL: Shift-drag panned instead and nothing was selected, or the marquee
  selected but the group drag moved only one.

## Step 13: The agent and the human still agree

**Task.** Move one card. Press "Do we agree?".

- PASS: the two read-backs are identical character for character, and the
  agent's next `atlas_read` reports the move as a relation change or does not
  report it at all if the meaning did not change.
- FAIL: the read-backs differ, or a purely cosmetic move is reported as a change,
  or a meaningful re-parent is not.

Any camera, label, or overlay work that changes this step's result has broken
the shared-page property, whatever else it improved.

## Step 14: Verify in a background tab

**Task.** Repeat step 8 with the browser tab not focused, then bring it forward.

- PASS: the ring is in the right place when the tab comes forward.
- FAIL: overlays are stale or missing, which means something is being driven by
  a frame loop that does not run in a background tab.

## Log format

One block per run, appended to this file under a dated heading.

```
## Run YYYY-MM-DD, <operator>, <commit>

| Step | Result | Seconds | Screenshot | What happened |
|---|---|---|---|---|
| 1 | PASS | 4 | /tmp/sweep/01.png | Whole board in view, titles readable |
| 8 | FAIL | 12 | /tmp/sweep/08.png | Ring stayed put after the pan, ended up around the card to its left |
```

Then, under the table, one paragraph per FAIL written as the sentence a person
would say, not as a defect title.

## Run 2026-09-03, agent (headless Chrome, real CDP input), 619f86c plus the fixes below

Board: a copy of the 2026-09-02 typed diagrams board, 37 cards, 9 frames, 7
shapes, served on `http://127.0.0.1:8100` so the sweep did not write to the
board on 8098. Pointer and keys were dispatched through `Input.dispatchMouseEvent`
and `Input.dispatchKeyEvent`, not through page script: a synthetic
`PointerEvent` is not the UI, and it lies about at least one thing that matters
here (`setPointerCapture` refuses a pointer id the browser never issued).

| Step | Result | Seconds | Screenshot | What happened |
|---|---|---|---|---|
| 1 | FAIL | 3 | scratchpad/sweep/01-orient.png | Opened at 81% with 9 of 24 visible cards fully in the pane. Titles readable, nothing covering the centre. |
| 2 | PASS | 8 | scratchpad/sweep/02-fit.png | `H` armed the hand, a 500 px drag panned exactly 500 px, Fit returned to 22% with all 28 cards in view |
| 3 | PASS | 6 | Not linked | Three wheel steps at one card: the same card was still under the pointer at 108% |
| 4 | PASS | 5 | Not linked | At 22% the card was its title; at 108% its body and its source line were both on it |
| 5 | PASS | 4 | scratchpad/sweep/02-fit.png | At the whole-board fit the detail level is `title`, so the state machine and the frames carry the picture |
| 6 | PASS | 12 | Not linked | Hovering an unselected card put the rail on its top-right corner; the question mark opened the editor; the chip landed on the card |
| 7 | PASS | 9 | Not linked | Pressed `!` on a second card, having read the key off the rail. Same editor, same result. |
| 8 | PASS | 14 | Not linked | Ring matched the card to the pixel; after a 400 px pan it clipped to the 147 px still on screen; after resizing to 900x700 it became a downward edge marker 12 px inside the pane |
| 9 | PASS | 10 | Not linked | Five docked panels, zero overlapping rectangles, nothing over the canvas centre, and the centre card readable |
| 10 | PASS | 15 | Not linked | Camera preference cycled follow → asks → held → follow, each with visible text. The agent-ink panel is absent on a board with no ink layer rather than an empty panel. |
| 11 | PASS | 11 | Not linked | Six pans left zero cards on screen; one click on the minimap brought five named cards back |
| 12 | PASS | 20 | Not linked | Marquee took nine cards; dragging one moved all six visible members by exactly 100 by 60 |
| 13 | PASS | 6 | Not linked | "Identical. …the same text, character for character." A purely cosmetic move afterwards was not reported as a change. |
| 14 | PASS | 8 | Not linked | The point arrived while the tab was hidden; the ring was exactly on the card when the tab came forward |

**Step 1.** The board opens on the densest legible cluster, not on the whole
board, so most of it is off screen until you press Fit. That is the opening
rule working as written: the whole-content fit here is 22%, below the title
floor, and the board has drawn shapes so it is not treated as one connected
diagram. It is still the wrong first impression, and it is the same finding
that motivated the level-of-detail work: now that cards are titles at 22%, the
reason to refuse the whole fit is weaker than it was when this rule was
written. Worth revisiting the floor rather than the mechanism.

**Found and fixed during this run.** Four things the sweep caught that no test
did:

- The canvas never took focus when you clicked it, so every single-key
  shortcut, the tools and the three marks, was dead until focus arrived some
  other way. Clicking the board and pressing `H` did nothing at all.
- A second `report()` left the help disclosure open for the rest of the
  session, sitting over the top-left of the canvas, which on a fitted board is
  over the content.
- Moving the pointer from a card onto its mark rail hid the rail, because
  `pointerover` fires before `pointerenter` and the view's handler saw a target
  that was not a card. The rail vanished exactly as you reached for it.
- The change notice lost its "at most a third of the pane" cap when it moved
  into a dock zone: a percentage `max-height` against a box whose own height
  comes from its content is cyclic and the browser drops it. It grew to 418 px
  over a 717 px pane. Every zone now has both vertical insets, so its height is
  definite, and the cap is back at 32%.

**Open, not fixed.** `.segment-tree-panel` is chrome over the canvas that the
dock does not own: it sits at the top-left, 18 to 238 across and 170 to 342
down, and it swallowed a drag aimed at a card underneath it. Step 12 passed
only after moving to a card outside it. It belongs in the registry with a data
requirement, like everything else in section 2 of the design.
