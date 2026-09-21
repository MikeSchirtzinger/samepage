// The human's half of the shared atlas.
//
// This module owns no model of its own. It instantiates the wasm replica,
// real yrs, compiled from the same `same-page-atlas-core` the host uses,
// speaks y-sync over the runtime's `/ws`, and renders whatever the document
// says. Every gesture here is a write to that document, which is why the
// agent's next `atlas_read` sees it without anyone posting an "intent".

import init, * as atlasWasm from "/pkg/same_page_atlas_web.js";
import { semanticTargetNamespace } from "/_agui/semantic-targets.js";
import { createSegmentPicker, decodeMaskRuns } from "./segment-picker.js";
import { openDecisionReview, openDecisionHistory } from "/decision-review.js";
import { focusGraph, linkedDetailViews } from "/explore-graph.js";
import { openSourceViewer, closeSourceViewer, sourceReferences } from "/source-view.js";

const ink_outline = (...args) => atlasWasm.ink_outline(...args);
const ink_palette = (...args) => atlasWasm.ink_palette(...args);
const shape_vocabulary = (...args) => atlasWasm.shape_vocabulary(...args);
const attention_payload = (...args) => atlasWasm.attention_payload(...args);
const panel_layout = (...args) => atlasWasm.panel_layout(...args);
const label_detail = (...args) => atlasWasm.label_detail(...args);

const SVG_NS = "http://www.w3.org/2000/svg";
const XHTML_NS = "http://www.w3.org/1999/xhtml";
const SOCKET_URL = `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/ws`;
const DRAG_WRITE_MS = 66; // ~15 CRDT writes/second while dragging
const NODE_DRAG_THRESHOLD_PX = 4;
const ATTENTION_SETTLE_MS = 200;
const DRAG_ATTENTION_KEEPALIVE_MS = 250;
const RECONNECT_MS = 1000;
const GLYPH_MEANING = { "?": "I don't follow this", "!": "I think this is wrong", "*": "this matters" };
/// Handle size in *screen* pixels. Divided by the camera scale before it is
/// written into world coordinates, so a grab target stays grabbable at any zoom.
const HANDLE_PX = 9;
/// Smallest box a resize handle will leave behind, in world units.
const MIN_SIZE = 16;
/// Roughly what `.inline-input` measures, so it can be kept inside the pane.
const INLINE_WIDTH = 262;
const NODE_TITLE_READABLE_SCALE = 11 / 13.5;
/// The zoom at which a whole authored diagram is still readable, measured on
/// a real browser rather than derived: 29% is the threshold the round-2
/// architecture payload was read at, and the same number
/// `round_two_architecture_hierarchy_is_readable_in_one_fit` holds the layout
/// engine to. It is the floor for showing a connected diagram entire; below
/// it the opening falls back to framing a legible cluster.
const WHOLE_DIAGRAM_LEGIBLE_SCALE = 0.29;
const GESTURE_CARD_STORAGE_KEY = "atlas.gesture-card-dismissed.v1";
const SEGMENT_TREE_STORAGE_KEY = "atlas.segment-tree-collapsed.v1";
const DIAGRAM_COLLAPSE_STORAGE_KEY = "atlas.diagram-collapsed.v1";
const ALTITUDE_LENS_STORAGE_KEY = "atlas.altitude-level.v1";
const LEARNER_CATALOG_SCHEMA = "atlas-learner-catalog-v1";
/// `.atlas-changes` width, and the most of the pane it may take before an
/// arrival announcement stays collapsed. Both live here because the rule is
/// arithmetic the renderer has to do, and reading it out of the stylesheet at
/// runtime would be a second source of truth for the same number.
const PANEL_WIDTH_PX = 320;
const PANEL_SHARE = 1 / 3;
// The zones `panels.rs` places into, in the order they are created. The
// stylesheet positions these six selectors and nothing else positions chrome.
const DOCK_ZONES = [
  "top-left",
  "top-right",
  "bottom-left",
  "bottom-centre",
  "bottom-right",
  "bottom-strip",
];
const TOOL_SHORTCUTS = new Set(["p", "r", "o", "d", "a", "l", "t", "v", "f", "h"]);
// These gestures are local camera preferences, independent of shared selection.
const DEFAULT_HINT_FIRST =
  "drag background to pan · Shift-drag to select · scroll or pinch to zoom · double-click a linked card to explore";

// The fit rules moved into `same-page-atlas-core`'s `camera.rs` and are
// reached through `AtlasCamera.plan_opening`. They are geometry the host and
// the browser both have to agree about, and two implementations of that are
// two opinions. `NODE_TITLE_READABLE_SCALE`, `WHOLE_DIAGRAM_LEGIBLE_SCALE`,
// and the palette inset are still measured here, because they are facts about
// this page's chrome, and passed in.

// The drawing tools, in toolbar order. `form` is the core crate's vocabulary;
// nothing here invents a shape the shared model cannot store.
const TOOLS = [
  { key: "v", digit: "1", tool: "select", form: null, glyph: "↖", title: "Select: drag objects or drag a selection box" },
  // Hand pans even when the pointer starts over an object.
  { key: "h", tool: "hand", form: null, glyph: "✋︎", title: "Hand: drag anywhere to move the board" },
  { key: "p", tool: "draw", form: "ink", glyph: "✎", title: "Pen: circle things or strike them out" },
  { key: "r", tool: "draw", form: "rect", glyph: "▭", title: "Box: group nodes inside it" },
  { key: "o", tool: "draw", form: "ellipse", glyph: "◯", title: "Ellipse" },
  { key: "d", tool: "draw", form: "diamond", glyph: "◇", title: "Diamond" },
  { key: "a", tool: "draw", form: "arrow", glyph: "→", title: "Arrow: drag between two cards to bind it" },
  { key: "l", tool: "draw", form: "line", glyph: "╱", title: "Line" },
  { key: "t", tool: "draw", form: "text", glyph: "T", title: "Text: click where it goes, then type" },
  { key: "f", tool: "draw", form: "frame", glyph: "⊞", title: "Frame: a named region that holds drawn shapes" },
];

// Stroke widths the palette offers, Excalidraw's three. Any width in the
// model's range still renders; these are only what a click can pick.
const TOOL_ORDER = ["h", "v", "r", "d", "o", "a", "l", "p", "t", "f"];
const TOOL_DIGITS = { "1": "v", "2": "r", "3": "d", "4": "o", "5": "a", "6": "l", "7": "p", "8": "t" };
const TOOL_ICONS = {
  v: "M5 3l14 9-7 1-3 7z", h: "M8 12V6a1.5 1.5 0 013 0v5-7a1.5 1.5 0 013 0v7-6a1.5 1.5 0 013 0v7-3a1.5 1.5 0 013 0v6c0 5-3 7-7 7-3 0-4-2-6-4l-3-4a1.5 1.5 0 012-2l2 2",
  r: "M5 4h14a1 1 0 011 1v14a1 1 0 01-1 1H5a1 1 0 01-1-1V5a1 1 0 011-1z",
  d: "M12 3l9 9-9 9-9-9z", o: "M21 12a9 9 0 11-18 0 9 9 0 0118 0",
  a: "M3 12h18m-6-6 6 6-6 6", l: "M3 17L21 7", p: "m4 16 11-11 4 4L8 20l-5 1zM13 7l4 4",
  t: "M4 5V3h16v2M12 3v18m-4 0h8", f: "M7 3v18M17 3v18M3 7h18M3 17h18",
};
const STROKE_WIDTHS = [
  { value: 1, glyph: "─", title: "thin" },
  { value: 2, glyph: "━", title: "regular" },
  { value: 4, glyph: "▬", title: "bold" },
];
// Glyph and title per stroke style. The LIST of styles comes from the model
// (`shape_vocabulary()`), so a style the core adds shows up here with a
// plain-text fallback rather than not at all.
const STROKE_STYLE_LOOKS = {
  solid: { glyph: "──", title: "solid" },
  dashed: { glyph: "╌╌", title: "dashed: tentative" },
  dotted: { glyph: "┈┈", title: "dotted: faint" },
};
const STROKE_STYLES = [];
let DEFAULT_STROKE_WIDTH = 2;

// `stroke-dasharray` for a style at a width. Scaled by the width so a bold
// dash is still a dash and not a run of squares.
function dashArray(style, width) {
  if (style === "dashed") return `${width * 4} ${width * 2.5}`;
  if (style === "dotted") return `${Math.max(width * 0.01, 0.1)} ${width * 2.2}`;
  return null;
}

// The closed ink vocabulary, resolved to real colour, by the core crate, once.
//
// This table used to live here. It is the same palette the `.excalidraw`
// export writes into `strokeColor`, so a second copy in JavaScript was a
// standing invitation for a drawing to change colour on the way out of the
// page. The renderer asks the shared model instead.
const INK_CSS = {};
const inkOf = (name) => INK_CSS[name] || INK_CSS.slate || "#93a1bd";

export async function activate(ctx) {
  await init();
  Object.assign(INK_CSS, JSON.parse(ink_palette()));
  {
    const vocabulary = JSON.parse(shape_vocabulary());
    DEFAULT_STROKE_WIDTH = vocabulary.default_stroke_width;
    STROKE_STYLES.length = 0;
    for (const value of vocabulary.stroke_styles) {
      STROKE_STYLES.push({ value, ...(STROKE_STYLE_LOOKS[value] || { glyph: value, title: value }) });
    }
  }

  const wasmSchemaVersion = typeof atlasWasm.doc_schema_version === "function"
    ? atlasWasm.doc_schema_version()
    : "missing";
  const doc = new atlasWasm.AtlasDoc();
  const traceApi = Object.freeze({
    schema: "same-page-trace-v1",
    snapshot: () => JSON.parse(atlasWasm.trace_snapshot()),
    clear: () => atlasWasm.trace_clear(),
    captureDocument: () => {
      doc.snapshot();
      return JSON.parse(atlasWasm.trace_snapshot());
    },
    benchmark: (iterations = 128, samples = 5) =>
      JSON.parse(atlasWasm.trace_benchmark(iterations, samples)),
  });
  Object.defineProperty(globalThis, "__samePageAtlasTrace", {
    value: traceApi,
    configurable: true,
  });
  const view = ctx.mount;
  const atlasPane = view.closest(".atlas-pane");
  const learningView = document.getElementById("learning-view");
  const learnerMode = document.body.dataset.atlasMode === "learn";
  const targets = semanticTargetNamespace("atlas");
  const world = element("div", "atlas-world");

  // Three layers, and the split is a real decision rather than a convenience.
  //
  // The model gives every shape one `z`, but a filled box that groups cards
  // has to sit BEHIND them or it hides what it is grouping, and a strike
  // through a card has to sit IN FRONT or it is invisible. So the form picks
  // the layer and `z` orders within it. Pretending `z` were global would mean
  // half the shapes could only ever be drawn wrong.
  // Frames are the FIRST thing appended to the world, so a named container
  // never paints over the cards it owns. It is a DOM layer rather than an SVG
  // one because a frame carries a title band with real text in it, and text
  // that has to stay legible at every zoom belongs in the DOM.
  const frameLayer = element("div", "atlas-frames");
  const backLayer = svg("atlas-shapes-back"); // box forms: containers, regions
  const constraintBackLayer = svg("constraint-layer-back");
  const edgeLayer = svg("atlas-edges"); // node-to-node links
  const frontLayer = svg("atlas-shapes-front"); // ink, lines, arrows, text
  const constraintForeLayer = svg("constraint-layer-fore");
  // Handles for whatever is selected. Its own layer above everything, because
  // a grab target you cannot reach is not a grab target, an endpoint handle
  // has to stay clickable even where the arrow disappears under a card.
  const handleLayer = svg("atlas-handles");
  const diagLayer = element("div", "diag-root");
  const explanationPanel = element("aside", "atlas-explanation-flow");
  explanationPanel.hidden = true;
  explanationPanel.setAttribute("aria-label", "Guided explanation");
  const explanationPlayer = element("section", "atlas-explanation-player");
  explanationPlayer.hidden = true;
  explanationPlayer.setAttribute("aria-label", "Diagram walkthrough");
  const explorationNav = element("nav", "exploration-nav");
  explorationNav.setAttribute("aria-label", "Blueprint and exploration navigation");
  const guidedViews = element("nav", "guided-views");
  guidedViews.setAttribute("aria-label", "Guided views");
  const explorationLibrary = element("section", "exploration-library");
  explorationLibrary.setAttribute("aria-label", "Visual explorations");
  let openedExploration = null;
  let focusedObject = null;
  const objectFocusTrail = [];
  let focusVisible = null;
  let walkthroughCollapsed = true;
  try { walkthroughCollapsed = localStorage.getItem("atlas.walkthrough-collapsed.v1") !== "false"; } catch { /* Local preference only. */ }
  const setWalkthroughCollapsed = collapsed => {
    walkthroughCollapsed = collapsed;
    explanationPlayer.classList.toggle("is-collapsed", collapsed);
    try { localStorage.setItem("atlas.walkthrough-collapsed.v1", String(collapsed)); } catch { /* Local preference only. */ }
    const button = explanationPlayer.querySelector(".walkthrough-toggle");
    if (button) { button.textContent = collapsed ? "Expand walkthrough" : "Minimize walkthrough"; button.setAttribute("aria-expanded", String(!collapsed)); }
    window.dispatchEvent(new Event("resize"));
  };
  const localViewKey = "atlas.local-view.v1";
  let pendingLocalView = null;
  try { pendingLocalView = JSON.parse(localStorage.getItem(localViewKey) || "null"); } catch { /* An unavailable preference does not prevent opening a view. */ }
  const linkedView = new URLSearchParams(globalThis.location.search).get("view");
  if (linkedView) pendingLocalView = { id: linkedView, overview: true };
  const saveLocalView = () => {
    try { localStorage.setItem(localViewKey, JSON.stringify(focusedObject ? { focus: focusedObject } : openedExploration ? { id: openedExploration, beat: explorationPreview, target: walkthroughTarget, overview: explorationOverview } : null)); } catch { /* The view remains usable without preference storage. */ }
  };
  let blueprintCamera = null;
  let blueprintViewport = null;
  let explorationNavSignature = "";
  let explorationPreview = null;
  let walkthroughTarget = null;
  let walkthroughHistory = [];
  let walkthroughExtraStop = null;
  let explorationEditing = false;
  let explorationOverview = false;
  const explorationDrafts = new Map();
  const learnerDestination = element("section", "atlas-learner-destination");
  learnerDestination.hidden = true;
  learnerDestination.setAttribute("aria-live", "polite");
  learnerDestination.setAttribute("aria-label", "Learning navigation");
  const learnerToolbarNavigation = element("nav", "atlas-learner-toolbar-navigation");
  learnerToolbarNavigation.setAttribute("aria-label", "Refresher navigation");
  const learnerBackLink = element("a", "atlas-learner-workspace-link", "Back to workspace");
  learnerBackLink.dataset.learnerNavigation = "workspace";
  const learnerTopicsButton = element("button", "atlas-learner-topics-button", "Switch refresher");
  learnerTopicsButton.type = "button";
  learnerTopicsButton.dataset.learnerNavigation = "topics";
  learnerTopicsButton.addEventListener("click", () => setLearnerView("topics"));
  learnerToolbarNavigation.append(learnerBackLink, learnerTopicsButton);
  for (const eventName of ["pointerdown", "pointermove", "pointerup", "click", "wheel"]) {
    explanationPanel.addEventListener(eventName, (event) => event.stopPropagation());
  }
  world.append(frameLayer, backLayer, constraintBackLayer, edgeLayer);
  view.replaceChildren(world, diagLayer);
  // One container per dock zone. The zone owns the coordinates; a panel owns
  // its own contents and nothing else. This is what replaces four panels each
  // carrying its own `right: 14px; top: 62px`, two of which were identical.
  //
  // `pointer-events: none` on the container and `auto` on its children: an
  // empty zone must not eat a drag that lands in the corner it reserves.
  const docks = new Map();
  for (const zone of DOCK_ZONES) {
    const dock = element("div", "dock");
    dock.dataset.zone = zone;
    docks.set(zone, dock);
    view.append(dock);
  }
  /// Elements the registry may place, by the id it knows them as. A panel
  /// that has not been built yet is simply absent from this map, and the
  /// layout for it is ignored rather than being an error.
  const panelElements = new Map();
  if (learnerMode && atlasPane) {
    atlasPane.querySelector(".atlas-toolbar")?.prepend(learnerToolbarNavigation);
    atlasPane.append(explanationPanel, learnerDestination);
  } else {
    document.getElementById("dock-guide")?.append(explanationPanel);
    view.after(explanationPlayer);
    view.before(explorationNav, guidedViews);
    document.getElementById("dock-guide")?.prepend(explorationLibrary);
  }
  atlasPane?.classList.toggle("atlas-learner-mode", learnerMode);
  world.append(frontLayer, constraintForeLayer, handleLayer); // after the nodes, which are appended as they render
  world.inert = true;
  view.inert = true;
  diagLayer.replaceChildren(schemaRegister("waiting", wasmSchemaVersion));

  const nodeElements = new Map();
  const shapeTargetIds = new Set();
  const constraintTargetElements = new Map();
  // The camera lives in the core, compiled from the same `camera.rs` the host
  // links. Every gesture is forwarded into it and every pose comes back out;
  // nothing on this page computes a transform, a scale, a fit, or an anchored
  // zoom. `camera` below is a read-only mirror of that pose, written only by
  // `syncCamera`, so rendering that legitimately needs the zoom (a grab
  // handle's world size is the inverse of it) can read one without anybody
  // being tempted to assign to it.
  const atlasCamera = new atlasWasm.AtlasCamera();
  const camera = { x: 0, y: 0, scale: 1 };
  function syncCamera() {
    const pose = JSON.parse(atlasCamera.state());
    camera.x = pose.x;
    camera.y = pose.y;
    camera.scale = pose.scale;
  }
  /// The pane, as the core measures panes. One place, so a viewport read and a
  /// fit can never disagree about which rectangle they meant.
  function paneViewport() {
    const box = view.getBoundingClientRect();
    return { width: box.width, height: box.height };
  }
  // Who may move the camera, and when.
  //
  // Moving the view is part of explaining something: "look over here instead"
  // is not a thing you can say by drawing. So an agent taking the camera is
  // wanted during a walkthrough and unwanted while the human is reading the
  // board on their own, which makes it a preference rather than a rule.
  // `follow` lets a deliberate reveal move the view, `ask` turns every reveal
  // into a clickable offer at the pane edge, `hold` keeps the camera the
  // human's alone. Under every policy a reveal the human clicked on is
  // honoured: asking for it IS the permission.
  const CAMERA_POLICIES = ["follow", "ask", "hold"];
  const CAMERA_POLICY_STORAGE_KEY = "atlas.camera-policy";
  // How long after a pan, zoom, or drag the camera still counts as busy. Even
  // under `follow`, a reveal that lands inside this window yields and offers
  // instead: a view that jumps out from under a moving hand is the one thing
  // no policy should permit.
  const CAMERA_BUSY_MS = 1200;
  let cameraPolicy = storedCameraPolicy();
  // A fit is not a camera grab. It frames a document the human has not
  // touched yet, and it keeps re-fitting while content is still arriving,
  // because a CRDT delivers a scene over several messages and latching onto
  // the first of them leaves the rest of the board outside the pane. The
  // first gesture ends it for good.
  let fittedExtent = null;
  let cameraTouched = false;
  let cameraReturn = null;
  let lastCameraGesture = 0;
  let handOnPane = false;
  let attentionTimer = null;
  let attentionDragTimer = null;
  let attentionDragging = null;
  let attentionErrorShown = false;
  let atlas = {
    nodes: [],
    edges: [],
    marks: [],
    claims: [],
    shapes: [],
    constraints: [],
    constraint_state: { status: "sat", core: [] },
  };
  let painting = { shapes: [], links: [] };
  let stateMachinePaint = [];
  let statePaintByMachine = new Map();
  let currentStateIds = new Set();
  let takenTransitionIds = new Set();
  // Set once the host confirms this document carries an agent-ink layer
  // (`/atlas/agent-ink`). Left `null` otherwise, so `render()` calling it
  // unconditionally costs nothing and builds nothing when the feature is off.
  let agentInkRefresh = null;
  let segmentControlsRefresh = null;
  let segmentPicker = null;
  // Selection is a SET, and `selected` is its first member.
  //
  // Keeping both is not redundancy. A single click is the overwhelmingly
  // common case, every panel and every mark path already speaks "the selected
  // thing", and the semantic contract still names one primary target. So the
  // set is the truth and `selected` is the head of it, maintained in one place
  // (`selectSet`) so the two can never disagree.
  let selected = null;
  let selection = [];
  // Space turns the pointer into a pan, the way every canvas tool does it.
  // Shift-drag selects a region; an ordinary background drag pans.
  let spaceHeld = false;
  // A viewing preference, deliberately not CRDT state. It lets the human
  // inspect all back-salience constraints at full weight for this session.
  let backConstraintsForward = false;
  let socket = null;
  let hostSchemaVersion = "waiting";
  let schemaReady = false;
  let inlineInput = null;
  let inlineDirect = null;
  const collapsedSegments = new Set(readStoredStringArray(SEGMENT_TREE_STORAGE_KEY));
  const collapsedContainers = new Set(readStoredStringArray(DIAGRAM_COLLAPSE_STORAGE_KEY));
  const storedAltitudeLevel = readStoredAltitudeLevel(ALTITUDE_LENS_STORAGE_KEY);
  let altitudeLevel = storedAltitudeLevel ?? "all";
  let altitudeNeedsInitialApply = storedAltitudeLevel !== null;
  let altitudeAppliedDeepest = null;
  let altitudeManualOverride = false;
  let sourcePopoverTarget = null;
  let contextMenu = null;
  let selectionRefreshAt = 0;
  // Which tool the pointer is holding. `select` is the old behaviour exactly:
  // drag a card, pan the background.
  let active = TOOLS[0];
  let inkColor = "amber";
  // The stroke a new shape gets, and what a click on the palette writes onto
  // the selected one. Only non-defaults are written, so a document authored
  // before these existed does not gain a property per shape.
  let strokeWidth = DEFAULT_STROKE_WIDTH;
  let strokeStyle = "solid";
  const strokeOf = () => ({
    ...(strokeWidth !== DEFAULT_STROKE_WIDTH ? { stroke_width: strokeWidth } : {}),
    ...(strokeStyle !== "solid" ? { stroke_style: strokeStyle } : {}),
  });

  const syncLabel = document.getElementById("sync-state");
  const hint = document.getElementById("atlas-hint");
  const conversation = document.getElementById("conversation");
  const returnViewButton = element("button", "tool camera-return", "Return view");
  returnViewButton.type = "button";
  returnViewButton.title = "Return to your view";
  returnViewButton.setAttribute("aria-label", "Return to your view before the agent walkthrough");
  returnViewButton.hidden = true;
  // Always visible, unlike the return button: a preference you cannot see is
  // one you cannot change while the thing it governs is annoying you.
  const cameraPolicyButton = element("button", "tool camera-policy", "");
  cameraPolicyButton.type = "button";
  const constraintSalienceButton = element("button", "tool constraint-salience", "");
  constraintSalienceButton.type = "button";
  constraintSalienceButton.setAttribute("aria-keyshortcuts", "Alt+B");

  // ── transport ───────────────────────────────────────────────────────

  function rawSend(frame) {
    if (socket && socket.readyState === WebSocket.OPEN) socket.send(frame);
  }

  function flush() {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    for (const frame of doc.take_outbound()) rawSend(frame);
  }

  function setSync(text, live) {
    if (!syncLabel) return;
    syncLabel.textContent = live ? "" : text;
    syncLabel.setAttribute("aria-label", text);
    syncLabel.title = text;
    syncLabel.classList.toggle("live", Boolean(live));
  }

  function connect() {
    if (!schemaReady || socket) return;
    socket = new WebSocket(SOCKET_URL);
    socket.binaryType = "arraybuffer";
    socket.onopen = () => {
      // The handshake resyncs from state vectors, so anything queued while the
      // socket was down is about to be re-derived; sending it too would just
      // duplicate work.
      doc.take_outbound();
      rawSend(doc.hello());
      setSync("replica live", true);
      view.inert = false;
      scheduleRender();
    };
    socket.onmessage = (event) => {
      try {
        for (const reply of doc.receive(new Uint8Array(event.data))) rawSend(reply);
        scheduleRender();
        // Frames are still arriving, so the page the human is "looking at" is
        // not settled yet. Re-arming here rather than in render() tracks the
        // sync itself: a render only fires when the replica turns dirty, and
        // the handshake can go quiet for a beat mid-stream.
        if (seenDigest === null) scheduleBaseline();
      } catch (error) {
        window.atlasValidation.error("sync_error", error);
        report(`sync frame rejected: ${error}`);
      }
    };
    socket.onerror = () => socket?.close();
    socket.onclose = () => {
      if (!schemaReady) {
        socket = null;
        return;
      }
      window.atlasValidation.invalidate();
      setSync("Replica offline. Reconnecting...", false);
      view.inert = true;
      socket = null;
      setTimeout(connect, RECONNECT_MS);
    };
  }

  function snapshotSchemaVersion(event) {
    const chrome = event?.snapshot?.chrome;
    return chrome?.atlas?.doc_schema_version ?? chrome?.doc_schema_version ?? "missing";
  }

  function applySchemaHandshake(event) {
    hostSchemaVersion = snapshotSchemaVersion(event);
    if (hostSchemaVersion !== wasmSchemaVersion) {
      schemaReady = false;
      world.inert = true;
      view.inert = true;
      diagLayer.replaceChildren(schemaRegister(hostSchemaVersion, wasmSchemaVersion));
      setSync("schema mismatch", false);
      if (socket) {
        socket.onclose = null;
        socket.close();
        socket = null;
      }
      return;
    }
    if (schemaReady) return;
    schemaReady = true;
    world.inert = false;
    diagLayer.replaceChildren();
    connect();
    scheduleRender();
  }

  ctx.client.on("STATE_SNAPSHOT", applySchemaHandshake);

  // ── local writes ────────────────────────────────────────────────────

  /// Whether the help disclosure is open because a report opened it, rather
  /// than because the human did.
  ///
  /// This used to be a local, and two reports in a row leaked it open for the
  /// rest of the session: the first report's timer bailed because the text
  /// had changed under it, and the second report saw an already-open
  /// disclosure and so believed it had not opened anything. The help panel
  /// then sat over the top-left corner of the canvas permanently, which on a
  /// fitted board is over the content.
  let helpOpenedForReport = false;
  let reportTimer = null;

  function report(message) {
    if (!hint) return;
    const disclosure = hint.closest("details");
    if (disclosure && !disclosure.open) {
      helpOpenedForReport = true;
      disclosure.open = true;
    }
    hint.textContent = message;
    clearTimeout(reportTimer);
    reportTimer = setTimeout(() => {
      reportTimer = null;
      paintDefaultHint();
      if (helpOpenedForReport && disclosure) {
        disclosure.open = false;
        helpOpenedForReport = false;
      }
    }, 4000);
  }

  function paintDefaultHint() {
    if (!hint) return;
    const first = element("span", "hint-line", DEFAULT_HINT_FIRST);
    const tools = element("span", "hint-line");
    tools.append(
      document.createTextNode("Tools: V select · H hand · P pen · R box · A arrow · T text · "),
      explanatoryGlyph("?", "question", GLYPH_MEANING["?"]),
      document.createTextNode(" · "),
      explanatoryGlyph("!", "disagree", GLYPH_MEANING["!"]),
      document.createTextNode(" · "),
      explanatoryGlyph("*", "emphasize", GLYPH_MEANING["*"]),
    );
    hint.replaceChildren(first, tools);
  }

  function explanatoryGlyph(glyph, word, meaning) {
    const control = element("button", "hint-glyph", `${glyph} ${word}`);
    control.type = "button";
    control.title = `${glyph} means ${meaning}`;
    control.setAttribute("aria-label", `${glyph} means ${meaning}. Explanatory only.`);
    control.addEventListener("pointerdown", (event) => event.stopPropagation());
    control.addEventListener("click", (event) => {
      event.stopPropagation();
      report(`${glyph} means ${meaning}. Select something, then choose the plain-word action.`);
    });
    return control;
  }

  paintDefaultHint();

  function write(operation, strict = false) {
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      report("Replica offline. No change was saved. Reconnecting...");
      if (strict) throw new Error("Replica offline. No change was saved.");
      return null;
    }
    try {
      const result = operation();
      flush();
      scheduleRender();
      return result;
    } catch (error) {
      report(String(error && error.message ? error.message : error));
      if (strict) throw error;
      return null;
    }
  }

  const place = (patch) => write(() => openedExploration
    ? doc.place_exploration_node(openedExploration, JSON.stringify(patch))
    : doc.place_node(JSON.stringify(patch)));
  // One human gesture is one intent, however many cards it moved.
  //
  // Writing a set member by member puts N transactions on the wire, N merges
  // on the host, and N commits in the ledger for a single hand movement, and
  // the read-back then files one act as N separate moves. Everything inside a
  // `write` shares one flush, so the whole set travels as one.
  const placeSet = (patches) => write(() => {
    let last = null;
    for (const patch of patches) last = doc.place_node(JSON.stringify(patch));
    return last;
  });
  const markTarget = (target, glyph, text) => {
    let id;
    try {
      id = doc.mark(target, glyph, text);
      flush();
      scheduleRender();
    } catch (error) {
      console.error("atlas mark commit failed", error);
      report("didn't save. Try again.");
      return null;
    }
    let saved = false;
    try {
      const snapshot = JSON.parse(doc.snapshot());
      saved = Boolean(id) && snapshot.marks.some((mark) => mark.id === id);
    } catch (error) {
      console.error("atlas mark receipt could not be checked", error);
    }
    if (!saved) {
      report("didn't save. Try again.");
      return null;
    }
    return id;
  };
  const openChallenge = (targetId) => openMarkEditor(targetId, "!");
  const requestClaims = (nodeId) => {
    let id;
    try {
      id = doc.challenge(nodeId);
      flush();
      scheduleRender();
    } catch (error) {
      console.error("atlas challenge commit failed", error);
      report("didn't save. Try again.");
      return null;
    }
    const label = titleForTarget(nodeId);
    report(`Challenge opened on "${label}". The agent will answer with claims you can accept or reject.`);
    return id;
  };
  /// Mark a whole set with one glyph and one reason, in one transaction.
  ///
  /// The mark model stores one target per mark, so a set becomes N marks. That
  /// is the honest translation and not a silent narrowing: the human said this
  /// about these six things, and every one of the six carries it. The refusal
  /// case is a mark that could not be written, which is reported by name
  /// rather than folded into a success.
  const markEach = (ids, glyph, text) => {
    const written = [];
    try {
      for (const id of ids) {
        const markId = doc.mark(id, glyph, text);
        if (markId) written.push([id, markId]);
      }
      flush();
      scheduleRender();
    } catch (error) {
      console.error("atlas set mark commit failed", error);
      report("didn't save. Try again.");
      return null;
    }
    let saved = [];
    try {
      const snapshot = JSON.parse(doc.snapshot());
      const live = new Set(snapshot.marks.map((mark) => mark.id));
      saved = written.filter(([, markId]) => live.has(markId));
    } catch (error) {
      console.error("atlas set mark receipt could not be checked", error);
    }
    if (saved.length !== ids.length) {
      const missing = ids.filter((id) => !saved.some(([target]) => target === id));
      report(
        `${glyph} reached ${saved.length} of ${ids.length}. Not written: ${missing
          .map((id) => titleForTarget(id))
          .join(", ")}.`,
      );
      return saved.length ? saved.map(([, markId]) => markId) : null;
    }
    report(`${glyph} ${GLYPH_MEANING[glyph]} attached to all ${ids.length}.`);
    return saved.map(([, markId]) => markId);
  };

  const removeObject = (id) => write(() => doc.remove(id));
  const placeScopedShape = (patch) => openedExploration
    ? doc.place_exploration_shape(openedExploration, JSON.stringify(patch))
    : doc.place_shape(JSON.stringify(patch));
  const placeShape = (patch) => write(() => placeScopedShape(patch));
  const placeShapeSet = (patches) => write(() => {
    let last = null;
    for (const patch of patches) last = placeScopedShape(patch);
    return last;
  });
  const liftShape = (id) => write(() => doc.lift_shape(id));
  const materializeSegment = (id, result) => write(() => (
    doc.materialize_segment(id, JSON.stringify(result))
  ));

  // The agent authors semantics and a source-pixel box. The browser owns the
  // real WebGPU measurement that turns that proposal into pixels. Results go
  // back through the same CRDT replica, so neither side can manufacture the
  // other side's half of the receipt.
  const segmentWorker = new Worker(new URL("./segment-worker.js", import.meta.url), { type: "module" });
  const segmentInflight = new Map();
  const segmentReceipts = new Map();
  const segmentResults = new Map();
  const segmentErrors = [];
  const segmentInputEvents = [];
  let segmentRequest = 0;
  segmentWorker.addEventListener("message", ({ data }) => {
    if (segmentPicker?.handleMessage(data)) return;
    const key = segmentInflight.get(data.requestId);
    if (key) segmentInflight.delete(data.requestId);
    if (data.type === "segment-error") {
      const failure = { id: data.id, requestId: data.requestId, error: data.error };
      segmentErrors.push(failure);
      console.error("Atlas MobileSAM materialization failed", failure);
      report(`MobileSAM failed for ${data.id}: ${data.error}`);
      return;
    }
    if (data.type !== "segment-result") return;
    const current = JSON.parse(doc.snapshot()).shapes.find((shape) => shape.id === data.id);
    if (!current
        || current.segment_status !== "proposed"
        || current.segment_generation !== data.result?.generation) {
      segmentErrors.push({
        id: data.id,
        requestId: data.requestId,
        error: "stale worker result refused before CRDT materialization",
      });
      return;
    }
    segmentReceipts.set(`${data.id}:${data.result.generation}`, data.result.receipt);
    segmentResults.set(`${data.id}:${data.result.generation}`, data.result);
    materializeSegment(data.id, data.result);
  });
  segmentWorker.addEventListener("error", (event) => {
    const failure = { id: null, requestId: null, error: event.message || "segment worker crashed" };
    segmentErrors.push(failure);
    console.error("Atlas MobileSAM worker crashed", event);
    report(failure.error);
    segmentPicker?.handleWorkerError(failure.error);
  });

  function ensureSegmentMaterializations() {
    for (const segment of atlas.shapes.filter((shape) => (
      shape.form === "segment" && shape.segment_status === "proposed"
    ))) {
      const key = `${segment.id}:${segment.segment_generation}`;
      if ([...segmentInflight.values()].includes(key)) continue;
      const requestId = `atlas-segment-${++segmentRequest}`;
      segmentInflight.set(requestId, key);
      segmentWorker.postMessage({ type: "segment", requestId, id: segment.id, segment });
    }
  }

  function isMotionProgram(value) {
    return Boolean(
      value
      && value.schema === "atlas-segment-motion-v1"
      && typeof value.label === "string"
      && Array.isArray(value.tracks),
    );
  }

  function segmentById(id) {
    return atlas.shapes.find((shape) => shape.form === "segment" && shape.id === id) || null;
  }

  function segmentAncestors(segment) {
    const chain = [];
    const seen = new Set();
    let current = segment;
    while (current && !seen.has(current.id)) {
      chain.push(current);
      seen.add(current.id);
      current = current.segment_parent_id ? segmentById(current.segment_parent_id) : null;
    }
    return chain;
  }

  function segmentRootId(segment) {
    return segmentAncestors(segment).at(-1)?.id || segment.id;
  }

  function segmentDepth(segment) {
    return Math.max(0, segmentAncestors(segment).length - 1);
  }

  function motionOwnerFor(segmentId) {
    const segment = segmentById(segmentId);
    if (!segment) return null;
    for (const owner of segmentAncestors(segment)) {
      if (isMotionProgram(owner.segment_motion)) {
        const targetsSegment = owner.id === segmentId || owner.segment_motion.tracks.some((track) => (
          Array.isArray(track.target_ids) && track.target_ids.includes(segmentId)
        ));
        if (targetsSegment) return { owner, motion: owner.segment_motion };
      }
      const legacyTargetsSegment = owner.id === segmentId
        ? owner.segment_animation && owner.segment_animation !== "none"
        : owner.segment_animation === "tap-parts";
      if (legacyTargetsSegment) {
        return {
          owner,
          motion: { label: `Legacy ${owner.segment_animation}` },
        };
      }
    }
    return null;
  }

  function selectedSegmentFamily() {
    const segments = selection.map((id) => (
      atlas.shapes.find((shape) => shape.form === "segment" && shape.id === id)
    ));
    if (!segments.length || segments.some((segment) => !segment)) return null;
    const rootIds = new Set(segments.map(segmentRootId));
    if (rootIds.size !== 1) return null;
    const ownerId = [...rootIds][0];
    const owner = segmentById(ownerId);
    return owner ? { owner, segments } : null;
  }

  function prefillMotionRequest(event) {
    const family = selectedSegmentFamily();
    if (!family || !conversation?.ui?.input) {
      report("Select one object or several parts of the same object first.");
      return;
    }
    segmentInputEvents.push({
      trusted: event?.isTrusted === true,
      motionPrompt: family.segments.map((segment) => segment.id),
      ownerId: family.owner.id,
    });
    restoreComposerAnchor();
    document.querySelector('[data-dock-tab="chat"]')?.click();
    const names = family.segments.map((segment) => segment.label || "part").join(", ");
    conversation.ui.input.value = `Animate ${names}: `;
    conversation.ui.input.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
    queueMicrotask(() => conversation.ui.input.focus());
  }

  function clearSelectedMotion(event) {
    const family = selectedSegmentFamily();
    if (!family) {
      report("Select one object or several parts of the same object first.");
      return;
    }
    const active = motionOwnerFor(family.segments[0].id)
      || motionOwnerFor(family.owner.id);
    if (!active) return;
    segmentInputEvents.push({
      trusted: event?.isTrusted === true,
      motionCleared: active.owner.id,
    });
    write(() => doc.set_segment_motion(active.owner.id, "null"));
  }

  function replaySelectedMotion(event) {
    const family = selectedSegmentFamily();
    if (!family) {
      report("Select an object with motion first.");
      return;
    }
    const active = motionOwnerFor(family.segments[0].id)
      || motionOwnerFor(family.owner.id);
    if (!active) return;
    const replayed = replaySegmentMotion(active.owner.id);
    segmentInputEvents.push({
      trusted: event?.isTrusted === true,
      motionReplayed: active.owner.id,
      targetCount: replayed,
    });
  }

  function setUpSegmentObjectPanel() {
    const panel = element("section", "segment-flap-panel");
    const heading = element("div", "segment-flap-heading", "OBJECT CONTROL");
    const guidance = element("p", "segment-control-guidance", "Rename it here, add mask parts, or describe any 2D motion in chat.");
    const nameRow = element("label", "segment-control-field");
    nameRow.append(element("span", null, "Name"));
    const nameInput = element("input", "segment-name-input");
    nameInput.type = "text";
    nameInput.maxLength = 160;
    nameInput.autocomplete = "off";
    nameRow.append(nameInput);
    const addPart = element("button", "segment-add-part-button", "Add part mask");
    addPart.type = "button";
    const describeMotion = element("button", "segment-describe-motion-button", "Describe motion...");
    describeMotion.type = "button";
    const replayMotion = element("button", "segment-replay-motion-button", "Replay motion");
    replayMotion.type = "button";
    const stopMotion = element("button", "segment-stop-motion-button", "Stop motion");
    stopMotion.type = "button";
    const animationRow = element("div", "segment-animation-actions");
    animationRow.append(describeMotion, replayMotion, stopMotion);
    const motionStatus = element("p", "segment-motion-status", "No motion");
    const row = element("label", "segment-flap-row");
    const low = element("span", "segment-flap-limit", "down");
    const slider = element("input", "segment-flap-slider");
    slider.type = "range";
    slider.min = "-1";
    slider.max = "1";
    slider.step = "0.01";
    slider.value = "0";
    slider.setAttribute("aria-label", "Wing flap");
    const high = element("span", "segment-flap-limit", "up");
    const output = element("output", "segment-flap-output", "0 degrees");
    row.append(low, slider, high);
    panel.append(heading, guidance, nameRow, addPart, animationRow, motionStatus, row, output);
    panel.hidden = true;
    view.append(panel);
    let segmentId = null;
    let segmentName = "";

    describeMotion.addEventListener("click", (event) => prefillMotionRequest(event));
    replayMotion.addEventListener("click", (event) => replaySelectedMotion(event));
    stopMotion.addEventListener("click", (event) => clearSelectedMotion(event));
    addPart.addEventListener("click", () => {
      if (segmentId) segmentPicker?.show(segmentId);
    });
    const commitName = () => {
      const value = nameInput.value.trim();
      const current = atlas.shapes.find((shape) => shape.id === segmentId);
      if (!segmentId || !current) return;
      if (!value) {
        nameInput.value = segmentName;
        return;
      }
      if (value === segmentName) return;
      segmentName = value;
      placeShape({ id: segmentId, label: value });
    };
    nameInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        commitName();
        view.focus({ preventScroll: true });
      }
      if (event.key === "Escape") {
        nameInput.value = segmentName;
        view.focus({ preventScroll: true });
      }
    });
    nameInput.addEventListener("blur", commitName);
    slider.addEventListener("input", (event) => {
      const value = Number(slider.value);
      if (!segmentId || !Number.isFinite(value)) return;
      segmentInputEvents.push({ trusted: event.isTrusted, value });
      applySegmentFlap(segmentId, value);
      output.textContent = `${Math.round(value * 35)} degrees`;
      write(() => doc.set_segment_flap(segmentId, value));
    });

    return () => {
      const segment = atlas.shapes.find((shape) => (
        shape.id === selected
        && shape.form === "segment"
        && shape.segment_status === "materialized"
      ));
      const winged = Boolean(segment
        && Array.isArray(segment.segment_parts)
        && segment.segment_parts.length === 2
      );
      panel.hidden = !segment;
      segmentId = segment?.id || null;
      if (!segment) return;
      segmentName = segment.label || "";
      if (document.activeElement !== nameInput) nameInput.value = segmentName;
      const partCount = atlas.shapes.filter((shape) => (
        shape.form === "segment" && shape.segment_parent_id === segment.id
      )).length;
      const depth = segmentDepth(segment);
      const canAddPart = depth < atlasWasm.segment_max_depth();
      const active = motionOwnerFor(segment.id);
      motionStatus.textContent = active
        ? `Motion: ${active.motion.label}`
        : "No motion";
      replayMotion.disabled = !active;
      stopMotion.disabled = !active;
      guidance.textContent = partCount > 0 && !segment.segment_parent_id
        ? `Shift-click any of its ${partCount} parts, then describe their motion together.`
        : "Describe translation, rotation, scale, timing, looping, or a sequence in ordinary words.";
      addPart.textContent = partCount > 0 ? "Add nested part mask" : "Add part mask";
      addPart.disabled = !canAddPart;
      addPart.title = canAddPart
        ? `Add a mask directly beneath ${segment.label || "this part"}`
        : "This part is already at the hierarchy depth limit";
      row.hidden = !winged;
      output.hidden = !winged;
      if (!winged) return;
      const value = Number(segment.segment_flap) || 0;
      if (document.activeElement !== slider) slider.value = String(value);
      output.textContent = `${Math.round(value * 35)} degrees`;
      applySegmentFlap(segment.id, value);
    };
  }

  function setUpSegmentTree() {
    const panel = element("section", "segment-tree-panel");
    panel.setAttribute("aria-label", "Objects");
    const header = element("header", "segment-tree-header");
    header.append(element("span", "segment-tree-title", "OBJECTS"));
    const actions = element("div", "segment-tree-actions");
    const collapse = element("button", "segment-tree-collapse", "−");
    collapse.type = "button";
    collapse.setAttribute("aria-label", "Collapse objects");
    collapse.setAttribute("aria-expanded", "true");
    const add = element("button", "segment-tree-add", "+ mask");
    add.type = "button";
    add.addEventListener("click", () => segmentPicker?.show(""));
    actions.append(add, collapse);
    header.append(actions);
    const list = element("div", "segment-tree-list");
    list.setAttribute("role", "tree");
    panel.append(header, list);
    panel.hidden = true;
    view.append(panel);
    let draggedId = null;
    collapse.addEventListener("click", () => {
      const collapsed = panel.classList.toggle("collapsed");
      collapse.textContent = collapsed ? "+" : "−";
      collapse.setAttribute("aria-expanded", String(!collapsed));
      collapse.setAttribute("aria-label", collapsed ? "Expand objects" : "Collapse objects");
    });
    const persistCollapsed = () => {
      try {
        localStorage.setItem(SEGMENT_TREE_STORAGE_KEY, JSON.stringify([...collapsedSegments]));
      } catch {
        // Collapsing is still useful in storage-restricted browser contexts.
      }
    };
    const refresh = () => {
      const segments = atlas.shapes.filter((shape) => shape.form === "segment");
      panel.hidden = segments.length === 0;
      if (!segments.length) return;
      list.replaceChildren();
      const roots = segments.filter((shape) => !shape.segment_parent_id);
      const branchIds = new Set(segments
        .filter((shape) => segments.some((candidate) => candidate.segment_parent_id === shape.id))
        .map((shape) => shape.id));
      let pruned = false;
      for (const id of collapsedSegments) {
        if (branchIds.has(id)) continue;
        collapsedSegments.delete(id);
        pruned = true;
      }
      if (pruned) persistCollapsed();
      const appendBranch = (segment, depth) => {
        const children = segments.filter((shape) => shape.segment_parent_id === segment.id);
        const expanded = children.length > 0 && !collapsedSegments.has(segment.id);
        const row = element("div", "segment-tree-row");
        row.style.setProperty("--segment-tree-depth", String(depth));
        const disclosure = element(
          "button",
          `segment-tree-toggle${children.length ? "" : " segment-tree-toggle-empty"}`,
          children.length ? (expanded ? "▾" : "▸") : "",
        );
        disclosure.type = "button";
        disclosure.tabIndex = children.length ? 0 : -1;
        disclosure.disabled = children.length === 0;
        disclosure.setAttribute("aria-hidden", String(children.length === 0));
        if (children.length) {
          disclosure.setAttribute("aria-expanded", String(expanded));
          disclosure.setAttribute(
            "aria-label",
            `${expanded ? "Collapse" : "Expand"} ${segment.label || "object"}`,
          );
          disclosure.addEventListener("click", (event) => {
            event.stopPropagation();
            if (expanded) collapsedSegments.add(segment.id);
            else collapsedSegments.delete(segment.id);
            persistCollapsed();
            refresh();
          });
        }
        const button = element(
          "button",
          `segment-tree-item ${depth === 0 ? "segment-tree-root" : "segment-tree-part"}`,
          segment.label || (depth === 0 ? "Object" : "Part"),
        );
        button.type = "button";
        button.dataset.segmentId = segment.id;
        button.dataset.depth = String(depth);
        button.dataset.hasChildren = String(children.length > 0);
        button.setAttribute("role", "treeitem");
        button.setAttribute("aria-level", String(depth + 1));
        button.setAttribute("aria-selected", String(isSelected(segment.id)));
        if (children.length) button.setAttribute("aria-expanded", String(expanded));
        button.classList.toggle("selected", isSelected(segment.id));
        button.draggable = depth > 0;
        button.title = depth > 0
          ? "Shift-click to select with siblings. Drag onto another part to nest it."
          : "Shift-click to select with parts.";
        button.addEventListener("click", (event) => (
          event.shiftKey ? toggleSelected(segment.id) : select(segment.id)
        ));
        button.addEventListener("dragstart", (event) => {
          if (depth === 0) return;
          draggedId = segment.id;
          button.classList.add("dragging");
          event.dataTransfer?.setData("text/plain", segment.id);
          if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
        });
        button.addEventListener("dragend", () => {
          draggedId = null;
          button.classList.remove("dragging");
          list.querySelectorAll(".drop-target").forEach((item) => item.classList.remove("drop-target"));
        });
        button.addEventListener("dragover", (event) => {
          if (!draggedId || draggedId === segment.id) return;
          event.preventDefault();
          if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
          button.classList.add("drop-target");
        });
        button.addEventListener("dragleave", () => button.classList.remove("drop-target"));
        button.addEventListener("drop", (event) => {
          event.preventDefault();
          button.classList.remove("drop-target");
          const movingId = draggedId || event.dataTransfer?.getData("text/plain");
          draggedId = null;
          if (!movingId || movingId === segment.id) return;
          try {
            write(() => doc.set_segment_parent(movingId, segment.id));
            select(movingId, false);
            requestAnimationFrame(() => select(movingId));
            report(`Nested ${titleForTarget(movingId)} under ${segment.label || "this part"}.`);
          } catch (error) {
            report(error instanceof Error ? error.message : String(error));
          }
        });
        row.append(disclosure, button);
        list.append(row);
        if (expanded) {
          for (const child of children) appendBranch(child, depth + 1);
        }
      };
      for (const root of roots) appendBranch(root, 0);
    };
    return refresh;
  }

  const refreshSegmentControls = setUpSegmentObjectPanel();
  const refreshSegmentTree = setUpSegmentTree();
  segmentControlsRefresh = () => {
    refreshSegmentControls();
    refreshSegmentTree();
  };

  // The other half of an agent-ink binding: a human drag already wrote the
  // new x/y/w/h through the ordinary place_shape/place_node above, and this
  // reads that same number back into whatever variable represents it, then
  // lets a solve propagate it to anything downstream.
  //
  // Called once per committed drag, from `endDrag` alongside the write that
  // committed it. Never from the pointermove throttle above, which would
  // turn one gesture into a solve every ~66ms. `patch` is whatever was just
  // written; only the keys agent-ink can bind to are offered.
  function captureAgentInk(id, patch) {
    if (!agentInkRefresh) return; // off unless the host said this document carries agent ink
    let touched = false;
    for (const property of ["x", "y", "w", "h"]) {
      if (!(property in patch)) continue;
      touched = true;
      try {
        doc.agent_ink_capture(id, property, patch[property]);
      } catch (error) {
        // The property is solver-owned (free or derived): the wasm side has
        // already re-solved to put the shape back where its variable
        // actually has it, so this says why it snapped back instead of
        // leaving a human drag that looks like it silently did nothing.
        report(String(error && error.message ? error.message : error));
      }
    }
    if (touched) {
      flush();
      scheduleRender();
    }
  }

  // ── anchors ─────────────────────────────────────────────────────────

  /// Where one registered target lands on screen right now.
  ///
  /// This is handed to `targets.register` as `project`, and it is the reason
  /// the ring survives a pan. The page contributes three measurements, the
  /// pane's origin, the pane's clip, and the target's world box, and the core
  /// does the projection: camera, clip intersection, and the edge degradation
  /// with a direction when the target is off the pane entirely.
  function projectAnchor(id) {
    const world = worldBounds(id);
    if (!world) return { kind: "nowhere" };
    const pane = view.getBoundingClientRect();
    if (!pane.width || !pane.height) return { kind: "nowhere" };
    try {
      const placed = JSON.parse(atlasCamera.project_anchors(
        JSON.stringify({ x: pane.left, y: pane.top }),
        JSON.stringify([[id, { x: world.x, y: world.y, w: world.w, h: world.h }]]),
        JSON.stringify([{
          left: pane.left,
          top: pane.top,
          right: pane.right,
          bottom: pane.bottom,
        }]),
      ));
      return placed[0] ?? { kind: "nowhere" };
    } catch (error) {
      console.error("atlas anchor projection failed", error);
      return { kind: "nowhere" };
    }
  }

  // Every camera write reprojects, and so does a change in the pane's size.
  // There is no frame loop: a `requestAnimationFrame` poll paints nothing in
  // a background or headless tab, which is exactly where an agent verifies
  // its own work.
  new ResizeObserver(() => {
    targets.reproject?.();
  }).observe(view);
  // A reveal glides the world layer with a CSS transition, and the transform
  // the page wrote is already the destination. This lands the ring on the
  // target once the glide is actually over.
  world.addEventListener("transitionend", (event) => {
    if (event.propertyName === "transform") targets.reproject?.();
  });

  // ── minimap ─────────────────────────────────────────────────────────
  //
  // The board drawn small, with the pane's own rectangle on it. It is the
  // answer to the state the fit and the reveal can both leave a person in:
  // panned off the content, looking at empty canvas, with no way back that
  // does not involve guessing which direction the board is in.
  //
  // Every number here comes from `camera.rs`. The page owns the pixels and
  // the pointer, and nothing else.

  const MINIMAP_W = 168;
  const MINIMAP_H = 112;
  const MINIMAP_MARGIN = 6;
  /// Below this, a pointer move is a click that wobbled, not a drag. Without
  /// it a click on the minimap reads as a one-pixel drag and the camera lands
  /// a hair off where the finger went down.
  const MINIMAP_JITTER_PX = 2;
  const MINIMAP_STORAGE_KEY = "atlas.minimap-open.v1";
  // Browser local, never CRDT: whether one person keeps a small copy of the
  // board in their corner is not a fact about the document.
  let minimapOpen = readStoredFlag(MINIMAP_STORAGE_KEY, false);

  const minimap = element("div", "minimap");
  minimap.setAttribute("role", "group");
  minimap.setAttribute("aria-label", "Board overview");
  const minimapCanvas = element("div", "minimap-canvas");
  minimapCanvas.style.width = `${MINIMAP_W}px`;
  minimapCanvas.style.height = `${MINIMAP_H}px`;
  const minimapContent = element("div", "minimap-content");
  const minimapMarks = element("div", "minimap-marks");
  const minimapViewport = element("div", "minimap-viewport");
  minimapViewport.setAttribute("aria-hidden", "true");
  const minimapEmpty = element("span", "minimap-empty", "nothing on the board yet");
  minimapCanvas.append(minimapContent, minimapMarks, minimapViewport, minimapEmpty);
  const minimapClose = element("button", "minimap-close", "×");
  minimapClose.type = "button";
  minimapClose.title = "Put the board overview away";
  minimapClose.setAttribute("aria-label", "Put the board overview away");
  minimapClose.addEventListener("pointerdown", (event) => event.stopPropagation());
  minimapClose.addEventListener("click", (event) => {
    event.stopPropagation();
    minimapOpen = false;
    writeStoredFlag(MINIMAP_STORAGE_KEY, false);
    if (dockFocus === "minimap") dockFocus = null;
    applyPanelLayout();
  });
  minimap.append(minimapCanvas, minimapClose);
  panelElements.set("minimap", minimap);

  let minimapView = null;
  let minimapContentBox = null;
  let minimapDrag = null;

  /// The union of everything on the board, in world units.
  function contentBounds() {
    const boxes = [];
    for (const node of atlas.nodes || []) {
      if (!inExplorationView(node.id)) continue;
      const entry = nodeElements.get(node.id);
      boxes.push({
        x: node.x,
        y: node.y,
        w: entry?.root.offsetWidth || node.w || 180,
        h: entry?.root.offsetHeight || node.h || 40,
      });
    }
    for (const item of painting.shapes || []) {
      if (inExplorationView(item.shape.id)) boxes.push(shapeBounds(item.shape, item.ends, item.path));
    }
    const finite = boxes.filter((box) =>
      box && [box.x, box.y, box.w, box.h].every(Number.isFinite));
    if (!finite.length) return null;
    const left = Math.min(...finite.map((box) => box.x));
    const top = Math.min(...finite.map((box) => box.y));
    const right = Math.max(...finite.map((box) => box.x + box.w));
    const bottom = Math.max(...finite.map((box) => box.y + box.h));
    return { x: left, y: top, w: right - left, h: bottom - top };
  }

  function paintMinimap() {
    if (learnerMode || !minimapOpen) return;
    minimapContentBox = contentBounds();
    const pane = paneViewport();
    let view = null;
    if (minimapContentBox && pane.width && pane.height) {
      try {
        const answer = atlasCamera.minimap(
          JSON.stringify(minimapContentBox),
          pane.width,
          pane.height,
          MINIMAP_W,
          MINIMAP_H,
          MINIMAP_MARGIN,
        );
        view = answer ? JSON.parse(answer) : null;
      } catch (error) {
        console.error("atlas minimap failed", error);
      }
    }
    minimapView = view;
    // An empty board renders its frame and says so, rather than removing the
    // control. A minimap that disappears when there is nothing to see is one
    // nobody can find when they have panned off into nothing.
    minimapEmpty.hidden = Boolean(view);
    minimapContent.hidden = !view;
    minimapViewport.hidden = !view;
    if (!view) {
      minimapMarks.replaceChildren();
      return;
    }
    placeMinimapRect(minimapContent, view.content);
    placeMinimapRect(minimapViewport, view.viewport);
    // One dot per card, so the small copy is the board rather than a grey
    // rectangle. Cheap: the set is the document's cards, redrawn on the same
    // events everything else here is.
    const dots = [];
    for (const node of atlas.nodes || []) {
      if (!inExplorationView(node.id)) continue;
      const entry = nodeElements.get(node.id);
      const dot = element("i", "minimap-dot");
      placeMinimapRect(dot, {
        x: (node.x - minimapContentBox.x) * view.scale + view.content.x,
        y: (node.y - minimapContentBox.y) * view.scale + view.content.y,
        w: Math.max(2, (entry?.root.offsetWidth || node.w || 180) * view.scale),
        h: Math.max(2, (entry?.root.offsetHeight || node.h || 40) * view.scale),
      });
      dots.push(dot);
    }
    for (const item of painting.shapes || []) {
      if (!inExplorationView(item.shape.id) || !["rect", "ellipse", "diamond", "segment"].includes(item.shape.form)) continue;
      const bounds = shapeBounds(item.shape, item.ends, item.path);
      const dot = element("i", "minimap-dot");
      placeMinimapRect(dot, {
        x: (bounds.x - minimapContentBox.x) * view.scale + view.content.x,
        y: (bounds.y - minimapContentBox.y) * view.scale + view.content.y,
        w: Math.max(2, bounds.w * view.scale),
        h: Math.max(2, bounds.h * view.scale),
      });
      dots.push(dot);
    }
    minimapMarks.replaceChildren(...dots);
  }

  function placeMinimapRect(el, rect) {
    el.style.left = `${rect.x}px`;
    el.style.top = `${rect.y}px`;
    el.style.width = `${Math.max(1, rect.w)}px`;
    el.style.height = `${Math.max(1, rect.h)}px`;
  }

  function minimapGoTo(event) {
    if (!minimapView || !minimapContentBox) return;
    const box = minimapCanvas.getBoundingClientRect();
    const pane = paneViewport();
    const moved = atlasCamera.center_on_minimap_point(
      JSON.stringify(minimapContentBox),
      JSON.stringify(minimapView),
      event.clientX - box.left,
      event.clientY - box.top,
      pane.width,
      pane.height,
    );
    if (!moved) return;
    lastCameraGesture = performance.now();
    adoptCurrentCamera();
    applyCamera();
    scheduleAttention();
  }

  minimapCanvas.addEventListener("pointerdown", (event) => {
    // The empty board still swallows the pointer. Falling through would start
    // a marquee on the canvas underneath a control the human aimed at.
    event.stopPropagation();
    event.preventDefault();
    if (!minimapView) return;
    minimapDrag = { x: event.clientX, y: event.clientY, moved: false };
    minimapGoTo(event);
    try {
      // Capture keeps a drag alive when the pointer leaves the small canvas,
      // which is most drags. It can refuse (a pointer id the browser does not
      // consider active), and a refusal must not cost the jump the human just
      // asked for, so the move happens first.
      minimapCanvas.setPointerCapture(event.pointerId);
    } catch {
      // Dragging still works while the pointer stays inside.
    }
  });
  minimapCanvas.addEventListener("pointermove", (event) => {
    if (!minimapDrag) return;
    if (!minimapDrag.moved
      && Math.hypot(event.clientX - minimapDrag.x, event.clientY - minimapDrag.y) < MINIMAP_JITTER_PX) {
      return;
    }
    minimapDrag.moved = true;
    minimapGoTo(event);
  });
  for (const name of ["pointerup", "pointercancel", "pointerleave"]) {
    minimapCanvas.addEventListener(name, () => {
      minimapDrag = null;
    });
  }
  // The wheel belongs to the canvas. Zooming the board while the pointer
  // happens to be over its small copy is a gesture nobody asked for, and
  // swallowing it would make the corner a dead zone for the one gesture
  // everybody uses.
  minimapCanvas.addEventListener("wheel", (event) => event.stopPropagation());

  // ── dock ────────────────────────────────────────────────────────────

  /// What the document holds, measured here and decided in the core.
  ///
  /// Rule 5 of the dock rules: a control is gated on data, never on a mode
  /// name. This function is the only place the page states what data exists,
  /// so a panel cannot be shown by one condition and gated by another.
  function documentFacts() {
    const pane = view.getBoundingClientRect();
    return {
      has_explanation: Boolean(atlas.explanation),
      has_agent_ink: Boolean(agentInkRefresh),
      change_count: changeLines.length,
      broken_constraints: (atlas.constraints || [])
        .filter((constraint) => constraint.status === "broken").length,
      pane_is_cramped: !panelFitsBesideTheCanvas(pane),
    };
  }

  /// The flyout the human last reached for. It outranks the registry's z
  /// order for its zone, and it is the only way a human can pull an edge back
  /// from a panel that outranks the one they want. Set by a click, never by
  /// an arriving document.
  let dockFocus = null;

  /// Which flyouts the human has open. Browser-local, never CRDT: rule 7.
  function openPanels() {
    const open = [];
    if (atlas.explanation) open.push("explanation-flow");
    if (changesOpen) open.push("changes");
    if (panelElements.get("agent-ink")?.open) open.push("agent-ink");
    if (minimapOpen) open.push("minimap");
    return open;
  }

  /// Place every panel where the registry says, once per render.
  ///
  /// Learner mode is left alone on purpose. There the explanation is part of
  /// the page's layout rather than chrome floating over a canvas, and docking
  /// it would move a section out of the flow it belongs to.
  function applyPanelLayout() {
    if (learnerMode) return;
    let layout;
    try {
      layout = JSON.parse(panel_layout(
        JSON.stringify(openPanels()),
        JSON.stringify(documentFacts()),
        dockFocus ?? undefined,
      ));
    } catch (error) {
      console.error("atlas panel layout failed", error);
      return;
    }
    let collapsedTheNotice = false;
    for (const placed of layout.panels) {
      const el = panelElements.get(placed.id);
      if (!el) continue;
      const dock = docks.get(placed.zone);
      if (dock && el.parentElement !== dock) dock.append(el);
      if (dock) {
        // Slot order IS the stacking. No arithmetic, no per-panel offset
        // constant that has to be kept in step with a panel's height.
        el.style.order = String(placed.slot);
      }
      el.classList.add("docked");
      armDockExpansion(placed.id, el);
      el.classList.toggle("collapsed", placed.state === "collapsed");
      el.dataset.dockTitle = placed.title;
      // The dock hides a panel whose data is gone and never un-hides one:
      // every panel here already owns its own "is there anything to show"
      // question, and answering it twice is how a panel ends up visible by
      // one rule and empty by another.
      if (placed.state === "absent") el.hidden = true;
      // The notice yields the edge to the walkthrough rather than being moved
      // somewhere else by a stylesheet override. `.atlas-view.has-explanation-flow
      // .atlas-changes` was that override, and it is gone.
      if (placed.id === "changes" && placed.state === "collapsed" && changesOpen) {
        changesOpen = false;
        collapsedTheNotice = true;
      }
    }
    // The bottom strip spans the width, so the three corners along the bottom
    // have to stand on top of it rather than beside it. Measured rather than
    // assumed: the strip is one line for one broken constraint and several for
    // a page full of them, and a constant here would be wrong in both
    // directions. This is the only geometry the dock computes, and it is a
    // read of what the strip actually came out as.
    const strip = docks.get("bottom-strip");
    const stripHeight = strip && strip.childElementCount ? strip.offsetHeight + 10 : 0;
    view.style.setProperty("--dock-strip", `${stripHeight}px`);
    const tools = view.querySelector(".draw-tools");
    const toolsBottom = tools?.offsetHeight ? tools.offsetTop + tools.offsetHeight + 10 : 12;
    view.style.setProperty("--canvas-tools-bottom", `${toolsBottom}px`);
    if (collapsedTheNotice) paintChanges();
  }

  /// The human asking for a panel that something else is holding.
  ///
  /// A collapsed panel is its own title and nothing else, so its own controls
  /// are not reachable while it is collapsed. This is the one gesture that
  /// gets it back, and it is why `dockFocus` exists: without it the zone's z
  /// order would close the panel again on the same frame.
  function expandDockedPanel(id) {
    dockFocus = id;
    if (id === "changes") {
      changesOpen = true;
      paintChanges();
    }
    if (id === "minimap") {
      minimapOpen = true;
      writeStoredFlag(MINIMAP_STORAGE_KEY, true);
      paintMinimap();
    }
    const el = panelElements.get(id);
    if (el && el.tagName === "DETAILS") el.open = true;
    applyPanelLayout();
  }

  /// One listener per panel, on the panel itself, not delegated from the
  /// zone. `.atlas-explanation-flow` stops `click` from propagating (it has
  /// to: a click inside a panel is not a click on the canvas), so a delegated
  /// handler on the zone never hears the one panel most likely to be
  /// collapsed. Two listeners on one element both run; `stopPropagation` only
  /// stops the bubble.
  function armDockExpansion(id, el) {
    if (el.dataset.dockArmed === "yes") return;
    el.dataset.dockArmed = "yes";
    el.addEventListener("click", (event) => {
      if (!el.classList.contains("collapsed")) return;
      event.stopPropagation();
      expandDockedPanel(id);
    });
  }

  // ── camera ──────────────────────────────────────────────────────────

  function applyCamera() {
    syncCamera();
    world.style.transform = atlasCamera.transform();
    // The recompute path the ring never had. `renderAgentAttention` used to
    // be called only when a presence snapshot arrived, so pan, zoom, and
    // resize moved the board out from under a highlight that kept its pixels.
    // Optional: the surface crate gained `reproject` with this change, and a
    // host built before it should degrade to the old frozen ring rather than
    // taking the whole extension down with a TypeError.
    targets.reproject?.();
    paintDetail();
    paintMinimap();
    paintUnboundMarkers();
    paintZoom();
  }

  function animateCamera() {
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
      applyCamera();
      scheduleAttention();
      return;
    }
    world.style.transition = "transform 260ms ease-out";
    applyCamera();
    scheduleAttention();
    setTimeout(() => {
      world.style.transition = "";
    }, 280);
  }

  function worldBounds(id) {
    const node = atlas.nodes.find((candidate) => candidate.id === id);
    if (node) {
      // A container occupies its frame. Returning the hidden card's box would
      // hand every caller a zero-sized rectangle: a constraint drawn around
      // nothing, and a reveal that centres the camera on a point.
      const framed = frameElements.has(id) ? computeFrameBox(id) : null;
      if (framed) {
        return {
          x: framed.left,
          y: framed.top,
          w: framed.right - framed.left,
          h: framed.bottom - framed.top,
        };
      }
      const entry = nodeElements.get(id);
      return {
        x: node.x,
        y: node.y,
        w: entry?.root.offsetWidth || node.w,
        h: entry?.root.offsetHeight || node.h || 40,
      };
    }
    const item = (painting.shapes || []).find((candidate) => candidate.shape.id === id);
    if (item) return shapeBounds(item.shape, item.ends, item.path);
    const constraint = constraintTargetElements.get(id);
    if (!constraint) return null;
    const bounds = constraint.getBBox();
    return { x: bounds.x, y: bounds.y, w: bounds.width, h: bounds.height };
  }

  function constraintBounds(id) {
    const bounds = worldBounds(id);
    if (bounds) return bounds;
    const link = (painting.links || []).find((candidate) => candidate.id === id);
    if (!link) return null;
    const [[ax, ay], [bx, by]] = link.ends;
    return {
      x: Math.min(ax, bx),
      y: Math.min(ay, by),
      w: Math.abs(bx - ax),
      h: Math.abs(by - ay),
    };
  }

  function objectTitle(id) {
    const node = atlas.nodes.find((candidate) => candidate.id === id);
    if (node) return node.label;
    const edge = (atlas.edges || []).find((candidate) => candidate.id === id);
    if (edge) return edge.label || `${edge.from} -> ${edge.to}`;
    const shape = (atlas.shapes || []).find((candidate) => candidate.id === id);
    if (shape) return shape.label || `${shape.form} ${shape.id}`;
    return id;
  }

  /// Whether the human's own hand is on the camera right now.
  function cameraBusy() {
    return handOnPane || performance.now() - lastCameraGesture < CAMERA_BUSY_MS;
  }

  /// Is this target already legible without moving anything?
  ///
  /// The least intrusive answer to "look at this" is often no motion at all.
  /// But being on screen is not the same as being readable: the whole board
  /// zoomed out to fit has every target "in view" at a scale where the labels
  /// are a few pixels tall, and declining to move there would answer "look at
  /// this" by pointing at a speck. So it also has to be close to as legible as
  /// the move would have made it.
  function alreadyLegible(target, box, readable) {
    const margin = 24;
    const left = target.x * camera.scale + camera.x;
    const top = target.y * camera.scale + camera.y;
    const right = left + target.w * camera.scale;
    const bottom = top + target.h * camera.scale;
    const inView = left >= margin
      && top >= margin
      && right <= box.width - margin
      && bottom <= box.height - margin;
    return inView && camera.scale >= readable * 0.75;
  }

  // An arrival is off camera when less than half of its rendered area is in
  // the pane. A sliver at the edge does not count as showing the person what
  // arrived, and a huge group containing the viewport does not count as
  // absent merely because its outer frame extends beyond the pane.
  function mostlyOutsideViewport(target, viewport) {
    if (!target || !viewport.width || !viewport.height || camera.scale <= 0) return false;
    const left = target.x * camera.scale + camera.x;
    const top = target.y * camera.scale + camera.y;
    const right = left + Math.max(1, target.w * camera.scale);
    const bottom = top + Math.max(1, target.h * camera.scale);
    const visibleWidth = Math.max(0, Math.min(right, viewport.width) - Math.max(left, 0));
    const visibleHeight = Math.max(0, Math.min(bottom, viewport.height) - Math.max(top, 0));
    const area = Math.max(1, right - left) * Math.max(1, bottom - top);
    return (visibleWidth * visibleHeight) / area < 0.5;
  }

  /// Bring one semantic target to the middle of the pane by moving the camera.
  ///
  /// This is what the host calls instead of `scrollIntoView` when an agent
  /// reveals a target here. This pane is `overflow: hidden` and never scrolls
  ///, `unscroll()` below actively undoes it, so the DOM's own "scroll this
  /// into view" is a no-op, and a reveal of an off-pane card used to do
  /// nothing at all while still drawing a highlight.
  ///
  /// Returning `false` is not a failure: it declines the move and leaves the
  /// caller to offer the jump at the pane edge, which is the whole of `ask`
  /// and `hold`. `initiator: "human"` means someone clicked that offer, and
  /// no policy stands in the way of a move the human just asked for.
  function revealTarget(id, options) {
    const initiator = options?.initiator === "human" ? "human" : "agent";
    const target = worldBounds(id);
    const box = view.getBoundingClientRect();
    if (!target || !box.width || !box.height) return false;
    const viewport = paneViewport();
    const targetJson = JSON.stringify({ x: target.x, y: target.y, w: target.w, h: target.h });
    // A small target needs a closer view; a very large region needs the
    // opposite. "Reveal" means make the whole target legible, not zoom in at
    // all costs. Both the scale and the pose come from the core.
    const readable = atlasCamera.readable_scale_for(targetJson, JSON.stringify(viewport));
    if (initiator === "agent") {
      if (cameraPolicy === "hold" || cameraPolicy === "ask") return false;
      if (cameraBusy()) return false;
      if (alreadyLegible(target, box, readable)) return true;
    }
    const planned = atlasCamera.plan_focus(targetJson, JSON.stringify(viewport));
    if (!planned) return false;
    if (!cameraReturn) {
      cameraReturn = atlasCamera.state();
      returnViewButton.hidden = false;
    }
    atlasCamera.adopt(planned);
    cameraTouched = true;
    animateCamera();
    return true;
  }

  function returnToHumanCamera() {
    if (!cameraReturn) return;
    atlasCamera.adopt(cameraReturn);
    cameraReturn = null;
    returnViewButton.hidden = true;
    animateCamera();
    report("returned to your view");
  }

  function adoptCurrentCamera() {
    cameraTouched = true;
    cameraReturn = null;
    returnViewButton.hidden = true;
  }

  /// Is everything on the board one connected diagram?
  ///
  /// Union-find over the two relations the model actually has: a link between
  /// two nodes, and a node inside a container. One component and nothing in
  /// the drawn register means the board holds a single authored picture, and
  /// an opening that shows part of it is showing a fragment of one argument
  /// rather than a corner of a large map. Two disconnected clusters, ink, or
  /// a constraint glyph all make that premise false and the cluster opening
  /// keeps its job.
  function contentIsOneConnectedDiagram() {
    const nodes = (atlas.nodes || []).filter(node => inExplorationView(node.id));
    if (nodes.length < 2) return false;
    if ((painting.shapes || []).some(item => inExplorationView(item.shape.id))) return false;
    if ((atlas.constraints || []).length > 0) return false;
    const parent = new Map(nodes.map((node) => [node.id, node.id]));
    const find = (id) => {
      let root = id;
      while (parent.get(root) !== root) root = parent.get(root);
      while (parent.get(id) !== root) {
        const next = parent.get(id);
        parent.set(id, root);
        id = next;
      }
      return root;
    };
    const union = (a, b) => {
      if (!parent.has(a) || !parent.has(b)) return;
      const [ra, rb] = [find(a), find(b)];
      if (ra !== rb) parent.set(ra, rb);
    };
    for (const edge of atlas.edges || []) union(edge.from, edge.to);
    for (const node of nodes) if (node.parent) union(node.id, node.parent);
    const first = find(nodes[0].id);
    return nodes.every((node) => find(node.id) === first);
  }

  /// Fit the whole document into the pane while it is still arriving.
  ///
  /// Not once: a replica receives a scene over several sync messages, and a
  /// fit that latches onto the first of them frames a fraction of the board
  /// and calls it done. So it re-fits whenever the content's extent changes
  /// and the human has not touched the camera yet. Any interaction cancels it
  /// for good, because a fit that fires after the human has panned, zoomed,
  /// or started drawing is the surface taking a camera it was not offered.
  function fitCameraToContent(force = false, animate = false) {
    if (cameraTouched && !force) return;
    atlasPane?.classList.toggle("step-focused", Boolean(openedExploration && !explorationOverview && !explorationEditing));
    const flow = explorationFlows().find(item => item.definition.id === openedExploration);
    const beat = flow?.definition.beats.find(item => item.id === (explorationPreview || flow.state.current_beat));
    const pointTargets = walkthroughTarget && beat && !learnerMode
      ? [walkthroughTarget, ...walkthroughChoices(beat, walkthroughTarget).map(edge => edge.to)]
      : (beat?.actions || []).filter(action => action.kind === "point").map(action => action.target_id);
    const focusTargets = openedExploration && !explorationEditing && !explorationOverview && pointTargets.length
      ? new Set(view.clientWidth < 640 ? pointTargets.slice(0, 1) : pointTargets) : null;
    const boxes = [];
    const readableBoxes = [];
    for (const node of atlas.nodes) {
      if (!inExplorationView(node.id)) continue;
      if (focusTargets && !focusTargets.has(node.id)) continue;
      if (hiddenByCollapsedContainer(node.id)) continue;
      if (isContainer(node.id)) {
        boxes.push({ x: node.x, y: node.y, w: node.w, h: node.h });
        continue;
      }
      // Fitting must not depend on the compact card layout that the resulting
      // zoom selects. Otherwise the camera oscillates across its label threshold.
      const bounds = {
        x: node.x,
        y: node.y,
        w: node.w,
        h: node.h || 40,
      };
      boxes.push(bounds);
      readableBoxes.push(bounds);
    }
    for (const item of painting.shapes || []) {
      if (!inExplorationView(item.shape.id)) continue;
      if (focusTargets && !focusTargets.has(item.shape.id)) continue;
      boxes.push(shapeBounds(item.shape, item.ends, item.path));
    }
    // Each registered glyph is a single geometry envelope. Its SVG bbox
    // includes the padded group hull, sequence rails and numeral chips,
    // attach ticks, void hatch overlay, and label chip.
    for (const glyph of constraintTargetElements.values()) {
      const bounds = glyph.getBBox();
      boxes.push({ x: bounds.x, y: bounds.y, w: bounds.width, h: bounds.height });
    }
    for (const anchor of constraintForeLayer.querySelectorAll(".constraint-mark-anchor")) {
      const bounds = anchor.getBBox();
      boxes.push({ x: bounds.x, y: bounds.y, w: bounds.width, h: bounds.height });
    }
    const viewport = view.getBoundingClientRect();
    // Learner mode allocates the explanation beside or below the canvas in
    // layout. The camera therefore fits the rectangle it actually owns. The
    // author surface keeps its overlay and reserves that space explicitly.
    const lowerChrome = [...view.querySelectorAll('.dock[data-zone^="bottom-"] > :not([hidden]):not(.minimap)')]
      .map(panel => panel.getBoundingClientRect()).filter(box => box.width > 0 && box.height > 0);
    const bottomInset = lowerChrome.length ? Math.max(0, ...lowerChrome.map(box => viewport.bottom - box.top + 12)) : 0;
    const fitViewport = { width: viewport.width, height: Math.max(1, viewport.height - bottomInset) };
    const palette = view.querySelector(".draw-tools");
    const paletteInset = palette && palette.checkVisibility() ? palette.getBoundingClientRect().bottom - viewport.top + 28 : 0;
    const segmentOnly = atlas.nodes.length === 0
      && (painting.shapes || []).length > 0
      && (painting.shapes || []).every((item) => item.shape.form === "segment");
    const maxScale = segmentOnly ? 2 : focusedObject ? 1.5 : openedExploration ? 1.3 : 1;
    // A learner opens an authored visual as one explanatory object. Cluster
    // framing is useful on an authoring board because it keeps labels
    // readable, but on a stacked learner layout it can crop the rest of an
    // infographic out of the visual pane. Always establish the learner's
    // untouched camera from the complete content extent. Explicit evidence
    // cues may highlight a part, but they do not silently replace the whole.
    // `readable_scale` absent means "frame the whole content, cluster rules do
    // not apply": what a forced fit and the learner layout both want. Present,
    // it is the title floor below which the opening frames the densest legible
    // cluster instead. The core decides; this only reports what it measured.
    const options = force || learnerMode || focusedObject || atlas.nodes.length < 80 || contentIsOneConnectedDiagram()
      ? { margin: view.clientWidth < 640 ? 16 : 32, top_inset: Math.max(0, paletteInset), max_scale: maxScale }
      : {
        margin: 40,
        top_inset: Math.max(0, paletteInset),
        max_scale: maxScale,
        readable_scale: NODE_TITLE_READABLE_SCALE,
        cluster: readableBoxes.filter((entry) =>
          [entry.x, entry.y, entry.w, entry.h].every(Number.isFinite)),
        cohesion_floor: contentIsOneConnectedDiagram() ? WHOLE_DIAGRAM_LEGIBLE_SCALE : null,
      };
    // `JSON.stringify` writes `null` for a NaN, and the core refuses to
    // deserialize that rather than framing a board around a hole. A box with
    // no geometry is dropped here, where the DOM measurement that produced it
    // is still in view.
    const finite = boxes.filter((entry) =>
      [entry.x, entry.y, entry.w, entry.h].every(Number.isFinite));
    if (!finite.length) return;
    fitButton.textContent = openedExploration
      ? explorationOverview ? "Focus step" : "Whole diagram" : "Fit";
    fitButton.title = openedExploration
      ? explorationOverview ? "Frame the objects in the current step" : "Show the entire diagram"
      : "Fit the whole board";
    const planned = atlasCamera.plan_opening(
      JSON.stringify(finite),
      JSON.stringify(fitViewport),
      JSON.stringify(options),
    );
    if (!planned) return;
    const fit = JSON.parse(planned);
    fit.extent = `${fit.extent}:viewport=${Math.round(fitViewport.width)}x${Math.round(fitViewport.height)}:exploration=${openedExploration || "blueprint"}`;
    // Re-fitting an extent already fitted would fight the reveal that moved
    // the camera without touching the document.
    if (fit.extent === fittedExtent && !force) return;
    fittedExtent = fit.extent;
    // 0.1, not the wheel's old 0.3 floor: a fit clamped above what fits is a
    // diagram cropped at both edges with nothing saying so.
    atlasCamera.adopt(JSON.stringify(fit.camera));
    if (force) adoptCurrentCamera();
    if (animate) animateCamera();
    else applyCamera();
    scheduleAttention();
  }

  function zoomFromCenter(factor) {
    const viewport = paneViewport();
    if (!viewport.width || !viewport.height) return;
    atlasCamera.zoom_from_center(viewport.width, viewport.height, factor);
    lastCameraGesture = performance.now();
    adoptCurrentCamera();
    applyCamera();
    scheduleAttention();
  }

  // ── camera policy ───────────────────────────────────────────────────

  function storedCameraPolicy() {
    try {
      const stored = localStorage.getItem(CAMERA_POLICY_STORAGE_KEY);
      if (CAMERA_POLICIES.includes(stored)) return stored;
    } catch {
      // A browser with storage denied still gets a working board; it just
      // starts from the default every reload.
    }
    return "follow";
  }

  const CAMERA_POLICY_TEXT = {
    follow: {
      label: "Camera: follows",
      title: "The agent may move the view during a walkthrough. Click to stop it moving without asking.",
      said: "the agent may move your view",
    },
    ask: {
      label: "Camera: asks",
      title: "The agent points at the pane edge instead of moving the view. Click to keep the camera yours.",
      said: "the agent will point, not move your view",
    },
    hold: {
      label: "Camera: held",
      title: "The view only moves when you move it. Click to let the agent guide you again.",
      said: "the view is yours alone",
    },
  };

  function setCameraPolicy(next, announce) {
    if (!CAMERA_POLICIES.includes(next)) return;
    cameraPolicy = next;
    try {
      localStorage.setItem(CAMERA_POLICY_STORAGE_KEY, next);
    } catch {
      // Not persisting a preference is worth less than refusing to set it.
    }
    paintCameraPolicy();
    // The agent reads this in its next look at the board, so it can say "I
    // moved you there" or "click the marker" and be right either way.
    scheduleAttention();
    if (announce) report(CAMERA_POLICY_TEXT[next].said);
  }

  function paintCameraPolicy() {
    const text = CAMERA_POLICY_TEXT[cameraPolicy];
    cameraPolicyButton.textContent = text.label;
    cameraPolicyButton.title = text.title;
    cameraPolicyButton.setAttribute("aria-label", text.title);
    cameraPolicyButton.dataset.policy = cameraPolicy;
  }

  function toWorld(event) {
    const box = view.getBoundingClientRect();
    // `view.scrollLeft` is in here because it is not always zero, which took a
    // while to believe. The pane is `overflow: hidden` and nothing here ever
    // scrolls it, but focusing an element the browser considers out of view
    // scrolls an ancestor anyway, hidden or not, and the inline input for a
    // label near the right-hand edge is exactly that element. The pane then
    // sits a few dozen pixels off its own coordinate system, and every gesture
    // afterwards lands somewhere other than where the pointer was.
    //
    // `unscroll()` below puts it back; this reads it so that even the frame in
    // between is right.
    return JSON.parse(atlasCamera.to_world(
      event.clientX - box.left + view.scrollLeft,
      event.clientY - box.top + view.scrollTop,
    ));
  }

  function worldViewport() {
    const viewport = paneViewport();
    const seen = atlasCamera.world_viewport(viewport.width, viewport.height);
    return seen ? JSON.parse(seen) : null;
  }

  function postAttention() {
    const viewport = worldViewport();
    if (!viewport) return;
    let body;
    try {
      body = attention_payload(
        viewport.x,
        viewport.y,
        viewport.w,
        viewport.h,
        camera.scale,
        attentionDragging,
        cameraPolicy,
        attentionSource(),
      );
    } catch (error) {
      if (attentionErrorShown) return;
      attentionErrorShown = true;
      report(`could not encode viewport attention: ${error.message || error}`);
      return;
    }
    void fetch("/atlas/attention", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", accept: "application/json" },
      body,
    })
      .then(async (response) => {
        if (!response.ok) {
          const payload = await response.json().catch(() => ({}));
          throw new Error(payload.error || `HTTP ${response.status}`);
        }
        attentionErrorShown = false;
      })
      .catch((error) => {
        if (attentionErrorShown) return;
        attentionErrorShown = true;
        report(`could not report viewport attention: ${error.message || error}`);
      });
  }

  function reportRejectedGesture(key, target, reason) {
    const targetText = target ? ` for ${titleForTarget(target)}` : " with no selected card";
    report(`${key} was rejected${targetText}: ${reason}. No mark was attached.`);
    const viewport = worldViewport();
    if (!viewport) return;
    let body;
    try {
      const payload = JSON.parse(attention_payload(
        viewport.x,
        viewport.y,
        viewport.w,
        viewport.h,
        camera.scale,
        attentionDragging,
        cameraPolicy,
        attentionSource(),
      ));
      payload.rejectedGesture = { key, target: target || null, reason };
      body = JSON.stringify(payload);
    } catch (error) {
      report(`could not encode rejected gesture attention: ${error.message || error}`);
      return;
    }
    void fetch("/atlas/attention", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", accept: "application/json" },
      body,
    }).catch((error) => {
      report(`could not report rejected gesture attention: ${error.message || error}`);
    });
  }

  // A selected object always paints the composer chip. It becomes composer
  // attention only when the anchored chat input owns the caret; otherwise the
  // same visible selection is ordinary canvas attention.
  function attentionSource() {
    const anchor = conversation?.ui?.anchor;
    const activeElement = deepActiveElement();
    const composerFocused = activeElement && conversation?.shadowRoot?.contains(activeElement);
    return anchor && !anchor.hidden && composerFocused ? "composer" : "selection";
  }

  function scheduleAttention() {
    clearTimeout(attentionTimer);
    attentionTimer = setTimeout(postAttention, ATTENTION_SETTLE_MS);
  }

  function startDragAttention(id) {
    attentionDragging = id;
    clearInterval(attentionDragTimer);
    postAttention();
    attentionDragTimer = setInterval(postAttention, DRAG_ATTENTION_KEEPALIVE_MS);
  }

  function endDragAttention() {
    clearInterval(attentionDragTimer);
    attentionDragTimer = null;
    attentionDragging = null;
    postAttention();
  }

  // The camera is the only thing that moves this surface. A scrolled pane is
  // always something the browser did on its own, and it desynchronises the
  // page from the document's coordinates, so it is undone as soon as it
  // happens rather than compensated for forever.
  function unscroll() {
    if (view.scrollLeft !== 0) view.scrollLeft = 0;
    if (view.scrollTop !== 0) view.scrollTop = 0;
    scheduleAttention();
  }
  view.addEventListener("scroll", unscroll, { passive: true });

  // Any gesture on the pane, a pan, a drag, the first stroke of a drawing,
  // means the human has adopted this camera position; the fit must never fire
  // after that. Capture phase so retargeting cannot hide it. It also opens the
  // window in which a reveal yields rather than moving the view under a hand
  // that is already busy with it.
  view.addEventListener(
    "pointerdown",
    (event) => {
      if (event.target.closest?.(".camera-return, .camera-policy, .constraint-salience, .zoom-controls, .minimap, .gesture-card, .atlas-changes, .source-popover, .diag-strip, .atlas-context-menu, .agent-ink-panel")) return;
      handOnPane = true;
      // The canvas takes focus when you touch it.
      //
      // Without this, every single-key shortcut on this surface, the tools and
      // the three marks, was unreachable until the view happened to get focus
      // some other way, because the keydown handler requires the view itself
      // to be the target. Clicking the canvas and pressing H did nothing at
      // all, which reads as "the shortcut is a lie" rather than as a focus
      // bug. Inline editors and panels are excluded above, so this cannot
      // take focus away from something being typed into.
      if (event.target === view || view.contains(event.target)) {
        view.focus({ preventScroll: true });
      }
      lastCameraGesture = performance.now();
      // Before any drag delta is applied. Without it a drag that lands during
      // a reveal starts from the pose the animation was aiming at rather than
      // the one on screen, and the camera fights the hand.
      atlasCamera.cancel_and_resync();
      adoptCurrentCamera();
    },
    { capture: true, passive: true },
  );

  for (const name of ["pointerup", "pointercancel"]) {
    view.addEventListener(
      name,
      (event) => {
        handOnPane = false;
        // Same exclusion as the press: releasing the button that just turned
        // following back on must not count as a gesture on the camera, or the
        // next reveal declines for a second and looks like the switch failed.
        if (event.target.closest?.(".camera-return, .camera-policy, .constraint-salience, .zoom-controls, .minimap, .gesture-card, .atlas-changes, .source-popover, .diag-strip, .atlas-context-menu, .agent-ink-panel")) return;
        lastCameraGesture = performance.now();
      },
      { capture: true, passive: true },
    );
  }

  view.addEventListener("wheel", (event) => {
    if (event.target.closest?.(".draw-tools, .segment-picker, .segment-flap-panel, .segment-tree-panel")) return;
    event.preventDefault();
    lastCameraGesture = performance.now();
    adoptCurrentCamera();
    const box = view.getBoundingClientRect();
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? box.height : 1;
    if (event.deltaY === 0 && event.deltaX === 0) return;
    if (event.ctrlKey || event.metaKey) {
      atlasCamera.zoom_at(event.clientX - box.left, event.clientY - box.top, Math.exp(-event.deltaY * unit * .01));
    } else {
      atlasCamera.pan(-event.deltaX * unit, -event.deltaY * unit);
    }
    applyCamera();
    scheduleAttention();
  }, { passive: false });

  // ── tools ───────────────────────────────────────────────────────────

  const toolbar = element("div", "draw-tools");
  toolbar.setAttribute("role", "toolbar");
  toolbar.setAttribute("aria-label", "Drawing tools");
  const toolButtons = new Map();
  const drawingGroup = element("div", "draw-tool-group");
  toolbar.append(drawingGroup);
  const toolHint = element("p", "canvas-tool-hint");
  toolHint.setAttribute("aria-live", "polite");
  for (const key of TOOL_ORDER) {
    const spec = TOOLS.find(item => item.key === key);
    const button = element("button", "tool");
    button.dataset.tool = spec.key;
    const icon = document.createElementNS(SVG_NS, "svg");
    icon.setAttribute("viewBox", "0 0 24 24"); icon.setAttribute("aria-hidden", "true");
    const path = document.createElementNS(SVG_NS, "path"); path.setAttribute("d", TOOL_ICONS[spec.key]); icon.append(path);
    button.append(icon);
    const digit = Object.keys(TOOL_DIGITS).find(value => TOOL_DIGITS[value] === spec.key);
    if (digit) button.append(element("kbd", "tool-shortcut", digit));
    button.type = "button";
    button.title = TOOL_SHORTCUTS.has(spec.key)
      ? `${spec.title}  (${spec.key.toUpperCase()})`
      : spec.title;
    button.setAttribute("aria-label", spec.title);
    button.addEventListener("click", () => {
      setTool(spec);
      view.focus();
    });
    toolButtons.set(spec, button);
    drawingGroup.append(button);
  }
  const altitudeControls = element("div", "altitude-levels");
  altitudeControls.setAttribute("role", "group");
  altitudeControls.setAttribute("aria-label", "Visible containment depth");
  altitudeControls.title = "Visible detail: [ steps out, ] steps in. This does not change the shared question.";
  const altitudeButtons = new Map();
  const inkOptions = element("details", "ink-options");
  const inkOptionsSummary = element("summary", "ink-options-summary");
  inkOptionsSummary.setAttribute("aria-label", "Choose ink colour");
  const inkPreview = element("span", "ink-preview");
  inkPreview.setAttribute("aria-hidden", "true");
  inkOptionsSummary.append(inkPreview, element("span", null, "Ink"));
  const swatches = element("div", "tool-inks");
  for (const name of Object.keys(INK_CSS)) {
    const swatch = element("button", "swatch");
    swatch.type = "button";
    swatch.title = name;
    swatch.setAttribute("aria-label", `${name} ink`);
    swatch.style.background = INK_CSS[name];
    swatch.addEventListener("click", () => {
      inkColor = name;
      paintTools();
      inkOptions.open = false;
      view.focus();
    });
    swatches.append(swatch);
    swatch.dataset.ink = name;
  }
  // Width and dash live under the same disclosure as colour: they are the
  // three things a stroke is, and a selected shape takes a click on any of
  // them the way it would in Excalidraw.
  const strokeRow = element("div", "tool-strokes");
  strokeRow.setAttribute("role", "group");
  strokeRow.setAttribute("aria-label", "Stroke width and style");
  const strokeButtons = [];
  for (const spec of STROKE_WIDTHS) {
    const button = element("button", "stroke-pick", spec.glyph);
    button.type = "button";
    button.title = `${spec.title} stroke`;
    button.dataset.width = String(spec.value);
    button.addEventListener("click", () => {
      strokeWidth = spec.value;
      if (selected && paintedShape(selected)) placeShape({ id: selected, stroke_width: spec.value });
      paintTools();
      view.focus();
    });
    strokeButtons.push(button);
    strokeRow.append(button);
  }
  const styleRow = element("div", "tool-strokes");
  styleRow.setAttribute("role", "group");
  styleRow.setAttribute("aria-label", "Stroke style");
  const styleButtons = [];
  for (const spec of STROKE_STYLES) {
    const button = element("button", "stroke-pick", spec.glyph);
    button.type = "button";
    button.title = spec.title;
    button.dataset.style = spec.value;
    button.addEventListener("click", () => {
      strokeStyle = spec.value;
      if (selected && paintedShape(selected)) placeShape({ id: selected, stroke_style: spec.value });
      paintTools();
      view.focus();
    });
    styleButtons.push(button);
    styleRow.append(button);
  }
  swatches.append(strokeRow, styleRow);
  inkOptions.append(inkOptionsSummary, swatches);
  toolbar.append(inkOptions);
  returnViewButton.addEventListener("click", () => {
    returnToHumanCamera();
    view.focus();
  });
  toolbar.append(returnViewButton);

  // Camera ownership and constraint depth matter, but not on every gesture.
  // Keep the drawing vocabulary immediate and move these view preferences
  // behind one deliberate disclosure so the toolbar does not become a second
  // panel stretched across the picture.
  const viewOptions = element("details", "view-options");
  const viewOptionsSummary = element("summary", "view-options-summary", "View");
  const viewOptionsPanel = element("div", "view-options-panel");
  cameraPolicyButton.addEventListener("click", () => {
    const next = CAMERA_POLICIES[(CAMERA_POLICIES.indexOf(cameraPolicy) + 1) % CAMERA_POLICIES.length];
    setCameraPolicy(next, true);
    view.focus();
  });
  paintCameraPolicy();
  constraintSalienceButton.addEventListener("click", () => {
    toggleConstraintSalience();
    view.focus();
  });
  viewOptionsPanel.append(element("span", "view-options-label", "Containment depth"), altitudeControls, cameraPolicyButton, constraintSalienceButton);
  const frameTool = element("button", "compact-frame-tool", "Draw a frame (F)"); frameTool.type = "button";
  frameTool.addEventListener("click", () => { setTool(TOOLS.find(item => item.key === "f")); viewOptions.open = false; view.focus(); });
  viewOptionsPanel.append(frameTool);
  viewOptions.append(viewOptionsSummary, viewOptionsPanel);
  toolbar.append(viewOptions);
  paintConstraintSalience();
  const zoomControls = element("div", "zoom-controls");
  zoomControls.setAttribute("role", "group");
  zoomControls.setAttribute("aria-label", "Canvas zoom");
  const zoomOut = element("button", "zoom-button", "−");
  zoomOut.type = "button";
  zoomOut.title = "Zoom out";
  zoomOut.setAttribute("aria-label", "Zoom out");
  const zoomPercent = element("output", "zoom-percent", "100%");
  zoomPercent.setAttribute("aria-live", "polite");
  const zoomIn = element("button", "zoom-button", "+");
  zoomIn.type = "button";
  zoomIn.title = "Zoom in";
  zoomIn.setAttribute("aria-label", "Zoom in");
  const fitButton = element("button", "zoom-button zoom-fit", "Fit");
  fitButton.type = "button";
  fitButton.title = "Fit the whole board";
  zoomOut.addEventListener("click", () => zoomFromCenter(1 / 1.2));
  zoomIn.addEventListener("click", () => zoomFromCenter(1.2));
  fitButton.addEventListener("click", () => {
    if (openedExploration) explorationOverview = !explorationOverview;
    saveLocalView();
    renderExplanation();
    fitCameraToContent(true);
  });
  zoomControls.append(zoomOut, zoomPercent, zoomIn, fitButton);

  const toolToast = element("div", "tool-toast");
  toolToast.setAttribute("role", "status");
  toolToast.setAttribute("aria-live", "polite");
  toolToast.hidden = true;

  view.append(toolbar, toolHint);
  const paletteObserver = new ResizeObserver(() => {
    const bounds = toolbar.getBoundingClientRect();
    const viewport = view.getBoundingClientRect();
    toolHint.style.top = `${bounds.bottom - viewport.top + 8}px`;
  });
  paletteObserver.observe(toolbar);
  panelElements.set("zoom-controls", zoomControls);
  panelElements.set("tool-toast", toolToast);


  segmentPicker = createSegmentPicker({
    view,
    toolbar,
    worker: segmentWorker,
    maxDepth: atlasWasm.segment_max_depth(),
    initialSourceId: new URLSearchParams(globalThis.location.search).get("segment-recipe")
      || undefined,
    placement(result, parentId) {
      const parent = parentId
        ? atlas.shapes.find((shape) => shape.id === parentId && shape.form === "segment")
        : null;
      if (parent && Array.isArray(parent.segment_mask_source_box)
          && parent.segment_mask_source_box.length === 4) {
        const [parentLeft, parentTop, parentRight] = parent.segment_mask_source_box;
        const [left, top, right] = result.mask_source_box;
        const scale = parent.w / (parentRight - parentLeft);
        return {
          x: parent.x + (left - parentLeft) * scale,
          y: parent.y + (top - parentTop) * scale,
          w: Math.max(8, (right - left) * scale),
        };
      }
      const bounds = view.getBoundingClientRect();
      const center = toWorld({
        clientX: bounds.left + bounds.width / 2,
        clientY: bounds.top + bounds.height / 2,
      });
      const sourceWidth = result.mask_source_box[2] - result.mask_source_box[0];
      const sourceHeight = result.mask_source_box[3] - result.mask_source_box[1];
      const width = Math.max(64, Math.min(
        560,
        sourceWidth,
        bounds.width * 0.62,
        bounds.height * 0.62 * (sourceWidth / sourceHeight),
      ));
      const height = width * sourceHeight / sourceWidth;
      return { x: center.x - width / 2, y: center.y - height / 2, w: width };
    },
    accept({ proposal, materialization, points }) {
      if (points.some((point) => !point.trusted)) {
        throw new Error("Object selection must come from your pointer.");
      }
      const id = write(() => doc.accept_segment(
        JSON.stringify(proposal),
        JSON.stringify(materialization),
      ));
      if (!id) throw new Error("Atlas refused the selected mask.");
      select(id, false);
      requestAnimationFrame(() => select(id));
      return id;
    },
    finish(parentId) {
      select(parentId, false);
      requestAnimationFrame(() => select(parentId));
    },
    report,
  });

  // ── what changed while the human was not looking ─────────────────────
  //
  // The agent has been given a "CHANGED SINCE YOUR LAST READ" section since
  // the change section existed. The human had no equivalent on this canvas:
  // an agent could add a dozen cards, restate a claim, or withdraw a link,
  // and the only signal was that the picture was different than remembered.
  // Same comparison, same sentences, computed by the same Rust the agent's
  // copy comes from. A second implementation here would drift from it.
  //
  // `human` is passed as the reader so the human is not shown their own
  // edits as news, exactly the courtesy the agent gets.
  const changePanel = element("div", "atlas-changes");
  changePanel.hidden = true;
  changePanel.setAttribute("aria-live", "polite");
  const changeSummary = element("button", "atlas-changes-summary");
  changeSummary.type = "button";
  const changeList = element("div", "atlas-changes-list");
  const changeSeen = element("button", "quiet-button atlas-changes-seen", "I've looked");
  changeSeen.type = "button";
  changePanel.append(changeSummary, changeList, changeSeen);
  panelElements.set("changes", changePanel);

  let seenDigest = null;
  let changeLines = [];
  let changesOpen = false;
  let baselineTimer = null;

  // The baseline is what the human has already seen, and on a cold load that
  // is not knowable until the replica has finished arriving. The CRDT
  // handshake lands over several frames, so seeding on the first render
  // captured a half-empty document and then announced the rest of the page as
  // news. Settling first is the difference between "nothing changed" and "25
  // changes" on a page nobody has touched.
  function scheduleBaseline() {
    if (baselineTimer) clearTimeout(baselineTimer);
    baselineTimer = setTimeout(() => {
      baselineTimer = null;
      markChangesSeen();
    }, 700);
  }

  function markChangesSeen() {
    try {
      seenDigest = doc.digest();
    } catch (error) {
      console.error("atlas digest failed", error);
      seenDigest = null;
    }
    changeLines = [];
    changesOpen = false;
    paintChanges();
  }

  // A change line carries the object id in brackets. Offering the jump is
  // the difference between being told something moved and being able to go
  // look at it.
  function idInLine(line) {
    const found = /\[([0-9a-f]{16}-[0-9a-f]{8})\]/.exec(line);
    return found ? found[1] : null;
  }

  function refreshChanges() {
    if (!seenDigest) return;
    let lines = [];
    try {
      lines = JSON.parse(doc.changes_since(seenDigest, "human"));
    } catch (error) {
      console.error("atlas changes_since failed", error);
      return;
    }
    const same =
      lines.length === changeLines.length &&
      lines.every((line, at) => line === changeLines[at]);
    if (same) return;
    changeLines = lines;
    const viewport = view.getBoundingClientRect();
    const offscreenArrivalIds = changeLines
      .map(idInLine)
      .filter((id) => id && mostlyOutsideViewport(worldBounds(id), viewport));
    if (offscreenArrivalIds.length && panelFitsBesideTheCanvas(viewport)) changesOpen = true;
    changePanel.classList.toggle("has-offscreen-arrival", offscreenArrivalIds.length > 0);
    changePanel.classList.toggle("cramped", !panelFitsBesideTheCanvas(viewport));
    paintChanges();
  }

  /// Would an expanded panel still leave the human a canvas to read?
  ///
  /// The auto-expand above and the opening fit are causally linked: the fit
  /// is what leaves content off camera, and off-camera content is what opens
  /// the panel, so the panel opened itself exactly when the view was worst.
  /// Measured on the real pane: 320 CSS pixels of panel over about 683 of
  /// canvas, anchored right, covering roughly half the width and most of the
  /// height of the picture it was announcing. On a pane that narrow the
  /// announcement stays a one-line badge and the human opens it when they
  /// want it. `PANEL_SHARE` is the most of the pane an announcement may take.
  function panelFitsBesideTheCanvas(viewport) {
    const pane = viewport || view.getBoundingClientRect();
    if (!pane.width) return true;
    return Math.min(PANEL_WIDTH_PX, pane.width - 28) <= pane.width * PANEL_SHARE;
  }

  function paintChanges() {
    if (!changeLines.length) {
      changePanel.hidden = true;
      return;
    }
    changePanel.hidden = false;
    changeSummary.textContent = `${changeLines.length} change${
      changeLines.length === 1 ? "" : "s"
    } since you looked`;
    changeSummary.setAttribute("aria-expanded", String(changesOpen));
    changeList.hidden = !changesOpen;
    changeSeen.hidden = !changesOpen;
    if (!changesOpen) return;
    changeList.replaceChildren();
    for (const line of changeLines) {
      // The id is addressing, not prose. It is stripped from what the human
      // reads and kept as the jump target.
      const id = idInLine(line);
      const text = line.replace(/^-\s*/, "").replace(/\[[0-9a-f-]+\]\s*/g, "");
      const row = element(id ? "button" : "div", "atlas-change");
      if (id) {
        row.type = "button";
        row.title = "Go to it";
        row.addEventListener("click", () => {
          revealTarget(id, { initiator: "human" });
          select(id);
        });
      }
      row.textContent = text;
      changeList.append(row);
    }
  }

  changeSummary.addEventListener("click", () => {
    changesOpen = !changesOpen;
    // Opening it is the human reaching for this edge; closing it hands the
    // edge back to whatever the registry would have given it to.
    dockFocus = changesOpen ? "changes" : null;
    paintChanges();
    applyPanelLayout();
  });
  changeSeen.addEventListener("click", () => {
    markChangesSeen();
    view.focus();
  });

  // ── agent ink: numeric variables + step timeline ──────────────────
  //
  // Off by default. Whether this document carries an agent-ink layer at all
  // is the host's call (`GET /atlas/agent-ink`), not something the browser
  // replica can know from its own CRDT state alone: an empty `variables` list
  // means "nothing authored yet", not "the feature is off". Until that flag
  // comes back `enabled: true`, this builds no DOM, adds no listener, and
  // costs nothing per render. The page looks exactly as it does without it.
  fetch("/atlas/agent-ink", { headers: { accept: "application/json" } })
    .then((response) => response.json())
    .then((payload) => {
      if (!payload || !payload.enabled) return;
      agentInkRefresh = setUpAgentInkPanel();
      agentInkRefresh();
    })
    .catch((error) => {
      // No route, the flag is off, or the network is down: stay exactly as
      // if this section did not exist. Showing a scrubber over a document
      // the host never said carries agent ink would be a second, invented
      // account of the same page.
      console.error("agent-ink feature check failed", error);
    });

  /// Build the variables panel and step scrubber, and return the function
  /// `render()` should call on every redraw to keep them current.
  ///
  /// Called at most once, only after the host has confirmed the feature is
  /// on. `doc.agent_ink_variables()` and `doc.agent_ink_step()` are read
  /// straight from the shared replica, the same numbers the read-back
  /// dialog's `doc.describe()` puts into its VARIABLES section, so nothing
  /// here re-derives a number Rust already computed.
  function setUpAgentInkPanel() {
    const panel = element("details", "agent-ink-panel");
    const panelSummary = element("summary", "agent-ink-summary");
    const panelTitle = element("span", "agent-ink-title", "Learning controls");
    const panelMeta = element("span", "agent-ink-meta", "");
    panelSummary.append(panelTitle, panelMeta);
    const panelBody = element("div", "agent-ink-body");
    const inputList = element("div", "agent-ink-list agent-ink-inputs");
    const computed = element("details", "agent-ink-computed");
    const computedSummary = element("summary", "agent-ink-computed-summary", "Computed values");
    const computedList = element("div", "agent-ink-list");
    computed.append(computedSummary, computedList);
    const scrubRow = element("div", "agent-ink-scrub");
    scrubRow.hidden = true;
    const scrubLabel = element("span", "agent-ink-scrub-label");
    const scrub = element("input", "agent-ink-scrub-input");
    scrub.type = "range";
    scrub.min = "0";
    scrub.step = "1";
    scrub.setAttribute("aria-label", "Step through the recorded timeline");
    scrubRow.append(scrubLabel, scrub);

    // Promotion is the one agent-ink action that is not a direct CRDT
    // write: it is `.human_only()` on the host (see `atlas.rs`'s
    // `actions()`), so it travels as a real action call through
    // `ctx.action`, the same door `board.rs`'s human controls use, rather
    // than through `doc.agent_ink_*`. Hidden by default: shown only once a
    // render finds this document in learning mode with a recorded step, so
    // the control never offers a call that can only ever refuse.
    const promoteRow = element("div", "agent-ink-promote");
    promoteRow.hidden = true;
    const promoteButton = element("button", "agent-ink-promote-button", "Promote to alignment");
    promoteButton.type = "button";
    promoteRow.append(promoteButton);
    panelBody.append(inputList, scrubRow, computed, promoteRow);
    panel.append(panelSummary, panelBody);
    panelElements.set("agent-ink", panel);
    // A `details` opening or closing is a flyout being toggled, which the
    // registry has to hear about or the zone strip below it does not move.
    panel.addEventListener("toggle", () => {
      if (panel.open) dockFocus = "agent-ink";
      else if (dockFocus === "agent-ink") dockFocus = null;
      applyPanelLayout();
    });
    applyPanelLayout();

    promoteButton.addEventListener("click", async () => {
      promoteButton.disabled = true;
      try {
        const result = await ctx.action("atlas_mode_promote_to_alignment", {});
        report(result && result.result ? result.result : "Promoted to alignment mode.");
      } catch (error) {
        // A refused promotion (a step that will not re-solve, or one already
        // called on an alignment document) lands here with the real reason
        // `promote_to_alignment` gave. It is surfaced through the same toast
        // every other write failure in this file uses, not swallowed.
        report(error instanceof Error ? error.message : String(error));
      } finally {
        promoteButton.disabled = false;
        renderAgentInk();
      }
    });

    // Scrubbing restores a stored step byte for byte; it must never invoke
    // the solver, which is why this calls `agent_ink_scrub` and not a plain
    // variable write.
    scrub.addEventListener("input", () => {
      write(() => doc.agent_ink_scrub(Number(scrub.value)));
    });

    const rows = new Map(); // variable name -> {row, valueEl, input}

    function formatLivingValue(variable) {
      const value = Number(variable.value);
      if (!Number.isFinite(value)) return String(variable.value);
      const formatted = value.toLocaleString(undefined, {
        maximumFractionDigits: Number.isInteger(value) ? 0 : 2,
        useGrouping: Math.abs(value) >= 1000,
      });
      return variable.unit ? `${formatted} ${variable.unit}` : formatted;
    }

    function paintVariable(variable, stepVariable) {
      let entry = rows.get(variable.name);
      if (!entry) {
        const row = element("div", "agent-ink-row");
        const name = element("span", "agent-ink-name", variable.name);
        const value = element("span", "agent-ink-value");
        const state = element("span", "agent-ink-state");
        row.append(name, value, state);
        entry = { row, valueEl: value, stateEl: state, input: null };
        rows.set(variable.name, entry);
      }
      const target = variable.state === "pinned" ? inputList : computedList;
      if (entry.row.parentElement !== target) target.append(entry.row);
      entry.row.hidden = variable.name === stepVariable;
      entry.stateEl.textContent = variable.state;
      entry.stateEl.className = `agent-ink-state agent-ink-state-${variable.state}`;
      const shown = formatLivingValue(variable);
      // Pinned is the one state the human may write directly: scrubbing is
      // driven by the scrubber above, and free/derived are solver-owned
      // (`agent_ink_set_variable` refuses those, so offering the control
      // would only ever report an error).
      if (variable.state === "pinned") {
        if (!entry.input) {
          const input = element("input", "agent-ink-value-input");
          input.type = "number";
          const commit = () => {
            const next = Number(input.value);
            if (Number.isFinite(next)) write(() => doc.agent_ink_set_variable(variable.name, next));
          };
          input.addEventListener("change", commit);
          input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") input.blur();
          });
          entry.valueEl.replaceWith(input);
          entry.valueEl = null;
          entry.input = input;
        }
        // Never overwrite what the human is mid-edit typing; the next
        // `change` commits it and the panel picks the committed value back
        // up from the document like everything else here does.
        if (document.activeElement !== entry.input) entry.input.value = String(variable.value);
      } else if (entry.input) {
        const value = element("span", "agent-ink-value", shown);
        entry.input.replaceWith(value);
        entry.input = null;
        entry.valueEl = value;
      } else {
        entry.valueEl.textContent = shown;
        entry.valueEl.title = String(variable.value);
      }
    }

    function renderAgentInk() {
      let variables;
      let step;
      let mode;
      try {
        variables = JSON.parse(doc.agent_ink_variables());
        step = JSON.parse(doc.agent_ink_step());
        mode = JSON.parse(doc.agent_ink_mode());
      } catch (error) {
        console.error("agent-ink read failed", error);
        return;
      }
      const live = new Set(variables.map((variable) => variable.name));
      for (const [name, entry] of rows) {
        if (live.has(name)) continue;
        entry.row.remove();
        rows.delete(name);
      }
      for (const variable of variables) paintVariable(variable, step.index_variable);

      const visibleInputs = variables.filter((variable) => variable.state === "pinned").length;
      const visibleComputed = variables.filter((variable) => (
        variable.state !== "pinned" && variable.name !== step.index_variable
      )).length;
      panelMeta.textContent = `${visibleInputs + (step.count > 1 ? 1 : 0)} controls`;
      // With nothing to learn with, the panel is chrome sitting over the
      // top-left of the drawing, which is exactly where Fit puts the
      // heading. It comes back the moment a variable or a step exists.
      panel.hidden = variables.length === 0 && !(step.count > 1);
      computedSummary.textContent = `Computed values (${visibleComputed})`;
      computed.hidden = visibleComputed === 0;

      if (step.index_variable && step.count > 1) {
        scrubRow.hidden = false;
        scrub.max = String(step.count - 1);
        if (document.activeElement !== scrub) scrub.value = String(step.index ?? 0);
        scrubLabel.textContent = `${step.index_variable}: ${step.index ?? "?"} / ${step.count - 1}`;
      } else {
        scrubRow.hidden = true;
      }

      // `promote_to_alignment` itself refuses a document with no recorded
      // step yet, or one already in alignment mode. Mirror both refusals
      // here so the button only ever appears where it can actually succeed.
      promoteRow.hidden = mode.mode !== "learning" || step.count < 1;
    }

    return renderAgentInk;
  }

  showGestureCard();
  const maskLauncher = toolbar.querySelector(".segment-picker-launcher");
  if (maskLauncher) viewOptionsPanel.append(maskLauncher);
  paintTools(); // the select tool is armed from the start; say so
  // The runtime builds its presence tray on `document.body`, fixed to the
  // window rather than to this pane, which is how it came to sit on top of
  // the broken-constraint strip. It is chrome over this canvas, so this
  // canvas's dock places it.
  const presenceTray = document.querySelector(".agui-semantic-presence");
  if (presenceTray) panelElements.set("presence", presenceTray);
  applyPanelLayout(); // put the chrome in its zones before the first snapshot

  let toolToastTimer = null;
  function setTool(spec, announce = false) {
    active = spec;
    paintTools();
    renderHandles(); // grips are select-tool chrome; picking up the pen puts them away
    if (announce) showToolToast(spec);
  }

  function showToolToast(spec) {
    clearTimeout(toolToastTimer);
    const name = spec.title.split(":")[0].split(",")[0];
    toolToast.textContent = `${name} tool. Escape returns to Select.`;
    toolToast.hidden = false;
    toolToastTimer = setTimeout(() => {
      toolToast.hidden = true;
    }, 1800);
  }

  /// How much of a card is worth rendering at this zoom.
  ///
  /// The finding: at a whole-board fit every card rendered its title, its
  /// body, and its source at a few pixels a line, so thirty cards became the
  /// picture and the sketch they were annotating became noise behind them.
  /// The level is the core's answer and the stylesheet applies it; a selected
  /// card opts out, because being shown three words of the card you just
  /// clicked is the surface arguing with you.
  function paintDetail() {
    const detail = label_detail(camera.scale, false);
    view.dataset.detail = detail;
    for (const [id, entry] of nodeElements) {
      // Keep measured geometry while expressing actual disclosure state to
      // keyboard and assistive technology. Selecting restores all details.
      const collapsed = detail === "title" && !isSelected(id);
      for (const part of [entry.note, entry.head, entry.claims]) {
        part.inert = collapsed;
        if (collapsed) part.setAttribute("aria-hidden", "true");
        else part.removeAttribute("aria-hidden");
      }
    }
  }

  function paintZoom() {
    if (zoomPercent) zoomPercent.textContent = `${Math.round(camera.scale * 100)}%`;
  }

  function showGestureCard() {
    if (!learnerMode && (atlas.nodes.length || atlas.shapes.length)) return;
    try {
      if (localStorage.getItem(GESTURE_CARD_STORAGE_KEY) === "dismissed") return;
    } catch {
      // Storage can be denied. The card still dismisses for this page.
    }
    const card = element("section", "gesture-card");
    card.setAttribute("aria-label", "Canvas gestures");
    const close = element("button", "gesture-card-close", "×");
    close.type = "button";
    close.setAttribute("aria-label", "Dismiss canvas gestures");
    const title = element("strong", "gesture-card-title", "Canvas basics");
    const list = element(
      "p",
      "gesture-card-list",
      "Drag the background to pan. Shift-drag to select several objects. Scroll or pinch to zoom. Hold Space to pan with any tool.",
    );
    close.addEventListener("click", () => {
      card.remove();
      try {
        localStorage.setItem(GESTURE_CARD_STORAGE_KEY, "dismissed");
      } catch {
        // Dismissal still applies to this page when storage is denied.
      }
      view.focus();
    });
    card.append(close, title, list);
    panelElements.set("gesture-card", card);
    applyPanelLayout();
  }

  function paintTools() {
    for (const [spec, button] of toolButtons) {
      button.classList.toggle("active", spec === active);
      button.setAttribute("aria-pressed", String(spec === active));
    }
    for (const swatch of swatches.querySelectorAll(".swatch")) {
      swatch.classList.toggle("active", swatch.dataset.ink === inkColor);
    }
    for (const button of strokeButtons) {
      button.classList.toggle("active", Number(button.dataset.width) === strokeWidth);
    }
    for (const button of styleButtons) {
      button.classList.toggle("active", button.dataset.style === strokeStyle);
    }
    inkPreview.style.background = INK_CSS[inkColor];
    const hints = {
      v: "Drag to select. Double-click a card to explore it. Hold Space or the mouse wheel to pan.",
      h: "Drag anywhere to pan. Pinch or Ctrl + scroll to zoom. Press V to select.",
      r: "Drag to draw a rectangle, then type a label. Escape returns to Select.",
      d: "Drag to draw a decision. Type to label it. Escape returns to Select.",
      o: "Drag to draw an ellipse. Escape returns to Select. Type to label it.",
      a: "Drag between objects to connect them. The arrow stays attached when objects move.",
      l: "Drag to draw a line. Escape returns to Select.",
      p: "Draw around something to group it, or through it to cross it out. Escape returns to Select.",
      t: "Click anywhere and start typing. Escape finishes editing.",
      f: "Drag a frame around a region, then give it a name.",
    };
    toolHint.textContent = hints[active.key] || active.title;
    // The cursor is the honest signal for which tool is armed; without it the
    // first thing a draw tool does is look like a broken drag.
    view.classList.toggle("drawing", active.tool === "draw");
    view.classList.toggle("hand-armed", active.tool === "hand");
  }

  function paintConstraintSalience() {
    constraintSalienceButton.textContent = backConstraintsForward
      ? "Constraints: front"
      : "Constraints: depth";
    constraintSalienceButton.title = backConstraintsForward
      ? "Return back-salience constraints to depth (Alt+B)"
      : "Bring back-salience constraints forward (Alt+B)";
    constraintSalienceButton.classList.toggle("active", backConstraintsForward);
    constraintSalienceButton.setAttribute("aria-pressed", String(backConstraintsForward));
  }

  function toggleConstraintSalience() {
    backConstraintsForward = !backConstraintsForward;
    paintConstraintSalience();
    // Repaint only derived glyphs. This preference must never enter the CRDT
    // or trigger document measurements and outbound frames.
    renderConstraintView(groupMarksByTarget());
  }

  // ── rendering ───────────────────────────────────────────────────────

  function indexStateMachinePaint() {
    statePaintByMachine = new Map(
      stateMachinePaint.map((projection) => [projection.machine_id, projection]),
    );
    currentStateIds = new Set(
      stateMachinePaint.flatMap((projection) => projection.state_id ? [projection.state_id] : []),
    );
    takenTransitionIds = new Set(
      stateMachinePaint.flatMap(
        (projection) => projection.transition_id ? [projection.transition_id] : [],
      ),
    );
  }

  function buildNode(id) {
    const root = element("div", "node");
    root.dataset.id = id;
    const head = element("div", "node-head");
    const tone = element("span", "node-tone");
    const status = element("span", "status-chip");
    const tierChip = element("span", "tier-chip");
    tierChip.hidden = true;
    const who = element("span", "node-who");
    head.append(tone, status, tierChip, who);
    const stateRoles = element("div", "node-state-roles");
    const label = element("div", "node-label");
    const note = element("div", "node-note");
    const source = buildSourceButton(id);
    const marks = element("div", "node-marks");
    const challenge = buildChallengeButton(id);
    challenge.hidden = true;
    head.prepend(challenge);
    const claims = element("div", "node-claims");
    claims.hidden = true;
    head.append(marks);
    const title = element("div", "node-title-row");
    title.append(label, source);
    root.append(stateRoles, title, note, head, claims);
    world.append(root);
    const entry = { root, head, tone, status, tierChip, who, stateRoles, label, note, source, marks, challenge, claims };
    nodeElements.set(id, entry);
    return entry;
  }

  // ── containers as frames ────────────────────────────────────────────
  //
  // A container is a node that owns others. It draws as a named frame around
  // what it owns rather than as a card of its own, which is the Excalidraw
  // shape and the only one that reads as ownership: a card parked above three
  // other cards says "here is a fourth thing", and a box drawn around them
  // says "these three are mine".
  //
  // The frame's geometry is DERIVED from its members every render and is not
  // written back to the document. Two reasons, and the second is the binding
  // one: a frame follows its members the instant a human drags one, and
  // `place_node` clamps a stored width to 640, so a frame spanning three cards
  // could not be stored honestly even if we wanted to. Making the model agree
  // is the packing work in P2 item 3, not this. Until then `atlas_read`'s
  // LAYOUT numbers describe the container's parked card box, not the frame on
  // screen, and that gap is real.
  const FRAME_PAD = 22;
  const FRAME_TITLE_H = 26;
  // The title and the padding below it are one reserved header band. Keeping
  // that number explicit makes frame hit testing use the same child-free band
  // as frame layout instead of treating only the title text as reserved.
  const FRAME_HEADER_H = FRAME_PAD + FRAME_TITLE_H;
  const FRAME_CLAIM_LINE_H = 19;
  // How wide the grab band is, in world units. Inside it you are holding the
  // frame; inside the interior you are holding whatever is under the pointer.
  const FRAME_EDGE = 12;
  const frameElements = new Map();
  let containerChildren = new Map();
  let frameBoxes = new Map();

  function indexContainment() {
    containerChildren = new Map();
    for (const node of atlas.nodes) {
      if (!node.parent) continue;
      if (!containerChildren.has(node.parent)) containerChildren.set(node.parent, []);
      containerChildren.get(node.parent).push(node.id);
    }
    let pruned = false;
    for (const id of collapsedContainers) {
      if (containerChildren.has(id)) continue;
      collapsedContainers.delete(id);
      pruned = true;
    }
    if (pruned) persistCollapsedContainers();
  }

  function isContainer(id) {
    return containerChildren.has(id) && containerChildren.get(id).length > 0;
  }

  function containmentDepthOf(id) {
    let depth = 0;
    let node = atlas.nodes.find((candidate) => candidate.id === id);
    const seen = new Set();
    while (node?.parent && !seen.has(node.parent)) {
      seen.add(node.parent);
      depth += 1;
      node = atlas.nodes.find((candidate) => candidate.id === node.parent);
    }
    return depth;
  }

  function deepestContainment() {
    return atlas.nodes.reduce(
      (deepest, node) => Math.max(deepest, containmentDepthOf(node.id)),
      0,
    );
  }

  function normalizedAltitudeLevel(value) {
    if (value === "all") return "all";
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return "all";
    return Math.max(0, Math.min(Math.floor(numeric), deepestContainment()));
  }

  function persistAltitudeLevel() {
    try {
      localStorage.setItem(ALTITUDE_LENS_STORAGE_KEY, String(altitudeLevel));
    } catch {
      // The lens still works for this page when browser storage is unavailable.
    }
  }

  function paintAltitudeControls() {
    const deepest = deepestContainment();
    const levels = [...Array.from({ length: deepest + 1 }, (_, index) => index), "all"];
    const current = [...altitudeButtons.keys()];
    if (current.length !== levels.length || current.some((level, index) => level !== levels[index])) {
      altitudeButtons.clear();
      const label = element("span", "altitude-label", "Visible detail");
      const controls = [label];
      for (const level of levels) {
        const text = String(level);
        const caption = level === "all" ? "All details" : level === 0 ? "Overview" : `Level ${level}`;
        const button = element("button", "altitude-level", caption);
        button.style.setProperty("--depth-width", `${100 - (level === "all" ? deepest + 1 : level) * 8}%`);
        button.type = "button";
        button.dataset.altitudeLevel = text;
        button.setAttribute("aria-label", level === "all" ? "Show all containment depths" : `Show containment through depth ${level}`);
        button.addEventListener("pointerdown", (event) => event.stopPropagation());
        button.addEventListener("click", (event) => {
          event.stopPropagation();
          chooseAltitudeLevel(level);
          view.focus();
        });
        altitudeButtons.set(level, button);
        controls.push(button);
      }
      altitudeControls.replaceChildren(...controls);
    }
    for (const [level, button] of altitudeButtons) {
      const active = level === altitudeLevel;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    }
  }

  function applyAltitudeLevel(value) {
    altitudeLevel = normalizedAltitudeLevel(value);
    collapsedContainers.clear();
    if (altitudeLevel !== "all") {
      for (const node of atlas.nodes) {
        // A container on the chosen boundary stays visible as the summary of
        // everything below it. Adding deeper containers to the same set is
        // harmless while their ancestor is closed and keeps the rule true if
        // that ancestor is opened directly.
        if (isContainer(node.id) && containmentDepthOf(node.id) >= altitudeLevel) {
          collapsedContainers.add(node.id);
        }
      }
    }
    altitudeManualOverride = false;
    altitudeAppliedDeepest = deepestContainment();
    persistAltitudeLevel();
    persistCollapsedContainers();
    paintAltitudeControls();
  }

  function chooseAltitudeLevel(value, announce = true) {
    applyAltitudeLevel(value);
    repaintDiagramVisibility();
    if (announce) {
      report(altitudeLevel === "all"
        ? "Altitude all. Every containment depth is visible."
        : `Altitude ${altitudeLevel}. Deeper contents are collapsed.`);
    }
  }

  function stepAltitude(direction) {
    const deepest = deepestContainment();
    const current = altitudeLevel === "all" ? deepest + 1 : altitudeLevel;
    const next = Math.max(0, Math.min(current + direction, deepest + 1));
    chooseAltitudeLevel(next > deepest ? "all" : next);
  }

  function syncAltitudeToDocument() {
    const deepest = deepestContainment();
    const clamped = normalizedAltitudeLevel(altitudeLevel);
    const depthChanged = altitudeAppliedDeepest !== null && altitudeAppliedDeepest !== deepest;
    if (
      altitudeNeedsInitialApply
      || clamped !== altitudeLevel
      || (altitudeLevel !== "all" && depthChanged && !altitudeManualOverride)
    ) {
      altitudeNeedsInitialApply = false;
      applyAltitudeLevel(clamped);
      return;
    }
    altitudeNeedsInitialApply = false;
    altitudeAppliedDeepest = deepest;
    paintAltitudeControls();
  }

  function persistCollapsedContainers() {
    try {
      localStorage.setItem(
        DIAGRAM_COLLAPSE_STORAGE_KEY,
        JSON.stringify([...collapsedContainers]),
      );
    } catch {
      // Collapse still works for this page when browser storage is unavailable.
    }
  }

  function hiddenByCollapsedContainer(id) {
    let node = atlas.nodes.find((candidate) => candidate.id === id);
    const seen = new Set();
    while (node?.parent && !seen.has(node.parent)) {
      if (focusVisible && !focusVisible.has(node.parent)) return false;
      if (collapsedContainers.has(node.parent)) return true;
      seen.add(node.parent);
      node = atlas.nodes.find((candidate) => candidate.id === node.parent);
    }
    return false;
  }

  function repaintDiagramVisibility() {
    const marksByTarget = groupMarksByTarget();
    for (const node of atlas.nodes) renderNode(node, marksByTarget.get(node.id) || []);
    paintDetail();
    renderFrames(marksByTarget);
    renderEdges();
  }

  function setContainerCollapsed(id, collapsed) {
    if (!isContainer(id)) return false;
    if (collapsed) collapsedContainers.add(id);
    else collapsedContainers.delete(id);
    altitudeManualOverride = true;
    persistCollapsedContainers();
    repaintDiagramVisibility();
    return true;
  }

  /// Every node under this one, at any depth.
  ///
  /// `read()` in the core crate guarantees the parent field is a tree, cycles
  /// and dangling parents healed at projection time, so this walk terminates
  /// without a visit set. The guard below is belt and braces for a projection
  /// this client did not produce.
  function descendantsOf(id, seen = new Set()) {
    const out = [];
    for (const child of containerChildren.get(id) || []) {
      if (seen.has(child)) continue;
      seen.add(child);
      out.push(child, ...descendantsOf(child, seen));
    }
    return out;
  }

  /// World-space box a frame has to enclose, computed from what it owns.
  ///
  /// Measured from the rendered cards rather than from `node.h`, because the
  /// stored height is the agent's guess until a browser has laid the text out,
  /// and a frame drawn from a guess crops the card it is supposed to contain.
  function computeFrameBox(id) {
    if (frameBoxes.has(id)) return frameBoxes.get(id);
    let box = null;
    for (const child of containerChildren.get(id) || []) {
      const inner = isContainer(child) ? computeFrameBox(child) : null;
      const node = atlas.nodes.find((candidate) => candidate.id === child);
      const entry = nodeElements.get(child);
      // Offsets, not model coordinates. They are world units (the camera is a
      // transform on an ancestor, which offsets are measured before), they
      // carry the browser's real text height rather than the agent's guess,
      // and they are live during a drag, so the frame travels with the card
      // instead of snapping to it one render later.
      const drawn = entry?.root && !entry.root.hidden ? entry.root : null;
      const own = drawn
        ? {
          left: drawn.offsetLeft,
          top: drawn.offsetTop,
          right: drawn.offsetLeft + drawn.offsetWidth,
          bottom: drawn.offsetTop + drawn.offsetHeight,
        }
        : node && !isContainer(child)
          ? {
            left: node.x,
            top: node.y,
            right: node.x + node.w,
            bottom: node.y + (node.h || 40),
          }
          : null;
      for (const candidate of [inner, own]) {
        if (!candidate) continue;
        box = box
          ? {
            left: Math.min(box.left, candidate.left),
            top: Math.min(box.top, candidate.top),
            right: Math.max(box.right, candidate.right),
            bottom: Math.max(box.bottom, candidate.bottom),
          }
          : { ...candidate };
      }
    }
    const liveClaims = claimsOf(id).filter((claim) => !claim.withdrawn).length;
    const claimBand = liveClaims * FRAME_CLAIM_LINE_H;
    const framed = box && {
      left: box.left - FRAME_PAD,
      top: box.top - FRAME_HEADER_H,
      right: box.right + FRAME_PAD,
      bottom: box.bottom + FRAME_PAD + claimBand,
    };
    frameBoxes.set(id, framed);
    return framed;
  }

  // History stays on the object, with the full conversation one click away.
  function markSummary(marks, targetId) {
    if (!marks.length) return [];
    const button = element("button", "node-history");
    button.type = "button";
    button.append(element("span", "history-icon", ""), element("span", null, String(marks.length)));
    const pending = marks.filter(mark => !mark.answer).length;
    const label = `${marks.length} question${marks.length === 1 ? "" : "s"} and notes${pending ? `, ${pending} awaiting response` : ""}`;
    button.title = label;
    button.setAttribute("aria-label", `Open history: ${label}`);
    button.addEventListener("pointerdown", event => event.stopPropagation());
    button.addEventListener("dblclick", event => event.stopPropagation());
    button.addEventListener("click", event => {
      event.stopPropagation();
      select(targetId);
      document.dispatchEvent(new CustomEvent("atlas-open-inspector"));
      inspectorBody.querySelector(".inspector-history")?.scrollIntoView({ block: "start", behavior: "smooth" });
    });
    return [button];
  }

  // ── claims: what the agent says it understands, and the verdict ─────
  //
  // A claim is the agent's, a verdict is the human's, and the node shows
  // both so a review does not scroll away in the transcript. Standing, badge
  // text, and the exact summary sentence come from the shared core.

  function claimsOf(nodeId) {
    return (atlas.claims || []).filter((claim) => claim.about === nodeId);
  }

  function nodeChallenge(nodeId) {
    return JSON.parse(doc.node_challenge(nodeId));
  }

  function challengeWithin(nodeId) {
    return JSON.parse(doc.challenge_within(nodeId));
  }

  function applyChallenge(root, badge, nodeId, aggregate = false) {
    const { standing, review_state, summary } = aggregate
      ? challengeWithin(nodeId)
      : nodeChallenge(nodeId);
    if (standing) root.dataset.challenge = standing;
    else delete root.dataset.challenge;
    badge.hidden = !standing || review_state === "unreviewed";
    badge.dataset.challenge = standing;
    badge.textContent = review_state === "questioned" ? "?" : review_state === "disagreed" ? "!" : standing === "cemented" ? "◆" : "✓";
    badge.title = summary;
    badge.setAttribute("aria-label", `Inspect claims: ${summary}`);
  }

  function buildChallengeButton(id) {
    const button = element("button", "challenge-badge");
    button.type = "button";
    button.addEventListener("pointerdown", (event) => event.stopPropagation());
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      select(id);
      document.dispatchEvent(new CustomEvent("atlas-open-inspector"));
      inspectorBody.scrollTop = 0;
    });
    return button;
  }

  const giveVerdict = (claimId, verdict) => write(() => {
    doc.claim_verdict(claimId, verdict);
    return claimId;
  });

  function claimRow(claim) {
    const row = element("div", "claim-row");
    row.dataset.claimId = claim.id;
    row.dataset.basis = claim.basis;
    row.dataset.verdict = claim.withdrawn ? "withdrawn" : claim.verdict;
    row.addEventListener("pointerdown", (event) => event.stopPropagation());
    const head = element("div", "claim-head");
    const basis = element("span", "claim-basis", claim.basis);
    basis.title = `basis: ${claim.basis}`;
    head.append(basis);
    if (claim.path) {
      const source = element("button", "claim-source", claim.lines ? `${claim.path}:${claim.lines}` : claim.path);
      source.type = "button";
      source.title = "Open the source excerpt this claim rests on";
      source.addEventListener("click", (event) => {
        event.stopPropagation();
        openSourcePopover({ id: claim.id, label: claim.text, path: claim.path, lines: claim.lines });
      });
      head.append(source);
    }
    const verdict = element("span", "claim-verdict");
    if (claim.withdrawn) verdict.textContent = "withdrawn";
    else if (claim.verdict === "accepted") verdict.textContent = "✓ accepted";
    else if (claim.verdict === "rejected") verdict.textContent = "✕ rejected";
    else verdict.textContent = "open";
    head.append(verdict);
    const text = element("div", "claim-text", claim.text);
    row.append(head, text);
    if (!claim.withdrawn && claim.verdict === "open") {
      const actions = element("div", "claim-actions");
      for (const [word, value] of [["Accept", "accepted"], ["Reject", "rejected"]]) {
        const control = element("button", "claim-action", word);
        control.type = "button";
        control.dataset.verdict = value;
        control.addEventListener("click", (event) => {
          event.stopPropagation();
          if (giveVerdict(claim.id, value)) report(`${word}ed "${claim.text}".`);
        });
        actions.append(control);
      }
      row.append(actions);
    }
    const marks = atlas.marks.filter((mark) => mark.target === claim.id);
    for (const mark of marks) {
      const markRow = element("div", "mark-row claim-mark");
      const markHead = element("div");
      markHead.append(element("span", "glyph", `${mark.glyph} `), element("span", null, mark.text));
      markRow.append(markHead);
      markRow.append(element(
        "div",
        mark.answer ? "answer" : "muted",
        mark.answer ? mark.answer : `unanswered, ${GLYPH_MEANING[mark.glyph] || ""}`,
      ));
      row.append(markRow);
    }
    return row;
  }

  const CHALLENGE_TEXT = atlasWasm.challenge_text();

  function isChallengeMark(mark) {
    return mark.glyph === "?" && mark.text === CHALLENGE_TEXT;
  }

  /// One line per live claim on the card itself: enough to see the shape of
  /// the review at a glance. The full rows, with verdict buttons and
  /// sources, live in the Inspect panel where there is room to read them.
  function claimLine(claim) {
    const line = element("div", "claim-line");
    line.dataset.basis = claim.basis;
    line.dataset.verdict = claim.verdict;
    const basis = element("span", "claim-basis", claim.basis);
    const text = element("span", "claim-line-text", claim.text.length > 48 ? `${claim.text.slice(0, 48)}…` : claim.text);
    line.append(basis, text);
    line.title = `${claim.verdict}: ${claim.text}`;
    return line;
  }

  function renderClaims(entry, node, aggregate = false) {
    const live = claimsOf(node.id).filter((claim) => !claim.withdrawn);
    entry.claims.replaceChildren(...live.map(claimLine));
    entry.claims.hidden = live.length === 0;
    applyChallenge(entry.root, entry.challenge, node.id, aggregate);
  }

  function appendClaimRows(body, node) {
    const claims = claimsOf(node.id);
    if (claims.length === 0) return;
    const challenge = nodeChallenge(node.id);
    const section = element("section", "inspector-claims");
    section.append(element("h3", null, "Claims"));
    section.append(element("p", "claim-summary", challenge.summary));
    section.dataset.challenge = challenge.standing;
    for (const claim of claims) section.append(claimRow(claim));
    body.push(section);
  }

  function appendHiddenClaimRows(body, node) {
    if (!collapsedContainers.has(node.id)) return;
    const descendants = descendantsOf(node.id)
      .map((id) => atlas.nodes.find((candidate) => candidate.id === id))
      .filter(Boolean);
    const groups = descendants
      .map((descendant) => ({
        node: descendant,
        claims: claimsOf(descendant.id).filter((claim) => (
          !claim.withdrawn && claim.verdict !== "accepted"
        )),
      }))
      .filter((group) => group.claims.length > 0);
    if (groups.length === 0) return;

    const section = element("section", "inspector-claims hidden-inside");
    section.dataset.challenge = "open";
    section.append(element("h3", null, "hidden inside"));
    for (const group of groups) {
      const claimGroup = element("div", "hidden-claim-group");
      claimGroup.append(element("h4", null, group.node.label));
      for (const claim of group.claims) claimGroup.append(claimRow(claim));
      section.append(claimGroup);
    }
    body.push(section);
  }

  /// The marks of the last render, so the drag path can repaint frames
  /// without recomputing them at pointer rate.
  let marksIndex = new Map();

  function renderFrames(marksByTarget = marksIndex) {
    marksIndex = marksByTarget;
    frameBoxes = new Map();
    const live = new Set();
    for (const node of atlas.nodes) {
      if (!isContainer(node.id)) continue;
      const box = computeFrameBox(node.id);
      if (!box) continue;
      live.add(node.id);
      if (!frameElements.has(node.id)) {
        const root = element("div", "atlas-frame");
        root.dataset.id = node.id;
        const title = element("div", "atlas-frame-title");
        title.dataset.frameTitle = node.id;
        title.title = "Double-click to explore this component. Use the chevron to collapse its contents.";
        title.addEventListener("dblclick", event => { if (event.target.closest("button")) return; event.stopPropagation(); openObjectFocus(node.id); });
        const titleLabel = element("span", "atlas-frame-title-label");
        const stateStatus = element("span", "atlas-frame-state-status");
        const count = element("span", "atlas-frame-count");
        const collapse = element("button", "atlas-frame-collapse", "▾");
        collapse.type = "button";
        collapse.addEventListener("pointerdown", (event) => event.stopPropagation());
        collapse.addEventListener("click", (event) => {
          event.stopPropagation();
          setContainerCollapsed(node.id, !collapsedContainers.has(node.id));
        });
        const source = buildSourceButton(node.id);
        title.append(titleLabel, stateStatus, count, source, collapse);
        // Beside the name, inside the band the hit-test already reserves, so
        // a mark on a container is where a human looks for the container.
        const marks = element("div", "atlas-frame-marks");
        const challenge = buildChallengeButton(node.id);
        challenge.hidden = true;
        title.append(challenge);
        const claims = element("div", "atlas-frame-claims");
        claims.hidden = true;
        root.append(title, marks, claims);
        frameLayer.append(root);
        frameElements.set(
          node.id,
          { root, title, titleLabel, stateStatus, count, source, collapse, marks, challenge, claims },
        );
      }
      const frame = frameElements.get(node.id);
      frame.root.style.left = `${box.left}px`;
      frame.root.style.top = `${box.top}px`;
      frame.root.style.width = `${box.right - box.left}px`;
      frame.root.style.height = `${box.bottom - box.top}px`;
      frame.root.classList.toggle("selected", isSelected(node.id));
      frame.root.dataset.depth = String(depthOf(node.id));
      applyNodeStyle(frame.root, node);
      const owned = descendantsOf(node.id).length;
      const collapsed = collapsedContainers.has(node.id);
      const hiddenByAncestor = hiddenByCollapsedContainer(node.id);
      frame.root.hidden = hiddenByAncestor;
      frame.root.dataset.collapsed = String(collapsed);
      frame.titleLabel.textContent = node.label;
      paintSourceButton(frame.source, node);
      const statePaint = statePaintByMachine.get(node.id);
      const unresolved = statePaint?.unresolved === true;
      frame.stateStatus.textContent = unresolved ? "UNRESOLVED" : "";
      frame.stateStatus.hidden = !unresolved;
      frame.stateStatus.title = unresolved ? statePaint.read_back : "";
      if (unresolved) frame.root.dataset.stateUnresolved = "true";
      else delete frame.root.dataset.stateUnresolved;
      frame.count.textContent = collapsed ? `${owned} hidden` : `${owned} inside`;
      frame.collapse.textContent = collapsed ? "▸" : "▾";
      frame.collapse.setAttribute("aria-expanded", String(!collapsed));
      frame.collapse.setAttribute(
        "aria-label",
        `${collapsed ? "Expand" : "Collapse"} ${node.label}, ${owned} ${collapsed ? "hidden" : "inside"}`,
      );
      const frameMarks = marksByTarget.get(node.id) || [];
      frame.marks.replaceChildren(
        ...markSummary(frameMarks, node.id),
      );
      frame.marks.hidden = frameMarks.length === 0;
      renderClaims(frame, node, collapsed);
      if (hiddenByAncestor) {
        targets.unregister(node.id);
        continue;
      }
      // The frame is what the human sees, so it is what an agent's pointer
      // must ring and what a reveal must bring into view. Registering the
      // hidden card instead would ring a zero-sized box nobody can see.
      targets.register({
        id: node.id,
        label: node.label,
        element: frame.root,
        spatial: true,
        reveal: (options) => revealTarget(node.id, options),
        project: () => projectAnchor(node.id),
      });
    }
    for (const [id, frame] of frameElements) {
      if (live.has(id)) continue;
      frame.root.remove();
      frameElements.delete(id);
    }
  }

  function depthOf(id) {
    return Math.min(containmentDepthOf(id), 4);
  }

  /// The frame's grab band, in world units: the ring, plus the header.
  function frameEdgeHit(id, wx, wy) {
    if (frameElements.get(id)?.root.hidden) return false;
    const box = computeFrameBox(id);
    if (!box) return false;
    const inside = wx >= box.left && wx <= box.right && wy >= box.top && wy <= box.bottom;
    if (!inside) return false;
    const interior = frameInterior(box);
    return !(wx > interior.left && wx < interior.right && wy > interior.top && wy < interior.bottom);
  }

  function frameInterior(box) {
    return {
      left: box.left + FRAME_EDGE,
      top: box.top + FRAME_HEADER_H + FRAME_EDGE,
      right: box.right - FRAME_EDGE,
      bottom: box.bottom - FRAME_EDGE,
    };
  }

  // ── style vocabulary, rendered when the model carries it ─────────────
  //
  // `color`, `emphasis` and `size` are optional on a node. Absent, nothing
  // here changes anything, which is what lets this land before the model half
  // does. The palette is resolved through the core crate's `ink_palette`, the
  // same table the `.excalidraw` export writes, so a card and its export
  // cannot disagree about what "amber" means.
  const EMPHASIS = new Set(["normal", "strong", "muted"]);
  const SIZE_CLASS = new Set(["normal", "primary", "hero"]);
  // `kind` is the old `shape` hint made honest: the card keeps being a card,
  // and its silhouette says which one the author meant. A card that pretended
  // to be a literal ellipse would be a drawn shape, which is a different
  // register and a different tool.
  const KINDS = new Set(["rect", "rectangle", "ellipse", "diamond"]);

  function applyNodeStyle(root, node) {
    const color = typeof node.color === "string" && node.color in INK_CSS ? node.color : "";
    const emphasis = EMPHASIS.has(node.emphasis) ? node.emphasis : "";
    const size = SIZE_CLASS.has(node.size) ? node.size : "";
    const kind = KINDS.has(node.kind) ? node.kind : "";
    if (color) root.dataset.color = color;
    else delete root.dataset.color;
    if (emphasis && emphasis !== "normal") root.dataset.emphasis = emphasis;
    else delete root.dataset.emphasis;
    if (size && size !== "normal") root.dataset.size = size;
    else delete root.dataset.size;
    if (kind && kind !== "rect" && kind !== "rectangle") root.dataset.kind = kind;
    else delete root.dataset.kind;
    // One custom property carries the authored colour so every rule that wants
    // it (border, title, glow) reads the same value.
    root.style.setProperty("--authored-ink", color ? inkOf(color) : "");
  }

  function renderNode(node, marksForNode) {
    const entry = nodeElements.get(node.id) || buildNode(node.id);
    const { root } = entry;
    root.dataset.tone = node.tone;
    root.dataset.status = node.status;
    root.style.left = `${node.x}px`;
    root.style.top = `${node.y}px`;
    root.style.width = `${node.w}px`;
    entry.tone.textContent = node.tone;
    entry.status.textContent = node.status;
    entry.who.textContent = node.touched_by === node.created_by
      ? node.created_by
      : `${node.created_by} → ${node.touched_by}`;
    entry.who.className = `node-who ${node.touched_by}`;
    entry.label.textContent = node.label;
    entry.note.textContent = node.note;
    entry.note.hidden = !node.note;
    paintSourceButton(entry.source, node);
    const stateRoles = [];
    if (node.state_initial === true) {
      stateRoles.push(element("span", "node-state-role node-state-initial", "INITIAL"));
      root.dataset.stateInitial = "true";
    } else {
      delete root.dataset.stateInitial;
    }
    if (node.state_terminal === true) {
      stateRoles.push(element("span", "node-state-role node-state-terminal", "TERMINAL"));
      root.dataset.stateTerminal = "true";
    } else {
      delete root.dataset.stateTerminal;
    }
    if (currentStateIds.has(node.id)) {
      stateRoles.push(element("span", "node-state-role node-state-current", "CURRENT"));
      root.dataset.stateCurrent = "true";
    } else {
      delete root.dataset.stateCurrent;
    }
    entry.stateRoles.replaceChildren(...stateRoles);
    entry.stateRoles.hidden = stateRoles.length === 0;

    entry.marks.replaceChildren(
      ...markSummary(marksForNode, node.id),
    );
    entry.marks.hidden = marksForNode.length === 0;
    renderClaims(entry, node);
    root.classList.toggle("selected", isSelected(node.id));
    applyNodeStyle(root, node);
    applyTierStyling(root, node.id);
    // A container draws as its frame, so its card steps aside rather than
    // sitting inside the box it owns as a fourth sibling. `renderFrames`
    // registers the frame as this node's semantic target in its place.
    root.hidden = isContainer(node.id) || hiddenByCollapsedContainer(node.id);
    if (root.hidden) {
      targets.unregister(node.id);
      return;
    }
    targets.register({
      id: node.id,
      label: node.label,
      element: root,
      spatial: true,
      // By id, not by this render pass's `node`: a reveal can arrive long
      // after this closure was made, and the card will have moved by then.
      reveal: (options) => revealTarget(node.id, options),
      project: () => projectAnchor(node.id),
    });
  }

  function sourceRef(node) {
    return node.lines ? `${node.path}:${node.lines}` : node.path;
  }

  function buildSourceButton(id) {
    const button = element("button", "node-source", "</>");
    button.type = "button";
    button.setAttribute("aria-haspopup", "dialog");
    button.addEventListener("pointerdown", event => event.stopPropagation());
    button.addEventListener("dblclick", event => event.stopPropagation());
    button.addEventListener("click", event => {
      event.stopPropagation();
      const node = atlas.nodes.find(candidate => candidate.id === id);
      if (!node) return;
      select(id);
      openSourcePopover(node, button);
    });
    return button;
  }

  // A card's tier bindings (see src/tier.rs) are extra source references the
  // CRDT claims list does not carry: lanes `atlas_claim_lane` added beyond
  // the node's own path/lines. Shaped like a claim so they merge straight
  // into the same reference list the source viewer already knows how to
  // page through.
  function tierBindingReferences(nodeId) {
    return (nodeTiers.get(nodeId)?.bindings || []).map((binding) => ({
      about: nodeId,
      path: binding.path,
      lines: binding.lines,
      withdrawn: false,
    }));
  }

  function paintSourceButton(button, node) {
    const linked = sourceReferences(node, [...atlas.claims, ...tierBindingReferences(node.id)]).length > 0;
    button.title = linked ? `View code for ${node.label}` : `No source linked to ${node.label}`;
    button.setAttribute("aria-label", button.title);
    button.classList.toggle("source-unlinked", !linked);
  }

  function closeSourcePopover() {
    closeSourceViewer();
    sourcePopoverTarget = null;
  }

  function openSourcePopover(node, origin) {
    sourcePopoverTarget = node.id;
    openSourceViewer(node, sourceReferences(node, [...atlas.claims, ...tierBindingReferences(node.id)]), origin);
  }

  function transitionLabel(edge) {
    return `${edge.event}${edge.guard ? ` when ${edge.guard}` : ""}`;
  }

  function isBackTransition(edge) {
    if (!edge?.event) return false;
    if (edge.from === edge.to) return true;
    const from = atlas.nodes.find((node) => node.id === edge.from);
    const to = atlas.nodes.find((node) => node.id === edge.to);
    if (!from || !to || from.parent !== to.parent) return false;
    const machine = atlas.nodes.find((node) => node.id === from.parent);
    if (machine?.diagram_kind !== "state_machine") return false;
    const dx = to.x - from.x;
    const dy = to.y - from.y;
    return Math.abs(dx) >= Math.abs(dy) ? dx < 0 : Math.abs(dx) < 1 && dy < 0;
  }

  const TRANSITION_LANE_GAP = 56;
  const EDGE_LABEL_CLEARANCE = 6;

  function drawnNodeBox(id) {
    const node = atlas.nodes.find((candidate) => candidate.id === id);
    if (!node) return null;
    const root = nodeElements.get(id)?.root;
    const left = root?.offsetLeft ?? node.x;
    const top = root?.offsetTop ?? node.y;
    const width = root?.offsetWidth || node.w;
    const height = root?.offsetHeight || node.h || 56;
    return {
      left,
      top,
      right: left + width,
      bottom: top + height,
      width,
      height,
      center: [left + width / 2, top + height / 2],
    };
  }

  function boxEdgeToward(box, toward) {
    const [cx, cy] = box.center;
    const dx = toward[0] - cx;
    const dy = toward[1] - cy;
    if (Math.abs(dx) < 0.001 && Math.abs(dy) < 0.001) return [cx, cy];
    const scaleX = Math.abs(dx) < 0.001
      ? Number.POSITIVE_INFINITY
      : ((dx > 0 ? box.right : box.left) - cx) / dx;
    const scaleY = Math.abs(dy) < 0.001
      ? Number.POSITIVE_INFINITY
      : ((dy > 0 ? box.bottom : box.top) - cy) / dy;
    const scale = Math.max(0, Math.min(1, scaleX, scaleY));
    return [cx + dx * scale, cy + dy * scale];
  }

  function machineTransitions(edge) {
    const parent = atlas.nodes.find((node) => node.id === edge.from)?.parent;
    return (atlas.edges || [])
      .filter((candidate) => {
        if (!candidate.event) return false;
        const from = atlas.nodes.find((node) => node.id === candidate.from);
        const to = atlas.nodes.find((node) => node.id === candidate.to);
        return from?.parent === parent && to?.parent === parent;
      })
      .sort((a, b) => (
        Number(a.transition_order) - Number(b.transition_order)
        || a.id.localeCompare(b.id)
      ));
  }

  function quadraticPoint(points, t) {
    const one = 1 - t;
    return [
      one * one * points[0][0] + 2 * one * t * points[1][0] + t * t * points[2][0],
      one * one * points[0][1] + 2 * one * t * points[1][1] + t * t * points[2][1],
    ];
  }

  function quadraticTangent(points, t) {
    return [
      2 * (1 - t) * (points[1][0] - points[0][0]) + 2 * t * (points[2][0] - points[1][0]),
      2 * (1 - t) * (points[1][1] - points[0][1]) + 2 * t * (points[2][1] - points[1][1]),
    ];
  }

  function cubicPoint(points, t) {
    const one = 1 - t;
    return [
      one ** 3 * points[0][0]
        + 3 * one * one * t * points[1][0]
        + 3 * one * t * t * points[2][0]
        + t ** 3 * points[3][0],
      one ** 3 * points[0][1]
        + 3 * one * one * t * points[1][1]
        + 3 * one * t * t * points[2][1]
        + t ** 3 * points[3][1],
    ];
  }

  function cubicTangent(points, t) {
    const one = 1 - t;
    return [
      3 * one * one * (points[1][0] - points[0][0])
        + 6 * one * t * (points[2][0] - points[1][0])
        + 3 * t * t * (points[3][0] - points[2][0]),
      3 * one * one * (points[1][1] - points[0][1])
        + 6 * one * t * (points[2][1] - points[1][1])
        + 3 * t * t * (points[3][1] - points[2][1]),
    ];
  }

  function polylineSegment(points, t) {
    const lengths = points.slice(1).map((p, i) => Math.hypot(p[0]-points[i][0], p[1]-points[i][1]));
    let remaining = Math.max(0, Math.min(1, t)) * lengths.reduce((sum, length) => sum+length, 0);
    for (let i=0; i<lengths.length; i++) {
      if (remaining <= lengths[i] || i === lengths.length-1) return { a: points[i], b: points[i+1], t: lengths[i] ? remaining/lengths[i] : 0 };
      remaining -= lengths[i];
    }
    return { a: points[0], b: points[0], t: 0 };
  }

  function routePoint(route, t) {
    if (route.kind === "polyline") {
      const segment = polylineSegment(route.points, t);
      return [segment.a[0]+(segment.b[0]-segment.a[0])*segment.t, segment.a[1]+(segment.b[1]-segment.a[1])*segment.t];
    }
    if (route.kind === "quadratic") return quadraticPoint(route.points, t);
    if (route.kind === "cubic") return cubicPoint(route.points, t);
    const [start, end] = route.points;
    return [
      start[0] + (end[0] - start[0]) * t,
      start[1] + (end[1] - start[1]) * t,
    ];
  }

  function routeTangent(route, t) {
    if (route.kind === "polyline") {
      const { a, b } = polylineSegment(route.points, t);
      return [b[0]-a[0], b[1]-a[1]];
    }
    if (route.kind === "quadratic") return quadraticTangent(route.points, t);
    if (route.kind === "cubic") return cubicTangent(route.points, t);
    return [
      route.points[1][0] - route.points[0][0],
      route.points[1][1] - route.points[0][1],
    ];
  }

  // `layout_state_machine` reserves one lane per input transition. The CRDT
  // keeps that input order as `transition_order`, so every browser replica can
  // project the same lanes without storing browser-only route or label boxes.
  function transitionCurve(edge) {
    const transitions = machineTransitions(edge);
    const laneIndex = Math.max(0, transitions.findIndex((candidate) => candidate.id === edge.id));
    const fromBox = drawnNodeBox(edge.from);
    const toBox = drawnNodeBox(edge.to);
    if (!fromBox || !toBox) return null;
    const machineId = atlas.nodes.find((node) => node.id === edge.from)?.parent;
    const states = atlas.nodes.filter((node) => node.parent === machineId);
    const largest = states.reduce((size, state) => {
      const box = drawnNodeBox(state.id);
      return box ? Math.max(size, box.width, box.height) : size;
    }, 1);
    const lane = largest * 0.55 + TRANSITION_LANE_GAP * (laneIndex + 1) * 0.22;

    if (edge.from === edge.to) {
      const machineBox = frameBoxes.get(machineId);
      const machineCenter = machineBox
        ? [(machineBox.left + machineBox.right) / 2, (machineBox.top + machineBox.bottom) / 2]
        : states.reduce(
          (sum, state) => {
            const box = drawnNodeBox(state.id);
            return box ? [sum[0] + box.center[0] / states.length, sum[1] + box.center[1] / states.length] : sum;
          },
          [0, 0],
        );
      let angle = Math.atan2(
        fromBox.center[1] - machineCenter[1],
        fromBox.center[0] - machineCenter[0],
      );
      if (!Number.isFinite(angle)) angle = -Math.PI / 2;
      angle += 0.37 + laneIndex * 0.19;
      const outward = [Math.cos(angle), Math.sin(angle)];
      const across = [-outward[1], outward[0]];
      const controlCenter = [
        fromBox.center[0] + outward[0] * lane,
        fromBox.center[1] + outward[1] * lane,
      ];
      const firstControl = [
        controlCenter[0] + across[0] * lane * 0.42,
        controlCenter[1] + across[1] * lane * 0.42,
      ];
      const secondControl = [
        controlCenter[0] - across[0] * lane * 0.42,
        controlCenter[1] - across[1] * lane * 0.42,
      ];
      const start = boxEdgeToward(fromBox, firstControl);
      const end = boxEdgeToward(toBox, secondControl);
      const points = [start, firstControl, secondControl, end];
      return {
        kind: "cubic",
        points,
        d: `M ${start[0]} ${start[1]} C ${firstControl[0]} ${firstControl[1]}, ${secondControl[0]} ${secondControl[1]}, ${end[0]} ${end[1]}`,
        tangent: [secondControl, end],
      };
    }

    const dx = toBox.center[0] - fromBox.center[0];
    const dy = toBox.center[1] - fromBox.center[1];
    const length = Math.hypot(dx, dy);
    if (!Number.isFinite(length) || length < 1) return null;
    const control = [
      (fromBox.center[0] + toBox.center[0]) / 2 - dy / length * lane,
      (fromBox.center[1] + toBox.center[1]) / 2 + dx / length * lane,
    ];
    const start = boxEdgeToward(fromBox, control);
    const end = boxEdgeToward(toBox, control);
    const points = [start, control, end];
    return {
      kind: "quadratic",
      points,
      d: `M ${start[0]} ${start[1]} Q ${control[0]} ${control[1]}, ${end[0]} ${end[1]}`,
      tangent: [control, end],
    };
  }

  function paddedBox(box, padding = EDGE_LABEL_CLEARANCE) {
    return {
      left: box.left - padding,
      top: box.top - padding,
      right: box.right + padding,
      bottom: box.bottom + padding,
    };
  }

  function boxesIntersect(a, b) {
    return a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
  }

  function visibleStateBoxes() {
    const boxes = atlas.nodes.flatMap((node) => {
      if (!inExplorationView(node.id) || isContainer(node.id) || hiddenByCollapsedContainer(node.id)) return [];
      const box = drawnNodeBox(node.id);
      return box ? [paddedBox(box)] : [];
    });
    for (const [id, box] of frameBoxes) {
      if (!inExplorationView(id) || hiddenByCollapsedContainer(id)) continue;
      boxes.push({ left: box.left, right: box.right, top: box.top, bottom: box.top + FRAME_TITLE_H });
    }
    return boxes;
  }

  function pairOffset(edge) {
    if (!edge) return -18;
    const peers = machineTransitions(edge)
      .filter((candidate) => (
        (candidate.from === edge.from && candidate.to === edge.to)
        || (candidate.from === edge.to && candidate.to === edge.from)
      ));
    const index = Math.max(0, peers.findIndex((candidate) => candidate.id === edge.id));
    const side = index % 2 === 0 ? -1 : 1;
    return side * (18 + Math.floor(index / 2) * 10);
  }

  function labelPlacement(route, width, height, preferredOffset, occupied) {
    const along = [0.5, 0.4, 0.6, 0.3, 0.7, 0.22, 0.78];
    const preferredSide = Math.sign(preferredOffset) || -1;
    const base = Math.abs(preferredOffset);
    const offsets = [];
    for (const extra of [0, 16, 32, 48, 64, 80, 104]) {
      offsets.push(preferredSide * (base + extra));
      if (extra > 0) offsets.push(-preferredSide * (base + extra));
    }
    for (const t of along) {
      const anchor = routePoint(route, t);
      const tangent = routeTangent(route, t);
      const length = Math.hypot(tangent[0], tangent[1]);
      const normal = length > 0.001
        ? [-tangent[1] / length, tangent[0] / length]
        : [0, -1];
      for (const offset of offsets) {
        const center = [anchor[0] + normal[0] * offset, anchor[1] + normal[1] * offset];
        const box = {
          left: center[0] - width / 2,
          top: center[1] - height / 2,
          right: center[0] + width / 2,
          bottom: center[1] + height / 2,
        };
        const placed = { anchor, center, box, t, offset };
        if (!occupied.some((other) => boxesIntersect(paddedBox(box), other))) return placed;
      }
    }

    // The finite close candidates preserve a short leader in the usual case.
    // A dense future machine can exhaust them, so keep walking both normals at
    // the route midpoint. Finite rectangles always clear at a finite distance.
    const t = 0.5;
    const anchor = routePoint(route, t);
    const tangent = routeTangent(route, t);
    const length = Math.hypot(tangent[0], tangent[1]);
    const normal = length > 0.001
      ? [-tangent[1] / length, tangent[0] / length]
      : [0, -1];
    for (let extra = 128; ; extra += 32) {
      for (const side of [preferredSide, -preferredSide]) {
        const offset = side * (base + extra);
        const center = [anchor[0] + normal[0] * offset, anchor[1] + normal[1] * offset];
        const box = {
          left: center[0] - width / 2,
          top: center[1] - height / 2,
          right: center[0] + width / 2,
          bottom: center[1] + height / 2,
        };
        const placed = { anchor, center, box, t, offset };
        if (!occupied.some((other) => boxesIntersect(paddedBox(box), other))) return placed;
      }
    }
  }

  function appendLabelLeader(id, anchor, box, taken) {
    const nearest = [
      Math.max(box.left, Math.min(anchor[0], box.right)),
      Math.max(box.top, Math.min(anchor[1], box.bottom)),
    ];
    if (Math.hypot(nearest[0] - anchor[0], nearest[1] - anchor[1]) < 3) return;
    const leader = document.createElementNS(SVG_NS, "line");
    leader.setAttribute(
      "class",
      `atlas-edge-label-leader${taken ? " atlas-transition-taken" : ""}`,
    );
    leader.setAttribute("x1", anchor[0]);
    leader.setAttribute("y1", anchor[1]);
    leader.setAttribute("x2", nearest[0]);
    leader.setAttribute("y2", nearest[1]);
    leader.dataset.edgeId = id;
    edgeLayer.append(leader);
  }

  function inlineEdgeLabel(route, width, height, occupied) {
    if (route.kind !== "polyline") return null;
    const candidates = route.points.slice(1).map((b, i) => {
      const a = route.points[i];
      return { a, b, horizontal: a[1] === b[1], length: Math.hypot(b[0] - a[0], b[1] - a[1]) };
    }).sort((a, b) => b.length - a.length || Number(b.horizontal) - Number(a.horizontal));
    for (const { a, b, horizontal, length } of candidates) {
      if (length < (horizontal ? width : height) + 28) continue;
      for (const t of [.5, .35, .65, .2, .8]) {
        const center = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        const box = { left: center[0] - width / 2, top: center[1] - height / 2, right: center[0] + width / 2, bottom: center[1] + height / 2 };
        if (!occupied.some(other => boxesIntersect(paddedBox(box), other))) return { anchor: center, center, box, t, offset: 0 };
      }
    }
    return null;
  }

  let hoveredConnection = null;
  let pinnedConnection = null;
  const connectionReadout = element("div", "connection-readout");
  connectionReadout.setAttribute("role", "status");
  connectionReadout.hidden = true;
  view.append(connectionReadout);
  function paintConnectionFocus() {
    const id = hoveredConnection || pinnedConnection;
    const edge = (atlas.edges || []).find(edge => edge.id === id);
    edgeLayer.classList.toggle("is-tracing", Boolean(edge));
    for (const el of edgeLayer.querySelectorAll("[data-connection-id], .atlas-edge-label-frame, .atlas-edge-label-leader")) {
      el.classList.toggle("connection-active", Boolean(edge) && (el.dataset.connectionId || el.dataset.edgeId) === id);
    }
    for (const group of edgeLayer.querySelectorAll(".atlas-relationship")) {
      group.setAttribute("aria-pressed", String(group.dataset.connectionId === pinnedConnection));
    }
    for (const [id, entry] of nodeElements) {
      entry.root.classList.toggle("connection-endpoint", Boolean(edge) && (edge.from === id || edge.to === id));
    }
    connectionReadout.hidden = !edge;
    connectionReadout.textContent = edge ? `${titleForTarget(edge.from)} → ${titleForTarget(edge.to)}${edge.label ? ` · ${edge.label}` : ""}${pinnedConnection ? " · Esc to clear" : " · Click to keep highlighted"}` : "";
  }
  view.addEventListener("keydown", event => {
    if (event.key === "Escape" && (pinnedConnection || hoveredConnection)) {
      pinnedConnection = null; hoveredConnection = null; paintConnectionFocus();
    }
  });

  function relationshipGroup(link, edge, line, points, color) {
    const group = document.createElementNS(SVG_NS, "g");
    group.setAttribute("class", "atlas-relationship");
    group.dataset.connectionId = link.id;
    group.setAttribute("tabindex", "0");
    group.setAttribute("role", "button");
    group.setAttribute("aria-label", `${titleForTarget(edge.from)} to ${titleForTarget(edge.to)}${link.label ? `: ${link.label}` : ""}. Highlight connection.`);
    const halo = line.cloneNode(false);
    halo.removeAttribute("data-edge-id");
    halo.setAttribute("class", "relationship-halo");
    halo.setAttribute("stroke", "var(--canvas)");
    halo.setAttribute("stroke-width", "7");
    const hit = line.cloneNode(false);
    hit.removeAttribute("data-edge-id");
    hit.setAttribute("class", "relationship-hit");
    hit.setAttribute("stroke", "transparent");
    hit.setAttribute("stroke-width", "16");
    line.classList.add("relationship-line");
    const port = document.createElementNS(SVG_NS, "circle");
    port.setAttribute("class", "relationship-port");
    port.setAttribute("cx", points[0][0]); port.setAttribute("cy", points[0][1]);
    port.setAttribute("r", "3.5"); port.setAttribute("fill", "var(--canvas)");
    port.setAttribute("stroke", color); port.setAttribute("stroke-width", "1.5");
    const head = arrowhead(points.at(-2), points.at(-1), "triangle", color);
    head.classList.add("relationship-arrow");
    group.append(halo, line, port, head, hit);
    group.addEventListener("pointerenter", () => { hoveredConnection = link.id; paintConnectionFocus(); });
    group.addEventListener("pointerleave", () => { hoveredConnection = null; paintConnectionFocus(); });
    group.addEventListener("focus", () => { hoveredConnection = link.id; paintConnectionFocus(); });
    group.addEventListener("blur", () => { hoveredConnection = null; paintConnectionFocus(); });
    group.addEventListener("pointerdown", event => event.stopPropagation());
    const toggle = () => { pinnedConnection = pinnedConnection === link.id ? null : link.id; paintConnectionFocus(); };
    group.addEventListener("click", event => { event.stopPropagation(); toggle(); });
    group.addEventListener("keydown", event => {
      if (event.key === "Enter" || event.key === " ") { event.preventDefault(); event.stopPropagation(); toggle(); }
    });
    return group;
  }

  const edgeLabelMeasure = element("div", "atlas-edge-label atlas-edge-measure");
  edgeLabelMeasure.setAttribute("aria-hidden", "true");
  view.append(edgeLabelMeasure);

  // ── trust tiers + undeclared lanes (Explain / Map / Cement) ───────────
  //
  // Both are host-computed, never authored: see src/tier.rs and
  // src/lanes.rs. Polled separately from the CRDT render loop on purpose —
  // a bound file's hash or a G8 check result can change with nobody
  // touching the document at all. CSS gates all of this on
  // body[data-map-mode]; the data this writes is present in every mode,
  // Explain just does not draw it (see atlas-mode.js and styles.css).
  let nodeTiers = new Map();
  function applyTierStyling(root, id) {
    const tier = nodeTiers.get(id);
    const entry = nodeElements.get(id);
    if (!tier) {
      delete root.dataset.tier;
      delete root.dataset.struck;
      if (entry?.tierChip) entry.tierChip.hidden = true;
      return;
    }
    root.dataset.tier = tier.tier;
    if (tier.struck) root.dataset.struck = "true";
    else delete root.dataset.struck;
    if (entry?.tierChip) {
      entry.tierChip.hidden = false;
      entry.tierChip.dataset.tier = tier.tier;
      entry.tierChip.textContent = tier.chip;
      entry.tierChip.title = `Trust tier: ${tier.tier}`;
    }
  }
  async function refreshTiers() {
    try {
      const response = await fetch("/atlas/review", { cache: "no-store" });
      const body = await response.json();
      if (!body.ok) return;
      nodeTiers = new Map(
        (body.review.nodes || [])
          .filter((node) => node.tier)
          .map((node) => [node.id, node.tier]),
      );
      for (const [id, entry] of nodeElements) applyTierStyling(entry.root, id);
    } catch {
      // A quiet miss: cards keep showing the last tier they knew.
    }
  }

  const undeclaredFrame = element("section", "atlas-undeclared-frame");
  undeclaredFrame.setAttribute("aria-label", "Found in the code, not in the agreement");
  const undeclaredHeading = element("h3", null, "Found in the code, not in the agreement");
  const undeclaredList = element("div", "undeclared-list");
  undeclaredFrame.append(undeclaredHeading, undeclaredList);
  // A sibling of the canvas in normal flow, not a child positioned inside
  // it: a floating overlay would cover whatever canvas content happens to
  // sit underneath at any given camera position, at any viewport. Docked
  // below the canvas, it reserves its own space instead and the canvas
  // (flex: 1) simply shrinks to fit beside it.
  (view.parentElement || view).append(undeclaredFrame);

  async function claimLane(laneId, cardId) {
    const response = await fetch("/atlas/lanes/claim", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ card_id: cardId, lane_id: laneId }),
    });
    const body = await response.json();
    if (!body.ok) throw new Error(body.error || "claim refused");
    return body.summary;
  }

  async function removeLane(laneId, reason) {
    const response = await fetch("/atlas/lanes/remove", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ lane_id: laneId, reason }),
    });
    const body = await response.json();
    if (!body.ok) throw new Error(body.error || "removal refused");
    return body.result;
  }

  function renderUndeclaredLanes(lanes) {
    undeclaredList.replaceChildren();
    for (const lane of lanes) {
      const card = element("article", "undeclared-card");
      card.dataset.laneId = lane.lane_id;
      const kind = element("span", "undeclared-kind", lane.kind);
      const label = element("div", "undeclared-label", lane.label || lane.detail);
      const evidence = element(
        "div",
        "undeclared-evidence",
        `${lane.evidence.path}:${lane.evidence.line}  ${lane.evidence.snippet}`,
      );
      card.append(kind, label, evidence);
      if (lane.removal) {
        // Already cemented as a removal gate: this is no longer a plain
        // undeclared pulse, it is red or green depending on whether the
        // code is actually gone yet. No further action here; re-running
        // atlas_remove_lane or claiming it are still possible over MCP, but
        // the browser card just reports what G8 says.
        card.dataset.tier = lane.removal.tier;
        const chip = element("span", "tier-chip", lane.removal.chip);
        chip.dataset.tier = lane.removal.tier;
        card.append(chip);
      } else {
        const actions = element("div", "undeclared-actions");
        const claim = element("button", null, "Claim with selected card");
        claim.type = "button";
        claim.title = selected ? `Bind ${titleForTarget(selected)} to this lane` : "Select a card first";
        claim.disabled = !selected;
        claim.addEventListener("click", async () => {
          if (!selected) return;
          claim.disabled = true;
          claim.textContent = "Claiming…";
          try {
            await claimLane(lane.lane_id, selected);
            await refreshLanes();
            await refreshTiers();
          } catch (error) {
            claim.disabled = false;
            claim.textContent = "Claim failed";
            claim.title = String(error);
          }
        });
        const remove = element("button", null, "Remove from the code");
        remove.type = "button";
        remove.title = "Cement a G8 obligation that this code must be gone";
        remove.addEventListener("click", async () => {
          remove.disabled = true;
          remove.textContent = "Cementing…";
          try {
            const reason = globalThis.prompt(
              "Why should this be removed?",
              "this lane should not exist in the code",
            );
            if (reason === null) {
              remove.disabled = false;
              remove.textContent = "Remove from the code";
              return;
            }
            await removeLane(lane.lane_id, reason);
            await refreshLanes();
          } catch (error) {
            remove.disabled = false;
            remove.textContent = "Remove failed";
            remove.title = String(error);
          }
        });
        actions.append(claim, remove);
        card.append(actions);
      }
      undeclaredList.append(card);
    }
    const stillUndeclared = lanes.filter((lane) => !lane.removal).length;
    undeclaredHeading.textContent = `Found in the code, not in the agreement (${stillUndeclared})`;
  }

  async function refreshLanes() {
    try {
      const response = await fetch("/atlas/lanes", { cache: "no-store" });
      const body = await response.json();
      if (!body.ok) return;
      renderUndeclaredLanes(body.undeclared || []);
    } catch {
      // A quiet miss: the panel keeps showing its last known lanes.
    }
  }

  refreshTiers();
  refreshLanes();
  setInterval(refreshTiers, 3000);
  setInterval(refreshLanes, 4000);
  document.addEventListener("atlas-map-mode-changed", () => {
    refreshTiers();
    refreshLanes();
  });
  function ordinaryLabelSize(label, cache) {
    if (!cache.has(label)) {
      edgeLabelMeasure.textContent = label;
      cache.set(label, { width: Math.ceil(edgeLabelMeasure.offsetWidth) + 2, height: Math.max(26, Math.ceil(edgeLabelMeasure.offsetHeight)) });
    }
    return cache.get(label);
  }

  function appendEdgeLabel(request, occupied, measurements) {
    const { id, label, route, transition, edge, taken } = request;
    if (!label) return null;
    const { width, height } = transition
      ? { width: Math.min(300, Math.max(92, label.length * 5.8)), height: label.length > 38 ? 42 : 26 }
      : ordinaryLabelSize(label, measurements);
    const placed = (!transition && inlineEdgeLabel(route, width, height, occupied))
      || labelPlacement(route, width, height, pairOffset(edge), occupied);
    if (!placed) return null;
    appendLabelLeader(id, placed.anchor, placed.box, taken);
    const frame = document.createElementNS(SVG_NS, "foreignObject");
    frame.setAttribute(
      "class",
      `atlas-edge-label-frame${transition ? " transition" : ""}${taken ? " atlas-transition-taken" : ""}`,
    );
    frame.setAttribute("x", placed.box.left);
    frame.setAttribute("y", placed.box.top);
    frame.setAttribute("width", width);
    frame.setAttribute("height", height);
    frame.dataset.edgeId = id;
    frame.dataset.routeT = String(placed.t);
    frame.dataset.routeOffset = String(placed.offset);
    const text = document.createElementNS(XHTML_NS, "div");
    text.setAttribute("class", "atlas-edge-label");
    text.textContent = label;
    frame.append(text);
    edgeLayer.append(frame);
    occupied.push(paddedBox(placed.box));
    return placed;
  }

  // Ordinary links use core-computed endpoints. State-machine transitions use
  // their persisted input order to reproduce the host's distinct route lanes.
  // Labels are added after every route so no later path can cover the text.
  function renderEdges() {
    edgeLayer.replaceChildren();
    const labels = [];
    for (const link of painting.links || []) {
      const edge = (atlas.edges || []).find((candidate) => candidate.id === link.id);
      if (edge && (!inExplorationView(edge.from) || !inExplorationView(edge.to))) continue;
      if (edge && (hiddenByCollapsedContainer(edge.from) || hiddenByCollapsedContainer(edge.to))) {
        continue;
      }
      const transition = Boolean(edge?.event);
      const back = transition && isBackTransition(edge);
      const taken = transition && takenTransitionIds.has(link.id);
      const color = transition ? "#78a9ff" : "#7892ad";
      if (transition) {
        const curve = transitionCurve(edge);
        if (curve) {
          const path = document.createElementNS(SVG_NS, "path");
          path.setAttribute("d", curve.d);
          path.setAttribute(
            "class",
            `atlas-transition-edge${back ? " atlas-transition-back-edge" : ""}${taken ? " atlas-transition-taken" : ""}`,
          );
          if (taken) path.dataset.transitionTaken = "true";
          path.dataset.edgeId = link.id;
          edgeLayer.append(path);
          edgeLayer.append(arrowhead(curve.tangent[0], curve.tangent[1], "triangle", color));
          labels.push({
            id: link.id,
            label: transitionLabel(edge),
            route: curve,
            transition: true,
            edge,
            taken,
          });
          continue;
        }
      }
      const [a, b] = link.ends;
      const points = link.path?.length >= 2 ? link.path : [a, b];
      const line = document.createElementNS(SVG_NS, "polyline");
      line.setAttribute("points", points.map(point => point.join(",")).join(" "));
      line.setAttribute("fill", "none");
      line.setAttribute("stroke-linejoin", "round");
      if (!link.path?.length) {
        line.setAttribute("stroke-dasharray", "4 4");
        const title = document.createElementNS(SVG_NS, "title");
        title.textContent = "Route unavailable: measured cards overlap or exceed routing capacity.";
        line.append(title);
      }
      line.setAttribute("stroke", color);
      line.setAttribute("stroke-width", "1.5");
      if (transition) {
        line.setAttribute(
          "class",
          `atlas-transition-edge${taken ? " atlas-transition-taken" : ""}`,
        );
        if (taken) line.dataset.transitionTaken = "true";
      }
      line.dataset.edgeId = link.id;
      if (edge && !transition) edgeLayer.append(relationshipGroup(link, edge, line, points, color));
      else {
        edgeLayer.append(line);
        edgeLayer.append(arrowhead(points.at(-2), points.at(-1), "triangle", color));
      }
      labels.push({
        id: link.id,
        label: transition ? transitionLabel(edge) : link.label,
        route: { kind: "polyline", points },
        transition,
        edge,
        taken,
      });
    }
    labels.sort((a, b) => {
      if (a.transition && b.transition) {
        return Number(a.edge.transition_order) - Number(b.edge.transition_order)
          || a.id.localeCompare(b.id);
      }
      if (a.transition !== b.transition) return a.transition ? -1 : 1;
      return a.id.localeCompare(b.id);
    });
    const occupied = visibleStateBoxes();
    const measurements = new Map();
    for (const label of labels) appendEdgeLabel(label, occupied, measurements);
    paintConnectionFocus();
  }

  // ── the drawn layer ─────────────────────────────────────────────────
  //
  // Every coordinate painted here was computed by the core crate and arrived
  // through `doc.painting()`: outline polygons for ink, binding-resolved
  // endpoints for arrows. This function chooses colours and appends elements.
  // It does not decide where anything is, the moment it did, the page and the
  // agent would be reasoning about two different drawings.

  const isBoxForm = (form) => form === "rect" || form === "ellipse" || form === "diamond" || form === "frame";
  const isSizedForm = (form) => isBoxForm(form) || form === "text" || form === "segment";

  function motionMatchFor(shape) {
    for (const owner of segmentAncestors(shape)) {
      if (!isMotionProgram(owner.segment_motion)) continue;
      for (const track of owner.segment_motion.tracks) {
        const index = Array.isArray(track.target_ids)
          ? track.target_ids.indexOf(shape.id)
          : -1;
        if (index >= 0) return { owner, program: owner.segment_motion, track, index };
      }
    }
    return null;
  }

  function preservesSegmentSource(shape) {
    if (isMotionProgram(shape.segment_motion)) {
      if (shape.segment_motion.child_compositing !== "overlay") return false;
      const box = Array.isArray(shape.segment_prompt_box) ? shape.segment_prompt_box : null;
      const sourceWidth = Number(shape.segment_source_width) || 0;
      const sourceHeight = Number(shape.segment_source_height) || 0;
      if (!box || box.length !== 4 || !sourceWidth || !sourceHeight) return false;
      const coveredWidth = Math.max(0, Number(box[2]) - Number(box[0]));
      const coveredHeight = Math.max(0, Number(box[3]) - Number(box[1]));
      // Overlay compositing is honest only when the owner already preserves
      // essentially the whole source. A center mask with four surrounding
      // stage masks is a composite, not a background with redundant parts.
      return (coveredWidth * coveredHeight) / (sourceWidth * sourceHeight) >= 0.8;
    }
    if (shape.segment_animation && shape.segment_animation !== "none") return false;
    return atlas.shapes.some((candidate) => (
      candidate.form === "segment"
      && candidate.segment_parent_id === shape.id
      && candidate.segment_status === "materialized"
      && candidate.segment_mask_runs
    ));
  }

  function activateSegmentMotions(group, shape) {
    const body = group._atlasSegmentBody;
    const match = body?._atlasMotion;
    if (!body || !match) return;
    const { owner, program, track, index } = match;
    const preservedBase = preservesSegmentSource(owner);
    const overlay = program.child_compositing === "overlay" && preservedBase;
    const compositeBase = !preservedBase;
    const frames = track.keyframes.map((frame) => ({
      offset: frame.at,
      transform: `translate(${frame.x || 0}px, ${frame.y || 0}px) rotate(${frame.rotate || 0}deg) scale(${frame.scale_x ?? 1}, ${frame.scale_y ?? 1})`,
      opacity: overlay || compositeBase ? 1 : frame.opacity ?? 1,
      visibility: overlay && (frame.opacity ?? 1) <= 0.01 ? "hidden" : "visible",
    }));
    const curve = Array.isArray(track.curve)
      ? `cubic-bezier(${track.curve.join(",")})`
      : "linear";
    const origin = track.origin || [shape.segment_pivot_x, shape.segment_pivot_y];
    const bodyCanvas = body.querySelector(".segment-body-canvas");
    if (overlay && bodyCanvas) bodyCanvas.style.opacity = "1";
    body.dataset.motionActive = "true";
    group.style.transformBox = "view-box";
    group.style.transformOrigin = `${shape.x + origin[0] * shape.w}px ${shape.y + origin[1] * shape.h}px`;
    const animation = group.animate(frames, {
      duration: track.duration_ms,
      delay: (track.delay_ms || 0) + (track.stagger_ms || 0) * index,
      iterations: track.loop ? Infinity : 1,
      direction: track.alternate ? "alternate" : "normal",
      easing: curve,
      fill: "both",
    });
    animation.id = `atlas-motion:${owner.id}:${program.label}:${track.label}:${body.dataset.segmentId}`;
    body.dataset.motionAnimationId = animation.id;
    group._atlasMotionAnimation = animation;
    if (!track.loop) {
      animation.finished.then(() => {
        if (group._atlasMotionAnimation !== animation) return;
        if (bodyCanvas?.dataset.redundantSourceOverlay === "true") bodyCanvas.style.opacity = "0";
        body.dataset.motionActive = "false";
        group._atlasMotionAnimation = null;
        animation.cancel();
      }).catch(() => {
        // Cancellation is the normal interruption path. A rerender or replay
        // has already taken ownership of the next visible state.
      });
    }
  }

  function replaySegmentMotion(ownerId) {
    let replayed = 0;
    for (const group of view.querySelectorAll("g.shape.segment-shape")) {
      const body = group._atlasSegmentBody;
      if (body?._atlasMotion?.owner?.id !== ownerId) continue;
      group._atlasMotionAnimation?.cancel();
      const shape = segmentById(body.dataset.segmentId);
      if (!shape) continue;
      activateSegmentMotions(group, shape);
      replayed += 1;
    }
    return replayed;
  }

  function renderShapes() {
    for (const id of shapeTargetIds) targets.unregister(id);
    shapeTargetIds.clear();
    backLayer.replaceChildren(defs("back"));
    frontLayer.replaceChildren(defs("front"));
    const rendered = [];
    const segmentGroups = new Map();
    for (const item of painting.shapes || []) {
      const { shape } = item;
      const layer = isBoxForm(shape.form) ? backLayer : frontLayer;
      const group = document.createElementNS(SVG_NS, "g");
      group.dataset.id = shape.id;
      group.setAttribute(
        "class",
        `shape${shape.form === "frame" ? " frame-shape" : ""}${shape.form === "segment" ? " segment-shape" : ""}${isSelected(shape.id) ? " selected" : ""}`,
      );
      // Opacity and rotation belong to the whole shape, caption included, so
      // they go on the group rather than on each element.
      if (typeof shape.opacity === "number" && shape.opacity < 100) {
        group.setAttribute("opacity", String(Math.max(0, shape.opacity) / 100));
      }
      if (shape.angle && isSizedForm(shape.form)) {
        const cx = shape.x + shape.w / 2;
        const cy = shape.y + shape.h / 2;
        group.setAttribute("transform", `rotate(${(shape.angle * 180) / Math.PI} ${cx} ${cy})`);
      }
      for (const node of shapeElements(item)) group.append(node);
      if (shape.form === "segment") {
        group._atlasSegmentBody = group.querySelector(".segment-object");
      }
      rendered.push({ group, layer, shape });
      if (shape.form === "segment") segmentGroups.set(shape.id, group);
      shapeTargetIds.add(shape.id);
      targets.register({
        id: shape.id,
        label: shape.label || `${shape.form} ${shape.id}`,
        element: group,
        spatial: true,
        reveal: (options) => revealTarget(shape.id, options),
        project: () => projectAnchor(shape.id),
      });
    }
    for (const { group, layer, shape } of rendered) {
      const parent = shape.form === "segment" && shape.segment_parent_id
        ? segmentGroups.get(shape.segment_parent_id)
        : null;
      (parent || layer).append(group);
    }
    // A stored motion program is inert lesson data. It runs only through an
    // explicit replay action or human control, never merely because the page
    // rendered or reloaded.
    renderHandles();
  }

  function shapeElements(item) {
    const { shape, ends, path, outline } = item;
    const stroke = inkOf(shape.ink);
    const fill = shape.fill === "none" ? "none" : inkOf(shape.fill);
    const width = shape.stroke_width > 0 ? shape.stroke_width : DEFAULT_STROKE_WIDTH;
    const dash = dashArray(shape.stroke_style, width);
    const applyStroke = (node) => {
      node.setAttribute("stroke-width", String(width));
      if (dash) node.setAttribute("stroke-dasharray", dash);
      if (shape.stroke_style === "dotted") node.setAttribute("stroke-linecap", "round");
    };
    const out = [];

    if (shape.form === "segment") {
      const frame = document.createElementNS(SVG_NS, "foreignObject");
      frame.setAttribute("x", shape.x);
      frame.setAttribute("y", shape.y);
      frame.setAttribute("width", Math.max(shape.w, 1));
      frame.setAttribute("height", Math.max(shape.h, 1));
      frame.setAttribute("class", "segment-frame");
      const body = document.createElementNS(XHTML_NS, "div");
      const motionMatch = motionMatchFor(shape);
      const tapParent = shape.segment_parent_id
        ? atlas.shapes.find((candidate) => candidate.id === shape.segment_parent_id)
        : null;
      const groupTap = !motionMatch && tapParent?.segment_animation === "tap-parts";
      const tapSiblings = groupTap
        ? atlas.shapes
          .filter((candidate) => candidate.segment_parent_id === shape.segment_parent_id)
          .sort((left, right) => {
            const leftCenter = Array.isArray(left.segment_mask_source_box)
              ? (left.segment_mask_source_box[0] + left.segment_mask_source_box[2]) / 2
              : left.x;
            const rightCenter = Array.isArray(right.segment_mask_source_box)
              ? (right.segment_mask_source_box[0] + right.segment_mask_source_box[2]) / 2
              : right.x;
            return leftCenter - rightCenter || left.id.localeCompare(right.id);
          })
        : [];
      const animationClass = motionMatch
        ? " segment-custom-motion"
        : groupTap
        ? " segment-tap-part"
        : shape.segment_animation === "pulse"
          ? " segment-pulse"
          : shape.segment_animation === "rock"
            ? " segment-rock"
            : "";
      body.setAttribute("class", `segment-object${animationClass}`);
      if (motionMatch) {
        body._atlasMotion = motionMatch;
        body.dataset.motionOwner = motionMatch.owner.id;
        body.dataset.motionLabel = motionMatch.program.label;
        body.dataset.motionTrack = motionMatch.track.label;
        body.dataset.motionTargetIndex = String(motionMatch.index);
      }
      if (groupTap) {
        body.style.setProperty("--segment-tap-index", String(tapSiblings.findIndex((candidate) => candidate.id === shape.id)));
      }
      const origin = motionMatch?.track?.origin
        || [shape.segment_pivot_x, shape.segment_pivot_y];
      body.style.transformOrigin = `${origin[0] * 100}% ${origin[1] * 100}%`;
      body.dataset.segmentStatus = shape.segment_status;
      body.dataset.segmentId = shape.id;
      body.dataset.segmentParentId = shape.segment_parent_id || "";
      body.dataset.motionCompositing = motionMatch?.program?.child_compositing || "cutout";
      body.dataset.segmentPivot = `${shape.segment_pivot_x},${shape.segment_pivot_y}`;
      body.dataset.segmentSelected = String(isSelected(shape.id));
      const stage = document.createElementNS(XHTML_NS, "div");
      stage.setAttribute("class", "segment-stage");
      const bodyCanvas = document.createElementNS(XHTML_NS, "canvas");
      bodyCanvas.setAttribute("class", "segment-canvas segment-body-canvas");
      bodyCanvas.setAttribute("aria-label", `${shape.label} MobileSAM body mask`);
      const redundantSourceOverlay = Boolean(tapParent && preservesSegmentSource(tapParent));
      bodyCanvas.dataset.redundantSourceOverlay = String(redundantSourceOverlay);
      if (redundantSourceOverlay) bodyCanvas.style.opacity = "0";
      const wingCanvases = [];
      for (const part of Array.isArray(shape.segment_parts) ? shape.segment_parts : []) {
        const wing = document.createElementNS(XHTML_NS, "canvas");
        wing.setAttribute("class", "segment-canvas segment-wing-canvas");
        wing.setAttribute("aria-label", `${shape.label} ${part.role} MobileSAM mask`);
        wing.dataset.segmentRole = part.role;
        positionSegmentPart(wing, part, shape.segment_mask_source_box);
        wingCanvases.push([wing, part]);
        stage.append(wing);
      }
      stage.append(bodyCanvas);
      const badgeFrame = document.createElementNS(SVG_NS, "foreignObject");
      badgeFrame.setAttribute("x", shape.x);
      badgeFrame.setAttribute("y", shape.y - 30);
      badgeFrame.setAttribute("width", Math.max(shape.w, 1));
      badgeFrame.setAttribute("height", 30);
      badgeFrame.setAttribute("class", "segment-badge-frame");
      const badgeHolder = document.createElementNS(XHTML_NS, "div");
      badgeHolder.setAttribute("class", "segment-badge-holder");
      const badge = document.createElementNS(XHTML_NS, "span");
      badge.setAttribute("class", "segment-badge");
      badge.dataset.segmentSelected = String(isSelected(shape.id));
      badge.dataset.segmentStatus = shape.segment_status;
      badge.textContent = shape.segment_status === "materialized"
        ? `${shape.label} · g${shape.segment_generation}`
        : shape.segment_mask_runs
          ? `${shape.label} · refreshing g${shape.segment_generation}`
          : `${shape.label} · MobileSAM g${shape.segment_generation}`;
      badgeHolder.append(badge);
      badgeFrame.append(badgeHolder);
      body.append(stage);
      if (isSelected(shape.id)) {
        const pivot = document.createElementNS(XHTML_NS, "span");
        pivot.setAttribute("class", "segment-pivot-marker");
        pivot.setAttribute("aria-label", "Animation pivot");
        pivot.style.left = `${shape.segment_pivot_x * 100}%`;
        pivot.style.top = `${shape.segment_pivot_y * 100}%`;
        body.append(pivot);
      }
      frame.append(body);
      out.push(frame, badgeFrame);
      applySegmentFlapElement(body, Number(shape.segment_flap) || 0);
      if (shape.segment_mask_runs) {
        const preserveOwnerImage = preservesSegmentSource(shape);
        const childSegments = preserveOwnerImage ? [] : atlas.shapes
          .filter((candidate) => (
            candidate.form === "segment"
            && candidate.segment_parent_id === shape.id
            && candidate.segment_status === "materialized"
            && candidate.segment_mask_runs
          ))
          .map((candidate) => ({
            material: {
              mask_width: candidate.segment_mask_width,
              mask_height: candidate.segment_mask_height,
              mask_runs: candidate.segment_mask_runs,
              mask_source_box: candidate.segment_mask_source_box,
            },
            jointOrigin: motionMatchFor(candidate)?.track?.origin
              || [candidate.segment_pivot_x, candidate.segment_pivot_y],
          }));
        paintSegmentComposition(
          bodyCanvas,
          wingCanvases,
          shape,
          childSegments,
          preserveOwnerImage,
        );
      }
      return out;
    }

    // A plain text: no box, no stroke, just words on the page. Rendered
    // through <foreignObject> rather than <text> so it wraps, and so the
    // height it wraps to can be measured and written back, which is the only
    // way the agent's geometry is about the text that is actually there.
    if (shape.form === "text") {
      const frame = document.createElementNS(SVG_NS, "foreignObject");
      frame.setAttribute("x", shape.x);
      frame.setAttribute("y", shape.y);
      frame.setAttribute("width", shape.w);
      frame.setAttribute("height", Math.max(shape.h, 12));
      const body = document.createElementNS(XHTML_NS, "div");
      body.setAttribute("class", "shape-text");
      body.style.color = stroke;
      if (shape.font_size > 0) body.style.fontSize = `${shape.font_size}px`;
      body.textContent = shape.label; // never innerHTML: model-authored string
      frame.append(body);
      out.push(frame);
      return out;
    }

    if (shape.form === "ink") {
      // A filled outline, not a stroked polyline: the width varies along the
      // stroke, which a `stroke-width` cannot express.
      const path = document.createElementNS(SVG_NS, "path");
      path.setAttribute("d", polygonPath(outline));
      path.setAttribute("fill", stroke);
      path.setAttribute("stroke", "none");
      out.push(path);
    } else if (shape.form === "line" || shape.form === "arrow") {
      if (!ends) return out;
      const connector = path?.length >= 2 ? path : ends;
      const line = document.createElementNS(SVG_NS, "polyline");
      line.setAttribute("points", connector.map(([x, y]) => `${x},${y}`).join(" "));
      line.setAttribute("stroke", stroke);
      line.setAttribute("stroke-linecap", "round");
      line.setAttribute("stroke-linejoin", "round");
      line.setAttribute("fill", "none");
      applyStroke(line);
      out.push(line);
      if (shape.head !== "none") {
        out.push(arrowhead(connector[connector.length - 2], connector[connector.length - 1], shape.head, stroke));
      }
      if (shape.form === "arrow") {
        if (!shape.from) out.push(unboundEndMarker(ends[0], "start"));
        if (!shape.to) out.push(unboundEndMarker(ends[1], "end"));
      }
      // A 12px-wide invisible twin, so a 2px line can actually be clicked.
      const grab = line.cloneNode();
      grab.setAttribute("stroke", "transparent");
      grab.setAttribute("stroke-width", "12");
      grab.removeAttribute("stroke-dasharray");
      out.push(grab);
    } else {
      const [l, t] = [shape.x, shape.y];
      const [w, h] = [shape.w, shape.h];
      let node;
      if (shape.form === "rect" || shape.form === "frame") {
        node = document.createElementNS(SVG_NS, "rect");
        node.setAttribute("x", l);
        node.setAttribute("y", t);
        node.setAttribute("width", w);
        node.setAttribute("height", h);
        node.setAttribute("rx", shape.roundness === "sharp" ? "0" : shape.form === "frame" ? "4" : "10");
      } else if (shape.form === "ellipse") {
        node = document.createElementNS(SVG_NS, "ellipse");
        node.setAttribute("cx", l + w / 2);
        node.setAttribute("cy", t + h / 2);
        node.setAttribute("rx", w / 2);
        node.setAttribute("ry", h / 2);
      } else {
        node = document.createElementNS(SVG_NS, "polygon");
        node.setAttribute(
          "points",
          `${l + w / 2},${t} ${l + w},${t + h / 2} ${l + w / 2},${t + h} ${l},${t + h / 2}`,
        );
      }
      node.setAttribute("stroke", stroke);
      applyStroke(node);
      // `none`, not `transparent`. An unfilled region has to be grabbable by
      // its outline and transparent to the pointer everywhere else, or a box
      // drawn around three cards captures every click in the space between
      // them and you can no longer pan inside your own grouping. `transparent`
      // is a paint, so SVG hit-tests it; `none` is the absence of one.
      node.setAttribute("fill", fill === "none" ? "none" : fill);
      if (fill !== "none") node.setAttribute("fill-opacity", "0.12");
      out.push(node);
      if (fill === "none") {
        // …which leaves a 2px outline to aim at, so it gets the same wide
        // invisible twin a line gets.
        const grab = node.cloneNode();
        grab.setAttribute("stroke", "transparent");
        grab.setAttribute("stroke-width", "14");
        grab.removeAttribute("stroke-dasharray");
        out.push(grab);
      }
    }

    // A caption belongs to every form, not just the box ones. A labelled
    // arrow says what the relationship IS, and the model has always been able
    // to store that, only the renderer was refusing to show it, which made
    // typing a label onto an arrow look like the write had failed.
    if (shape.label && isBoxForm(shape.form) && shape.form !== "frame") {
      // A box's words live INSIDE the box and wrap to its width, the way a
      // bound text does in Excalidraw. A single unwrapped line on the top
      // edge ran into the neighbouring box the moment a label was longer
      // than the box, and left the box itself empty.
      const inset = 8;
      const frame = document.createElementNS(SVG_NS, "foreignObject");
      frame.setAttribute("x", shape.x + inset);
      frame.setAttribute("y", shape.y + inset);
      frame.setAttribute("width", Math.max(shape.w - inset * 2, 1));
      frame.setAttribute("height", Math.max(shape.h - inset * 2, 1));
      frame.setAttribute("class", "shape-box-label-frame");
      const body = document.createElementNS(XHTML_NS, "div");
      body.setAttribute("class", "shape-box-label");
      body.style.color = stroke;
      if (shape.font_size > 0) body.style.fontSize = `${shape.font_size}px`;
      const lines = shape.label.split("\n");
      const label = document.createElementNS(XHTML_NS, "span");
      label.textContent = lines[0];
      if (lines.length > 1) {
        const detail = document.createElementNS(XHTML_NS, "span");
        detail.setAttribute("class", "shape-label-detail");
        detail.textContent = `\n${lines.slice(1).join("\n")}`;
        label.append(detail);
      }
      body.append(label); // model-authored text never becomes markup
      frame.append(body);
      out.push(frame);
      return out;
    }
    if (shape.label) {
      const anchor = labelAnchor(shape, ends, path);
      const text = document.createElementNS(SVG_NS, "text");
      text.setAttribute("x", anchor.x);
      text.setAttribute("y", anchor.y);
      text.setAttribute("fill", stroke);
      text.setAttribute("font-size", String(shape.font_size > 0 ? shape.font_size : 12));
      text.setAttribute("font-weight", "600");
      if (shape.form === "frame") text.setAttribute("class", "frame-name");
      if (anchor.centered) text.setAttribute("text-anchor", "middle");
      text.textContent = shape.label; // never innerHTML: this string is model-authored
      out.push(text);
    }
    return out;
  }

  function unboundEndMarker([x, y], endpoint) {
    const marker = document.createElementNS(SVG_NS, "circle");
    marker.setAttribute("class", "unbound-end-marker");
    marker.setAttribute("cx", x);
    marker.setAttribute("cy", y);
    marker.dataset.endpoint = endpoint;
    marker.setAttribute("aria-label", `${endpoint} endpoint touches nothing`);
    marker.setAttribute("r", 5 / camera.scale);
    return marker;
  }

  function paintUnboundMarkers() {
    const radius = 5 / camera.scale;
    for (const marker of world.querySelectorAll(".unbound-end-marker")) {
      marker.setAttribute("r", radius);
    }
  }

  // Where a caption sits: inside the top-left of a region, above the middle of
  // a line, over the start of a stroke.
  function labelAnchor(shape, ends, path) {
    if ((shape.form === "line" || shape.form === "arrow") && ends) {
      if (path?.length > 2) {
        const lengths = path.slice(1).map((point, i) => Math.hypot(point[0] - path[i][0], point[1] - path[i][1]));
        let remaining = lengths.reduce((total, length) => total + length, 0) / 2;
        for (const [i, length] of lengths.entries()) {
          if (remaining <= length && length > 0) {
            const t = remaining / length;
            return {
              x: path[i][0] + t * (path[i + 1][0] - path[i][0]),
              y: path[i][1] + t * (path[i + 1][1] - path[i][1]) - 8,
              centered: true,
            };
          }
          remaining -= length;
        }
      }
      const [a, b] = ends;
      return { x: (a[0] + b[0]) / 2, y: (a[1] + b[1]) / 2 - 8, centered: true };
    }
    if (shape.form === "frame") {
      return { x: shape.x, y: shape.y - 8, centered: false };
    }
    if (isBoxForm(shape.form)) {
      return { x: shape.x + 12, y: shape.y + 20, centered: false };
    }
    const bounds = shapeBounds(shape, ends, path);
    return { x: bounds.x, y: bounds.y - 6, centered: false };
  }

  // The shape's box in world units.
  //
  // Derived here only for chrome the document does not describe, a caption
  // anchor, a handle position, a hit target. Everything that IS the drawing
  // still comes from `doc.painting()`; this never decides where a stroke is,
  // only where to put a grab dot relative to one.
  function shapeBounds(shape, ends, path) {
    if (isSizedForm(shape.form)) {
      return { x: shape.x, y: shape.y, w: shape.w, h: shape.h };
    }
    const points = path?.length
      ? path
      :
      (shape.form === "line" || shape.form === "arrow") && ends
        ? ends
        : (shape.points || []).map(([px, py]) => [shape.x + px, shape.y + py]);
    if (!points.length) return { x: shape.x, y: shape.y, w: 0, h: 0 };
    let [left, top, right, bottom] = [Infinity, Infinity, -Infinity, -Infinity];
    for (const [px, py] of points) {
      left = Math.min(left, px);
      top = Math.min(top, py);
      right = Math.max(right, px);
      bottom = Math.max(bottom, py);
    }
    return { x: left, y: top, w: right - left, h: bottom - top };
  }

  // ── handles ─────────────────────────────────────────────────────────
  //
  // What a shape can be *changed into* is a property of its form, so the
  // handles are derived from the form rather than configured. A box has
  // corners; a line has two ends; a text has one edge, because its height is
  // however many lines its words wrap to and dragging that would be authoring
  // a number the renderer is about to overwrite.

  function handlesFor(item) {
    const { shape, ends } = item;
    const box = shapeBounds(shape, ends, item.path);
    if (shape.form === "line" || shape.form === "arrow") {
      if (!ends) return [];
      return [
        { grip: "a", x: ends[0][0], y: ends[0][1], cursor: "crosshair" },
        { grip: "b", x: ends[1][0], y: ends[1][1], cursor: "crosshair" },
      ];
    }
    if (shape.form === "text") {
      return [{ grip: "e", x: box.x + box.w, y: box.y + box.h / 2, cursor: "ew-resize" }];
    }
    return [
      { grip: "nw", x: box.x, y: box.y, cursor: "nwse-resize" },
      { grip: "ne", x: box.x + box.w, y: box.y, cursor: "nesw-resize" },
      { grip: "se", x: box.x + box.w, y: box.y + box.h, cursor: "nwse-resize" },
      { grip: "sw", x: box.x, y: box.y + box.h, cursor: "nesw-resize" },
    ];
  }

  function renderHandles() {
    handleLayer.replaceChildren();
    if (active.tool !== "select") return; // mid-draw, handles are just clutter
    const item = (painting.shapes || []).find((entry) => entry.shape.id === selected);
    if (!item) return;
    // Handles are chrome, not content: they keep the same size on screen at
    // every zoom, which means their world size is the inverse of the camera.
    const size = HANDLE_PX / camera.scale;
    for (const handle of handlesFor(item)) {
      const dot = document.createElementNS(SVG_NS, "rect");
      dot.setAttribute("x", handle.x - size / 2);
      dot.setAttribute("y", handle.y - size / 2);
      dot.setAttribute("width", size);
      dot.setAttribute("height", size);
      dot.setAttribute("rx", size / 4);
      dot.setAttribute("class", "handle");
      dot.style.cursor = handle.cursor;
      dot.dataset.grip = handle.grip;
      dot.dataset.id = item.shape.id;
      handleLayer.append(dot);
    }
  }

  // Arrowheads are drawn as geometry rather than declared as an SVG <marker>.
  // A marker inherits the line's colour through `context-stroke`, which Safari
  // does not implement, and the fallback is a black triangle on a dark page.
  function arrowhead([ax, ay], [bx, by], kind, color) {
    const angle = Math.atan2(by - ay, bx - ax);
    if (kind === "dot") {
      const dot = document.createElementNS(SVG_NS, "circle");
      dot.setAttribute("cx", bx);
      dot.setAttribute("cy", by);
      dot.setAttribute("r", "4");
      dot.setAttribute("fill", color);
      return dot;
    }
    const size = 11;
    const spread = 0.42;
    const corner = (offset) => [
      bx - Math.cos(angle + offset) * size,
      by - Math.sin(angle + offset) * size,
    ];
    const head = document.createElementNS(SVG_NS, "polygon");
    const [lx, ly] = corner(spread);
    const [rx, ry] = corner(-spread);
    head.setAttribute("points", `${bx},${by} ${lx},${ly} ${rx},${ry}`);
    head.setAttribute("fill", color);
    return head;
  }

  function polygonPath(points) {
    if (!points || points.length < 3) return "";
    return `M${points.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join("L")}Z`;
  }

  // Present so each layer owns a well-formed <defs>; kept empty deliberately,
  // see the arrowhead note above.
  function defs(name) {
    const node = document.createElementNS(SVG_NS, "defs");
    node.dataset.layer = name;
    return node;
  }

  function groupMarksByTarget() {
    const marksByTarget = new Map();
    for (const mark of atlas.marks) {
      if (!marksByTarget.has(mark.target)) marksByTarget.set(mark.target, []);
      marksByTarget.get(mark.target).push(mark);
    }
    return marksByTarget;
  }

  function renderConstraintView(marksByTarget) {
    for (const id of constraintTargetElements.keys()) targets.unregister(id);
    constraintTargetElements.clear();
    renderConstraints(atlas, {
      diagStripHost: docks.get("bottom-strip"),
      backLayer: constraintBackLayer,
      foreLayer: constraintForeLayer,
      diagLayer,
      scene: world,
      boundsFor: constraintBounds,
      titleFor: objectTitle,
      marksByTarget,
      selected,
      selection,
      backConstraintsForward,
      onSelect: (id, event) => (event?.shiftKey ? toggleSelected(id) : select(id)),
      registerConstraint: (constraint, group) => {
        constraintTargetElements.set(constraint.id, group);
        targets.register({
          id: constraint.id,
          label: constraintStatement(constraint, objectTitle),
          element: group,
          spatial: true,
          reveal: (options) => revealTarget(constraint.id, options),
          project: () => projectAnchor(constraint.id),
        });
      },
    });
  }

  // ── interactive explanation flow ─────────────────────────────────
  //
  // The shared core owns the graph and cursor. This renderer owns only the
  // projection of the current beat and the local visual effects it names.
  // Expensive model work has already produced stable Atlas objects before a
  // flow is defined, so advancing a lesson never needs to load a model.

  let explanationSignature = "";
  let explanationPanelSignature = "";
  let explanationActionsExecuted = new Set();
  let explanationClockMode = "event";
  let explanationClock = { mode: "event", spokenChars: 0, status: "idle" };
  const explanationActionLog = [];
  const explanationInputEvents = [];
  const learnerViews = new Set(["lesson", "topics"]);
  const HIDDEN_TANGENTS_KEY = "agui.atlas.hidden-learning-tangents";
  const learnerCatalogUrl = new URLSearchParams(globalThis.location.search).get("lesson-catalog")
    || "/learner-catalog.json";
  let learnerView = learnerViewFromLocation();
  let learnerCatalog = { status: "idle", title: "Topics", topics: [], error: null };
  const tangentRequestLog = [];

  function learnerViewFromLocation() {
    const requested = new URLSearchParams(globalThis.location.search).get("learner-view") || "lesson";
    return learnerViews.has(requested) ? requested : "lesson";
  }

  function learnerTopic(raw, index) {
    if (!raw || typeof raw !== "object") throw new Error(`topic ${index + 1} is not an object`);
    const id = typeof raw.id === "string" ? raw.id.trim() : "";
    const title = typeof raw.title === "string" ? raw.title.trim() : "";
    const summary = typeof raw.summary === "string" ? raw.summary.trim() : "";
    const sourceId = typeof raw.source_id === "string" ? raw.source_id.trim() : "";
    if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(id)) throw new Error(`topic ${index + 1} has an invalid id`);
    if (!title || title.length > 100) throw new Error(`topic ${id} has an invalid title`);
    if (!summary || summary.length > 240) throw new Error(`topic ${id} has an invalid summary`);
    if (!sourceId || sourceId.length > 100) throw new Error(`topic ${id} has an invalid source_id`);
    let href;
    try {
      href = new URL(raw.href, globalThis.location.href);
    } catch {
      throw new Error(`topic ${id} has an invalid href`);
    }
    if (!["http:", "https:"].includes(href.protocol) || href.hostname !== globalThis.location.hostname) {
      throw new Error(`topic ${id} must stay on this lesson host`);
    }
    href.searchParams.set("atlas-mode", "learn");
    href.searchParams.delete("learner-view");
    if (learnerCatalogUrl !== "/learner-catalog.json") {
      href.searchParams.set("lesson-catalog", learnerCatalogUrl);
    }
    return { id, title, summary, sourceId, href: href.href };
  }

  function currentWorkspaceUrl() {
    const url = new URL(globalThis.location.href);
    for (const key of [
      "atlas-mode",
      "learner-view",
      "return-to",
      "remove-tangent",
      "workspace-view",
      "__atlas_trusted_navigation",
    ]) {
      url.searchParams.delete(key);
    }
    if (!learnerMode) url.searchParams.set("workspace-view", workspaceReturnView());
    return url;
  }

  function workspaceReturnView() {
    try {
      const stored = JSON.parse(localStorage.getItem("agui.atlas.workspace") || "{}");
      const candidate = stored.lastWorkActive || stored.active;
      return ["board", "inspector", "chat"].includes(candidate) ? candidate : "board";
    } catch {
      return "board";
    }
  }

  function learnerWorkspaceUrl({ removeId = "", openLearning = false } = {}) {
    const requested = new URLSearchParams(globalThis.location.search).get("return-to");
    let url = currentWorkspaceUrl();
    if (learnerMode && requested) {
      try {
        const candidate = new URL(requested, globalThis.location.href);
        if (["http:", "https:"].includes(candidate.protocol)
          && candidate.hostname === globalThis.location.hostname) {
          url = candidate;
        }
      } catch {
        // Invalid return targets fail closed to this host's ordinary workspace.
      }
    }
    url.searchParams.delete("atlas-mode");
    url.searchParams.delete("learner-view");
    url.searchParams.delete("return-to");
    if (removeId) url.searchParams.set("remove-tangent", removeId);
    if (openLearning) url.searchParams.set("workspace-view", "learn");
    return url;
  }

  function hiddenTangentIds() {
    try {
      const values = JSON.parse(localStorage.getItem(HIDDEN_TANGENTS_KEY) || "[]");
      return new Set(Array.isArray(values) ? values.filter((value) => typeof value === "string") : []);
    } catch {
      return new Set();
    }
  }

  function hideTangent(id) {
    const hidden = hiddenTangentIds();
    hidden.add(id);
    try {
      localStorage.setItem(HIDDEN_TANGENTS_KEY, JSON.stringify([...hidden]));
    } catch {
      // Storage denial changes persistence, not the ability to leave a refresher.
    }
  }

  function visibleWorkspaceTopics() {
    const hidden = hiddenTangentIds();
    return learnerCatalog.topics.filter((topic) => !hidden.has(topic.id));
  }

  async function ensureLearnerCatalog(force = false) {
    if (!force && ["loading", "ready"].includes(learnerCatalog.status)) return;
    learnerCatalog = { status: "loading", title: "Topics", topics: [], error: null };
    if (learnerView === "topics") renderExplanation();
    if (!learnerMode) renderLearningWorkspace();
    try {
      const response = await fetch(learnerCatalogUrl, { headers: { accept: "application/json" } });
      if (!response.ok) throw new Error(`topic catalog returned ${response.status}`);
      const payload = await response.json();
      if (payload?.schema !== LEARNER_CATALOG_SCHEMA) {
        throw new Error(`topic catalog schema must be ${LEARNER_CATALOG_SCHEMA}`);
      }
      if (!Array.isArray(payload.topics) || payload.topics.length < 1 || payload.topics.length > 24) {
        throw new Error("topic catalog must contain 1 to 24 topics");
      }
      const topics = payload.topics.map(learnerTopic);
      if (new Set(topics.map((topic) => topic.id)).size !== topics.length) {
        throw new Error("topic catalog contains duplicate ids");
      }
      const title = typeof payload.title === "string" && payload.title.trim()
        ? payload.title.trim().slice(0, 100)
        : "Choose a topic";
      learnerCatalog = { status: "ready", title, topics, error: null };
    } catch (error) {
      learnerCatalog = {
        status: "error",
        title: "Topics unavailable",
        topics: [],
        error: String(error?.message || error),
      };
    }
    explanationPanelSignature = "";
    if (learnerMode) renderExplanation();
    else renderLearningWorkspace();
  }

  function currentLearnerTopic() {
    const sourceId = new URLSearchParams(globalThis.location.search).get("segment-recipe") || "";
    return learnerCatalog.topics.find((topic) => topic.sourceId === sourceId) || null;
  }

  function learnerTopicLink(topic, label, className = "explanation-primary") {
    const link = element("a", className, label);
    const href = new URL(topic.href);
    href.searchParams.set("return-to", learnerWorkspaceUrl().href);
    link.href = href.href;
    link.dataset.learnerTopicId = topic.id;
    return link;
  }

  function learnerWorkspaceLink(label, options = {}, className = "explanation-control") {
    const link = element("a", className, label);
    link.href = learnerWorkspaceUrl(options).href;
    link.dataset.learnerNavigation = options.removeId ? "remove" : "workspace";
    return link;
  }

  function learnerNavigationButton(label, destination, className = "explanation-control") {
    const button = element("button", className, label);
    button.type = "button";
    button.dataset.learnerNavigation = destination;
    button.addEventListener("click", () => setLearnerView(destination));
    return button;
  }

  function renderLearnerTopics(flow) {
    const heading = element("header", "learner-destination-header");
    heading.append(
      element("span", "explanation-eyebrow", "TOPICS"),
      element("h2", "learner-destination-title", "Switch refreshers"),
      element("p", "learner-destination-copy", "Open another temporary refresher, or return to the workspace that started this tangent."),
    );
    if (learnerCatalog.status === "loading" || learnerCatalog.status === "idle") {
      learnerDestination.replaceChildren(heading, element("p", "learner-catalog-state", "Loading topics..."));
      void ensureLearnerCatalog();
      return;
    }
    if (learnerCatalog.status === "error") {
      const error = element("section", "learner-catalog-error");
      error.append(
        element("strong", null, "Topics could not be loaded."),
        element("p", null, learnerCatalog.error),
        learnerNavigationButton("Try again", "topics", "explanation-primary"),
      );
      error.querySelector("button")?.addEventListener("click", () => void ensureLearnerCatalog(true), { once: true });
      learnerDestination.replaceChildren(heading, error);
      return;
    }
    const current = currentLearnerTopic();
    const list = element("div", "learner-topic-grid");
    for (const topic of learnerCatalog.topics) {
      const card = element("article", "learner-topic-card");
      const isCurrent = current?.id === topic.id;
      if (isCurrent) {
        card.classList.add("current");
        card.append(element("span", "learner-topic-current", "CURRENT TOPIC"));
      }
      card.append(
        element("h3", "learner-topic-title", topic.title),
        element("p", "learner-topic-summary", topic.summary),
        learnerTopicLink(topic, isCurrent ? "Return to topic" : "Start topic"),
      );
      list.append(card);
    }
    const actions = element("div", "learner-destination-actions");
    actions.append(learnerWorkspaceLink("Back to workspace", {}, "explanation-primary"));
    if (flow) actions.append(learnerNavigationButton("Back to current refresher", "lesson"));
    learnerDestination.replaceChildren(heading, list, actions);
  }

  function renderLearningWorkspace() {
    if (!learningView || learnerMode) return;
    const header = element("section", "learning-workspace-intro");
    header.append(
      element("p", "learning-workspace-copy", "Ask a quick clarification or open a saved interactive refresher. Your canvas and conversation stay in place."),
    );

    const request = element("form", "learning-request-form");
    const label = element("label", "learning-request-label", "What do you need clarified?");
    const input = element("input", "learning-request-input");
    input.type = "text";
    input.name = "topic";
    input.required = true;
    input.maxLength = 180;
    input.placeholder = "For example, why this service owns retries";
    label.append(input);
    const submit = element("button", "explanation-primary", "Ask agent");
    submit.type = "submit";
    const status = element("p", "learning-request-status", "");
    status.setAttribute("role", "status");
    request.append(label, submit, status);
    request.addEventListener("submit", async (event) => {
      event.preventDefault();
      const topic = input.value.trim();
      if (!topic) return;
      if (!conversation?.ready) {
        status.textContent = "Connect an agent before asking for a clarification.";
        return;
      }
      submit.disabled = true;
      const interaction = {
        schema: "agui-direct-interaction-v1",
        kind: "request",
        source: "atlas.learning",
        control_id: "ask-for-clarification",
        label: "Ask agent",
        context: {
          topic,
          return_url: currentWorkspaceUrl().href,
          temporary: true,
        },
      };
      tangentRequestLog.push({ trusted: event.isTrusted === true, topic, ...interaction });
      const accepted = await conversation.ask(
        `Answer this as a focused clarification about ${topic}. Keep the current Atlas workspace unchanged so I can return to the main work immediately.`,
        { displayText: `Clarification: ${topic}`, interaction },
      );
      submit.disabled = false;
      if (!accepted) {
        status.textContent = "No agent accepted the refresher request.";
        return;
      }
      input.value = "";
      status.textContent = "Clarification sent. Your canvas remains unchanged.";
      document.querySelector('[data-dock-tab="chat"]')?.click();
    });

    const recents = element("section", "learning-recents");
    recents.append(element("h3", "learning-recents-title", "Saved refreshers"));
    if (["idle", "loading"].includes(learnerCatalog.status)) {
      recents.append(element("p", "learning-empty", "Loading…"));
      learningView.replaceChildren(header, request, recents);
      return;
    }
    if (learnerCatalog.status === "error") {
      recents.append(element("p", "learning-empty", "Saved refreshers are unavailable."));
      learningView.replaceChildren(header, request, recents);
      return;
    }
    const topics = visibleWorkspaceTopics();
    if (topics.length === 0) {
      recents.append(element("p", "learning-empty", "Nothing saved. Ask for a refresher when a tangent would help."));
    } else {
      const list = element("div", "learning-recent-list");
      for (const topic of topics) {
        const card = element("article", "learning-recent-card");
        const text = element("div", "learning-recent-text");
        text.append(
          element("h4", "learning-recent-name", topic.title),
          element("p", "learning-recent-summary", topic.summary),
        );
        const actions = element("div", "learning-recent-actions");
        const remove = element("button", "explanation-control quiet", "Remove");
        remove.type = "button";
        remove.dataset.removeTangent = topic.id;
        remove.addEventListener("click", () => {
          hideTangent(topic.id);
          renderLearningWorkspace();
        });
        actions.append(learnerTopicLink(topic, "Open refresher"), remove);
        card.append(text, actions);
        list.append(card);
      }
      recents.append(list);
    }
    learningView.replaceChildren(header, request, recents);
  }

  function renderLearnerDestination(flow) {
    atlasPane?.classList.add("learner-session-screen");
    learnerDestination.hidden = false;
    view.hidden = true;
    explanationPanel.hidden = true;
    learnerTopicsButton.setAttribute("aria-current", learnerView === "topics" ? "page" : "false");
    renderLearnerTopics(flow);
  }

  function restoreLearnerLessonSurface() {
    atlasPane?.classList.remove("learner-session-screen");
    learnerDestination.hidden = true;
    view.hidden = false;
    learnerTopicsButton.setAttribute("aria-current", "false");
  }

  function setLearnerView(next, options = {}) {
    if (!learnerMode || !learnerViews.has(next)) return false;
    const url = new URL(globalThis.location.href);
    if (next === "lesson") url.searchParams.delete("learner-view");
    else url.searchParams.set("learner-view", next);
    const method = options.replace === true ? "replaceState" : "pushState";
    history[method]({ ...(history.state || {}), atlasLearnerView: next }, "", url);
    learnerView = next;
    explanationPanelSignature = "";
    renderExplanation();
    if (next === "lesson") requestAnimationFrame(() => fitCameraToContent());
    return true;
  }

  learnerBackLink.href = learnerWorkspaceUrl().href;

  if (learnerMode) {
    globalThis.addEventListener("popstate", () => {
      learnerView = learnerViewFromLocation();
      explanationPanelSignature = "";
      renderExplanation();
      if (learnerView === "lesson") requestAnimationFrame(() => fitCameraToContent());
    });
  }
  if (!learnerMode) {
    const params = new URLSearchParams(globalThis.location.search);
    const removeId = params.get("remove-tangent") || "";
    if (/^[a-z0-9][a-z0-9-]{0,63}$/.test(removeId)) hideTangent(removeId);
    const requestedWorkspaceView = removeId ? "learn" : params.get("workspace-view");
    if (["board", "inspector", "chat", "learn"].includes(requestedWorkspaceView)) {
      queueMicrotask(() => document.querySelector(
        `[data-dock-tab="${CSS.escape(requestedWorkspaceView)}"]`,
      )?.click());
      const clean = new URL(globalThis.location.href);
      clean.searchParams.delete("workspace-view");
      clean.searchParams.delete("remove-tangent");
      history.replaceState(history.state, "", clean);
    }
  }
  void ensureLearnerCatalog();

  function explanationTargetElement(id) {
    return targetElement(id)
      || constraintTargetElements.get(id)
      || view.querySelector(`g.shape[data-id="${CSS.escape(id)}"]`);
  }

  function clearExplanationHighlights() {
    clearWalkthroughFocus();
    for (const { animation, overlay } of explanationTraces.values()) {
      animation?.cancel();
      overlay.remove();
    }
    explanationTraces.clear();
    for (const target of view.querySelectorAll(".explanation-target-active, .explanation-target-attention")) {
      target.classList.remove(
        "explanation-target-active",
        "explanation-target-reveal",
        "explanation-target-attention",
      );
    }
  }

  const explanationTraces = new Map();
  function traceExplanationPath(id, repeat) {
    const target = explanationTargetElement(id);
    const path = target?.querySelector("polyline");
    if (!path) return "missing-path";
    const length = path.getTotalLength();
    if (!(length > 0)) return "empty-path";
    const overlay = path.cloneNode(false);
    overlay.classList.add("explanation-path-trace");
    overlay.setAttribute("aria-hidden", "true");
    overlay.setAttribute("stroke", "var(--accent)");
    overlay.setAttribute("stroke-width", "6");
    overlay.setAttribute("pointer-events", "none");
    overlay.removeAttribute("stroke-dasharray");
    target.append(overlay);
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
      explanationTraces.set(id, { overlay, animation: null });
      return "static-direction-reduced-motion";
    }
    const dash = Math.max(8, length * .08);
    overlay.setAttribute("stroke-dasharray", `${dash} ${length + dash}`);
    const animation = overlay.animate(
      [{ strokeDashoffset: String(dash) }, { strokeDashoffset: String(-length) }],
      { duration: 2400, iterations: repeat ? Infinity : 1, fill: "forwards", easing: "linear" },
    );
    explanationTraces.set(id, { overlay, animation });
    return repeat ? "tracing-until-advance" : "tracing-forward-once";
  }

  function drawExplanationAttention(target) {
    if (!target || globalThis.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches) return;
    target.classList.remove("explanation-target-attention");
    // Restart the bounded cue when a new beat points at the same object. The
    // class is never restored by a rerender, so pause and resume cannot loop it.
    void target.getBoundingClientRect();
    target.classList.add("explanation-target-attention");
    target.addEventListener(
      "animationend",
      () => target.classList.remove("explanation-target-attention"),
      { once: true },
    );
  }

  function revealActiveWalkthroughStop() {
    const strip = explanationPlayer.querySelector(".walkthrough-stops");
    const current = strip?.querySelector('[aria-current="step"]');
    if (!current) return;
    const box = strip.getBoundingClientRect();
    const active = current.getBoundingClientRect();
    if (active.left < box.left || active.right > box.right) strip.scrollLeft += active.left - box.left;
  }

  function walkthroughStops(beat) {
    const evidence = beat.evidence || [];
    const candidates = [...(evidence.length ? evidence : (beat.actions || []).filter(action => ["point", "reveal"].includes(action.kind)))];
    if (walkthroughExtraStop?.beat === beat.id) candidates.push(walkthroughExtraStop);
    const seen = new Set();
    return candidates.filter(item => {
      if (seen.has(item.target_id) || !documentHasTarget(item.target_id)) return false;
      seen.add(item.target_id);
      return true;
    });
  }

  function walkthroughChoices(beat, targetId) {
    const stops = walkthroughStops(beat);
    const next = stops[stops.findIndex(item => item.target_id === targetId) + 1]?.target_id;
    return (atlas.edges || []).filter(edge => edge.from === targetId && documentHasTarget(edge.to) && inExplorationView(edge.to))
      .sort((a, b) => Number(b.to === next) - Number(a.to === next));
  }

  function focusWalkthroughStop(flow, beat, id) {
    if (!documentHasTarget(id)) return;
    const destination = walkthroughStops(beat).some(item => item.target_id === id) ? beat
      : flow.definition.beats.find(candidate => walkthroughStops(candidate).some(item => item.target_id === id)) || beat;
    // A real connection can lead outside an authored evidence list. Expose
    // that graph object locally without modifying the authored walkthrough.
    if (!walkthroughStops(destination).some(item => item.target_id === id)) {
      walkthroughExtraStop = { beat: destination.id, target_id: id, detail: atlas.nodes.find(node => node.id === id)?.note || objectTitle(id) };
    }
    if (walkthroughTarget !== id || beat.id !== destination.id) walkthroughHistory.push({ beat: beat.id, target: walkthroughTarget });
    previewExplanation(destination.id, id, true);
  }

  const walkthroughPaths = new Map();
  const walkthroughMarker = element("div", "walkthrough-focus-frame");
  walkthroughMarker.setAttribute("aria-hidden", "true");
  walkthroughMarker.append(element("span", "walkthrough-focus-label", "YOU ARE HERE"));
  let walkthroughFocusSignature = "";
  function clearWalkthroughFocus() {
    for (const path of walkthroughPaths.values()) path.remove();
    walkthroughPaths.clear();
    walkthroughMarker.remove();
    walkthroughFocusSignature = "";
    for (const target of view.querySelectorAll(".walkthrough-current, .walkthrough-destination")) target.classList.remove("walkthrough-current", "walkthrough-destination");
  }

  function renderWalkthroughFocus(beat, preserveAuthored = false) {
    const choices = walkthroughChoices(beat, walkthroughTarget);
    const signature = `${walkthroughTarget}:${choices.map(edge => edge.id).join(":")}`;
    if (signature !== walkthroughFocusSignature) {
      if (preserveAuthored) clearWalkthroughFocus();
      else clearExplanationHighlights();
      walkthroughFocusSignature = signature;
    }
    const current = explanationTargetElement(walkthroughTarget);
    current?.classList.add("walkthrough-current", "explanation-target-active");
    const bounds = worldBounds(walkthroughTarget);
    if (bounds) {
      walkthroughMarker.style.left = `${bounds.x - 6}px`;
      walkthroughMarker.style.top = `${bounds.y - 6}px`;
      walkthroughMarker.style.width = `${bounds.w + 12}px`;
      walkthroughMarker.style.height = `${bounds.h + 12}px`;
      if (!walkthroughMarker.isConnected) world.append(walkthroughMarker);
    }
    for (const edge of choices) {
      explanationTargetElement(edge.to)?.classList.add("walkthrough-destination");
      const source = edgeLayer.querySelector(`[data-edge-id="${CSS.escape(edge.id)}"]`);
      if (!source) continue;
      let overlay = walkthroughPaths.get(edge.id);
      if (!overlay) {
        overlay = source.cloneNode(false);
        overlay.removeAttribute("class");
        overlay.removeAttribute("data-edge-id");
        overlay.classList.add("walkthrough-flow-path");
        overlay.dataset.walkthroughEdge = edge.id;
        overlay.setAttribute("aria-hidden", "true");
        overlay.setAttribute("pointer-events", "none");
        walkthroughPaths.set(edge.id, overlay);
      }
      for (const attribute of ["d", "points"]) {
        if (source.hasAttribute(attribute)) overlay.setAttribute(attribute, source.getAttribute(attribute));
      }
      if (!overlay.isConnected) edgeLayer.append(overlay);
    }
  }

  function executeExplanationActions(flow, beat) {
    if (!learnerMode && openedExploration && explorationOverview) {
      atlasPane?.classList.remove("has-walkthrough-focus");
      clearExplanationHighlights();
      explanationSignature = "";
      return;
    }
    const localWalkthrough = !learnerMode && openedExploration && walkthroughTarget && !explorationEditing;
    atlasPane?.classList.toggle("has-walkthrough-focus", Boolean(localWalkthrough));
    const graphWalkthrough = localWalkthrough
      && (atlas.edges || []).some(edge => edge.from === walkthroughTarget || edge.to === walkthroughTarget)
      && !beat.actions.some(action => action.kind === "replay-motion");
    if (graphWalkthrough) {
      renderWalkthroughFocus(beat);
      return;
    }
    if (!localWalkthrough) clearWalkthroughFocus();
    const signature = `${flow.definition.id}:${flow.state.entered_revision}:${beat.id}`;
    if (signature !== explanationSignature) {
      explanationSignature = signature;
      explanationActionsExecuted = new Set();
      explanationClock = { mode: explanationClockMode, spokenChars: 0, status: "entered" };
      clearExplanationHighlights();
    }
    for (const [id, { animation, overlay }] of explanationTraces) {
      const target = explanationTargetElement(id);
      const path = target?.querySelector("polyline");
      if (path) overlay.setAttribute("points", path.getAttribute("points"));
      if (target && overlay.parentNode !== target) target.append(overlay);
      if (!animation) continue;
      if (flow.state.status === "completed" && animation.effect.getTiming().iterations === Infinity) {
        animation.effect.updateTiming({ iterations: 1 });
        animation.finish();
      }
      if (flow.state.status === "paused" && animation.playState === "running") animation.pause();
      else if (flow.state.status === "active" && animation.playState === "paused") animation.play();
      else if (flow.state.status === "stopped") animation.cancel();
    }
    if (flow.state.status === "stopped") {
      clearExplanationHighlights();
      return;
    }
    if (flow.state.status === "paused") return;
    for (const [index, action] of beat.actions.entries()) {
      const actionKey = `${signature}:${index}`;
      if (explanationActionsExecuted.has(actionKey)) {
        const target = explanationTargetElement(action.target_id);
        const trace = explanationTraces.get(action.target_id);
        if (target && trace && action.kind === "trace-path") {
          const path = target.querySelector("polyline");
          if (path) trace.overlay.setAttribute("points", path.getAttribute("points"));
          if (trace.overlay.parentNode !== target) target.append(trace.overlay);
        }
        if (target && (action.kind === "point" || action.kind === "reveal")) {
          target.classList.add("explanation-target-active");
          if (action.kind === "reveal") target.classList.add("explanation-target-reveal");
        }
        continue;
      }
      const cueStart = Array.isArray(action.cue_span) ? Number(action.cue_span[0]) : null;
      if (explanationClock.mode === "speech"
        && Number.isFinite(cueStart)
        && explanationClock.spokenChars < cueStart) continue;
      const target = explanationTargetElement(action.target_id);
      let result = "missing-target";
      if (action.kind === "point" && target) {
        target.classList.add("explanation-target-active");
        drawExplanationAttention(target);
        result = "highlighted";
      } else if (action.kind === "reveal" && target) {
        target.classList.add("explanation-target-active", "explanation-target-reveal");
        drawExplanationAttention(target);
        result = revealTarget(action.target_id, { initiator: "agent" })
          ? "revealed-or-already-legible"
          : "camera-declined";
      } else if (action.kind === "trace-path") {
        result = traceExplanationPath(action.target_id, flow.state.status === "active");
      } else if (action.kind === "replay-motion") {
        const replayed = replaySegmentMotion(action.target_id);
        result = replayed > 0 ? `replayed-${replayed}` : "no-rendered-motion";
      }
      explanationActionsExecuted.add(actionKey);
      explanationActionLog.push({
        flowId: flow.definition.id,
        revision: flow.state.revision,
        beatId: beat.id,
        kind: action.kind,
        targetId: action.target_id,
        clock: explanationClock.mode,
        cueSpan: action.cue_span || null,
        result,
        at: performance.now(),
      });
    }
    if (localWalkthrough) renderWalkthroughFocus(beat, true);
  }

  function advanceExplanation(flow, transition, event, options = {}) {
    const selectedIds = options.selectedTargetIds || [...selection];
    const next = write(() => doc.advance_explanation(
      flow.definition.id,
      flow.state.revision,
      transition.id,
      transition.label,
      JSON.stringify(selectedIds),
    ));
    if (!next) return null;
    const input = {
      trusted: event?.isTrusted === true,
      type: "transition",
      source: options.source || "transition-control",
      flowId: flow.definition.id,
      revision: flow.state.revision,
      transitionId: transition.id,
      response: transition.label,
      selectedTargetIds: selectedIds,
      hostCommitted: false,
      agentAccepted: false,
    };
    explanationInputEvents.push(input);
    return { state: JSON.parse(next), input };
  }

  function transitionIsDirect(beat, transition) {
    return beat.advance.mode === "choice" || transition.control === "button";
  }

  function directTransitionForTarget(beat, targetId) {
    const matches = beat.advance.transitions.filter((transition) => (
      transitionIsDirect(beat, transition) && transition.target_ids.includes(targetId)
    ));
    return matches.length === 1 ? matches[0] : null;
  }

  async function waitForHostExplanationCommit(flowId, revision, transitionId) {
    const deadline = performance.now() + 5_000;
    while (performance.now() < deadline) {
      try {
        const response = await fetch("/atlas/describe", {
          headers: { accept: "application/json" },
          cache: "no-store",
        });
        const payload = await response.json();
        const text = payload.ok ? payload.text || "" : "";
        const block = text.split("\nEXPLANATION FLOW\n").find((part) => part.startsWith(`- [${flowId}] `));
        if (block?.split("\n")[0].includes(`revision=${revision} `)
          && block.includes(`LAST INPUT: transition=${transitionId} `)) return true;
      } catch {
        /* the replica can still converge on the next poll */
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    return false;
  }

  async function submitDirectExplanation(flow, beat, transition, event, options = {}) {
    for (const control of explanationPanel.querySelectorAll("button")) control.disabled = true;
    const result = advanceExplanation(flow, transition, event, options);
    if (!result) {
      for (const control of explanationPanel.querySelectorAll("button")) control.disabled = false;
      return;
    }
    const committed = await waitForHostExplanationCommit(
      flow.definition.id,
      result.state.revision,
      transition.id,
    );
    result.input.hostCommitted = committed;
    if (!committed) {
      report("Your answer was saved, but the agent replica did not confirm it yet.");
      return;
    }
    const interaction = {
      schema: "agui-direct-interaction-v1",
      kind: "choice",
      source: "atlas.explanation",
      control_id: transition.id,
      label: transition.label,
      context: {
        flow_id: flow.definition.id,
        beat_id: beat.id,
        previous_revision: flow.state.revision,
        committed_revision: result.state.revision,
        transition_id: transition.id,
        selected_target_ids: result.input.selectedTargetIds,
        input_source: result.input.source,
        state_change_committed: true,
      },
    };
    const accepted = await conversation?.ask?.(transition.label, {
      displayText: transition.label,
      interaction,
    });
    result.input.agentAccepted = accepted?.ok === true;
  }

  function controlExplanation(flow, command, event) {
    explanationInputEvents.push({
      trusted: event?.isTrusted === true,
      type: "control",
      flowId: flow.definition.id,
      revision: flow.state.revision,
      command,
    });
    write(() => doc.control_explanation(
      flow.definition.id,
      flow.state.revision,
      command,
    ));
  }

  function openExplanationComposer() {
    document.querySelector('[data-dock-tab="chat"]')?.click();
    queueMicrotask(() => conversation?.ui?.input?.focus());
  }

  function learnerResponseForm(flow, beat) {
    const form = element("form", "explanation-response-form");
    const label = element("label", "explanation-response-label", "Your explanation");
    const input = element("textarea", "explanation-response-input");
    input.name = "response";
    input.rows = 3;
    input.required = true;
    input.placeholder = "Explain what the visual shows…";
    label.append(input);
    const actions = element("div", "explanation-response-actions");
    const status = element("span", "explanation-response-status", "");
    status.setAttribute("role", "status");
    const send = element("button", "explanation-primary", "Send answer");
    send.type = "submit";
    actions.append(status, send);
    form.append(label, actions);
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const response = input.value.trim();
      if (!response) return;
      input.disabled = true;
      send.disabled = true;
      status.textContent = "Sending…";
      const accepted = await conversation?.ask?.(response);
      if (!accepted) {
        input.disabled = false;
        send.disabled = false;
        status.textContent = "Not sent. Try again.";
        input.focus();
        return;
      }
      explanationInputEvents.push({
        trusted: event.isTrusted === true,
        type: "response",
        source: "learner-inline-response",
        flowId: flow.definition.id,
        revision: flow.state.revision,
        beatId: beat.id,
        response,
        agentAccepted: true,
      });
      status.textContent = "Answer sent";
    });
    return form;
  }

  function focusLearnerTarget(id, publish) {
    const target = explanationTargetElement(id);
    if (!target) return false;
    target.classList.add("explanation-target-active");
    drawExplanationAttention(target);
    selectSet([id], publish);
    return true;
  }

  function explorationFlows() {
    return atlas.explanations || (atlas.explanation ? [atlas.explanation] : []);
  }

  function inExplorationView(id) {
    if (learnerMode) return true;
    if (focusVisible) return focusVisible.has(id);
    const flows = explorationFlows();
    const flow = flows.find((item) => item.definition.id === openedExploration);
    if (flow?.definition.scene_ids?.length) return flow.definition.scene_ids.includes(id);
    // Unscoped tours retain the original board without leaking other views'
    // private diagram objects into it.
    return !flows.some((item) => item.definition.scene_ids?.includes(id));
  }

  document.addEventListener("atlas-search-target", (event) => {
    const id = event.detail?.id;
    if (event.detail?.exploration) { openExploration(id); return; }
    if (!id || !documentHasTarget(id)) return;
    if (!inExplorationView(id)) {
      const owner = explorationFlows().find(flow => flow.definition.scene_ids?.includes(id));
      if (owner) openExploration(owner.definition.id);
      else if (openedExploration) closeExploration();
    }
    selectSet([id], true);
    requestAnimationFrame(() => revealTarget(id, { initiator: "human" }));
  });

  let presentationView = null;
  document.addEventListener("atlas-presentation", event => {
    if (event.detail?.enabled) {
      presentationView = { camera: atlasCamera.state(), overview: explorationOverview };
      explorationOverview = true;
      requestAnimationFrame(() => fitCameraToContent(true));
    } else if (presentationView) {
      const saved = presentationView;
      presentationView = null;
      explorationOverview = saved.overview;
      requestAnimationFrame(() => {
        atlasCamera.adopt(saved.camera);
        animateCamera();
        fitButton.textContent = openedExploration ? explorationOverview ? "Focus step" : "Whole diagram" : "Fit";
      });
    }
  });

  function openExploration(id) {
    if (!explorationFlows().some((flow) => flow.definition.id === id)) return;
    pendingLocalView = null;
    if (focusedObject) objectFocusTrail.push({ id: focusedObject, exploration: openedExploration, camera: atlasCamera.state() });
    focusedObject = null; focusVisible = null;
    if (!openedExploration) {
      blueprintCamera = atlasCamera.state();
      blueprintViewport = { width: view.clientWidth, height: view.clientHeight };
    }
    openedExploration = id;
    explorationPreview = null;
    walkthroughTarget = null;
    walkthroughHistory = [];
    walkthroughExtraStop = null;
    explorationEditing = false;
    explorationOverview = true;
    saveLocalView();
    atlasPane?.classList.remove("exploration-editing");
    document.dispatchEvent(new CustomEvent("atlas-exploration-view", { detail: { open: true } }));
    explorationLibrary.classList.remove("show-directory");
    explanationPanelSignature = "";
    cameraTouched = true;
    selectSet([], true);
    render();
    requestAnimationFrame(() => fitCameraToContent(true));
  }

  function closeExploration() {
    focusedObject = null; focusVisible = null; objectFocusTrail.length = 0;
    openedExploration = null;
    explorationPreview = null;
    walkthroughTarget = null;
    walkthroughHistory = [];
    walkthroughExtraStop = null;
    pendingLocalView = null;
    saveLocalView();
    document.dispatchEvent(new CustomEvent("atlas-exploration-view", { detail: { open: false } }));
    explanationPanelSignature = "";
    clearExplanationHighlights();
    selectSet([], true);
    render();
    fitButton.textContent = "Fit";
    fitButton.title = "Fit the whole board";
    const viewportChanged = blueprintViewport && (Math.abs(view.clientWidth - blueprintViewport.width) > 80
      || Math.abs(view.clientHeight - blueprintViewport.height) > 120);
    if (blueprintCamera && !viewportChanged) {
      atlasCamera.adopt(blueprintCamera);
      animateCamera();
    } else {
      // The shared core chooses a legible opening for the viewport we have.
      // Restoring offscreen desktop coordinates on a phone loses the diagram.
      cameraTouched = false;
      fittedExtent = "";
      fitCameraToContent();
      cameraTouched = true;
    }
    blueprintCamera = null;
    blueprintViewport = null;
    explorationNav.querySelector("button")?.focus();
  }

  function openObjectFocus(id) {
    if (!atlas.nodes.some(node => node.id === id)) return;
    if (focusedObject === id) { document.dispatchEvent(new CustomEvent("atlas-open-inspector")); return; }
    objectFocusTrail.push({ id: focusedObject, exploration: openedExploration, camera: atlasCamera.state() });
    focusedObject = id; focusVisible = focusGraph(atlas, id)?.visible || null;
    openedExploration = null;
    saveLocalView();
    clearExplanationHighlights();
    // The selected component opens its contents, even if its parent was collapsed.
    for (const member of focusGraph(atlas, id)?.inside || []) collapsedContainers.delete(member);
    explorationNavSignature = ""; explanationPanelSignature = "";
    cameraTouched = true; selectSet([id], true); render();
    requestAnimationFrame(() => fitCameraToContent(true));
  }

  function backFromObject() {
    const previous = objectFocusTrail.pop();
    if (!previous) { closeExploration(); return; }
    focusedObject = previous.id; openedExploration = previous.exploration;
    focusVisible = focusedObject ? focusGraph(atlas, focusedObject)?.visible || null : null;
    explorationNavSignature = ""; explanationPanelSignature = "";
    saveLocalView();
    selectSet(focusedObject ? [focusedObject] : [], true); render();
    atlasCamera.adopt(previous.camera); animateCamera();
  }

  document.addEventListener("atlas-request-decision-draft", event => {
    const ids = event.detail?.ids || [];
    selectSet(ids, true); restoreComposerAnchor();
    document.querySelector('[data-dock-tab="chat"]')?.click();
    if (!conversation?.ui?.input) { report("Discussion is still connecting."); return; }
    const input = conversation.ui.input;
    const request = `Prepare a complete decision proposal for ${ids.map(objectTitle).join(" + ")} with atlas_decision_draft. Fill the title, decision, rationale, alternatives, tradeoffs, consequences, exact enforceable G8 checks, and uncovered requirements. Leave only review and confirmation for me.`;
    input.value = input.value.trim() ? `${input.value}\n\n${request}` : request;
    input.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
    queueMicrotask(() => input.focus());
  });

  function requestObjectDiagram(id, kind) {
    selectSet([id], true); restoreComposerAnchor();
    document.querySelector('[data-dock-tab="chat"]')?.click();
    if (!conversation?.ui?.input) { report("Discussion is still connecting. Try again when it is ready."); return; }
    const input = conversation.ui.input;
    const request = `Create a detailed ${kind} diagram for ${objectTitle(id)}. Link it to this object as a separate view, use the project evidence, and label anything still unknown.`;
    // Keep an existing unsent thought. The user chooses when to send it.
    input.value = input.value.trim() ? `${input.value}\n\n${request}` : request;
    input.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
    queueMicrotask(() => input.focus());
  }

  function renderExplorationNavigation() {
    if (learnerMode) return;
    const flows = explorationFlows();
    if (pendingLocalView?.focus && atlas.nodes.some(node => node.id === pendingLocalView.focus)) {
      const id = pendingLocalView.focus; pendingLocalView = null;
      requestAnimationFrame(() => openObjectFocus(id));
    }
    const restore = pendingLocalView && flows.find(flow => flow.definition.id === pendingLocalView.id);
    if (restore) {
      const saved = pendingLocalView;
      pendingLocalView = null;
      requestAnimationFrame(() => {
        openExploration(saved.id);
        if (saved.beat && restore.definition.beats.some(beat => beat.id === saved.beat)) previewExplanation(saved.beat, saved.target);
        explorationOverview = saved.overview !== false;
        renderExplanation();
      });
    }
    if (openedExploration && !flows.some((flow) => flow.definition.id === openedExploration)) {
      openedExploration = null;
    }
    for (const target of world.querySelectorAll("[data-id]")) {
      target.classList.toggle("exploration-outside", !inExplorationView(target.dataset.id));
    }
    const current = flows.find((flow) => flow.definition.id === openedExploration);
    atlasPane?.classList.toggle("in-exploration", Boolean(current));
    const signature = JSON.stringify([focusedObject, openedExploration, selection, flows.map((flow) => [flow.definition, flow.state.revision]), atlas.marks, atlas.decision_drafts]);
    if (signature === explorationNavSignature) return;
    explorationNavSignature = signature;
    const focusedInput = explorationLibrary.contains(document.activeElement) && document.activeElement.matches("textarea")
      ? { start: document.activeElement.selectionStart, end: document.activeElement.selectionEnd } : null;
    const home = element("button", "exploration-home", focusedObject || objectFocusTrail.length ? "← Back" : current ? "← Overview" : "Overview");
    home.type = "button";
    home.addEventListener("click", () => focusedObject || objectFocusTrail.length ? backFromObject() : closeExploration());
    const heading = element("div", "exploration-heading");
    heading.append(element("strong", null, focusedObject ? objectTitle(focusedObject) : current?.definition.title || "System architecture"));
    if (focusedObject) {
      const trail = element("span", "focus-breadcrumb", "Overview");
      const parent = atlas.nodes.find(node => node.id === focusedObject)?.parent;
      if (parent) trail.textContent += ` / ${objectTitle(parent)}`;
      heading.prepend(trail);
      const detail = element("span", "focus-kind", isContainer(focusedObject) ? "Contents & connections" : "Connections"); heading.append(detail);
    }
    heading.append(element("span", null, current
      ? `Exploring ${current.definition.about?.map(objectTitle).join(" + ") || "this workspace"}`
      : "Components and relationships. Open an exploration to examine a question."));
    const syncState = document.getElementById("sync-state");
    if (syncState) heading.append(syncState);
    const browse = element("button", "exploration-browse", "Diagrams ▾");
    browse.type = "button";
    browse.setAttribute("aria-expanded", "false");
    const viewMenu = element("div", "exploration-view-menu");
    viewMenu.setAttribute("popover", "auto");
    viewMenu.setAttribute("aria-label", "Choose a view");
    const offeredViews = focusedObject ? linkedDetailViews(atlas, focusedObject) : flows;
    if (focusedObject) {
      viewMenu.append(element("p", "view-menu-label", `Views of ${objectTitle(focusedObject)}`));
      const connections = element("button", "exploration-view-option", "Contents & connections"); connections.type = "button";
      connections.setAttribute("aria-current", "true"); connections.addEventListener("click", () => viewMenu.hidePopover()); viewMenu.append(connections);
    }
    for (const flow of offeredViews) {
      const option = element("button", "exploration-view-option");
      option.type = "button";
      option.append(element("strong", null, flow.definition.title), element("span", null, `${flow.definition.beats.length} step${flow.definition.beats.length === 1 ? "" : "s"}`));
      option.setAttribute("aria-current", String(flow.definition.id === openedExploration));
      option.addEventListener("click", () => { viewMenu.hidePopover(); openExploration(flow.definition.id); });
      viewMenu.append(option);
    }
    if (focusedObject) {
      if (!offeredViews.length) viewMenu.append(element("p", "view-menu-note", "No separate diagrams are linked yet. Ask for a view based on project evidence."));
      for (const kind of ["state", "permissions", "implementation"]) {
        const request = element("button", "exploration-view-option view-request", `Ask for ${kind === "implementation" ? "an" : "a"} ${kind} diagram`); request.type = "button";
        request.addEventListener("click", () => { viewMenu.hidePopover(); requestObjectDiagram(focusedObject, kind); }); viewMenu.append(request);
      }
    }
    browse.addEventListener("click", () => {
      viewMenu.togglePopover();
      const anchor = browse.getBoundingClientRect();
      viewMenu.style.top = `${anchor.bottom + 8}px`;
      viewMenu.style.left = `${Math.max(12, Math.min(innerWidth - 312, anchor.right - 300))}px`;
    });
    viewMenu.addEventListener("toggle", () => browse.setAttribute("aria-expanded", String(viewMenu.matches(":popover-open"))));
    const navActions = element("div", "exploration-nav-actions");
    if (current) {
      const questionsVisible = !document.querySelector(".workspace-dock")?.classList.contains("collapsed")
        && document.querySelector('[data-dock-tab="guide"]')?.getAttribute("aria-selected") === "true";
      const questions = element("button", "exploration-discuss", questionsVisible ? "Close questions" : "Questions");
      questions.type = "button";
      questions.setAttribute("aria-expanded", String(questionsVisible));
      questions.addEventListener("click", () => {
        const dock = document.querySelector(".workspace-dock");
        if (!dock?.classList.contains("collapsed") && document.querySelector('[data-dock-tab="guide"]')?.getAttribute("aria-selected") === "true") {
          document.querySelector('[data-collapse="dock"]')?.click();
        } else openExplorationQuestions();
      });
      const edit = element("button", "exploration-edit", explorationEditing ? "Done editing" : "Edit canvas");
      edit.type = "button";
      edit.setAttribute("aria-pressed", String(explorationEditing));
      edit.addEventListener("click", () => {
        explorationEditing = !explorationEditing;
        atlasPane?.classList.toggle("exploration-editing", explorationEditing);
        explorationNavSignature = "";
        renderExplorationNavigation();
        fitCameraToContent(true);
      });
      navActions.append(questions, edit);
    }
    if (flows.length || focusedObject) navActions.append(browse, viewMenu);
    const help = document.querySelector(".canvas-help");
    if (help) viewOptionsPanel.append(help);
    const find = element("button", "workspace-find", "Search");
    find.type = "button";
    find.title = "Find a node or drawing by name (⌘K)";
    find.addEventListener("click", () => document.dispatchEvent(new CustomEvent("atlas-open-search")));
    navActions.prepend(find);
    if (selected) {
      const inspect = element("button", "selection-inspect", "Details"); inspect.type = "button";
      inspect.title = "Inspect selected object, claims, and provenance";
      inspect.addEventListener("click", () => document.dispatchEvent(new CustomEvent("atlas-open-inspector")));
      navActions.prepend(inspect);
      const drafts = (atlas.decision_drafts || []).filter(draft => draft.input.ids.length === selection.length && draft.input.ids.every(id => selection.includes(id)));
      if (drafts.length) {
        const review = element("button", "selection-review", "Review decision"); review.type = "button";
        review.addEventListener("click", () => openDecisionReview([...selection], ctx)); navActions.prepend(review);
      }
    }
    explorationNav.replaceChildren(home, heading, navActions);
    atlasPane?.classList.toggle("object-focused", Boolean(focusedObject));
    home.hidden = !current && !focusedObject && !objectFocusTrail.length;
    guidedViews.hidden = true;
    const guideLabel = element("div", "guided-views-label");
    guideLabel.append(element("span", null, "EXPLORE"));
    const guideList = element("div", "guided-views-list");
    for (const flow of flows) {
      const button = element("button", "guided-view");
      button.type = "button";
      button.dataset.explorationId = flow.definition.id;
      button.title = flow.definition.goal;
      const label = element("span", "guided-view-label");
      label.append(element("strong", null, flow.definition.title));
      button.append(label, element("span", "guided-view-arrow", "↗"));
      button.addEventListener("click", () => openExploration(flow.definition.id));
      guideList.append(button);
    }
    guidedViews.replaceChildren(guideLabel, guideList);
    explorationLibrary.classList.toggle("in-room", Boolean(current));
    const title = element("h2", null, current ? "Questions & understanding" : "Explorations");
    const intro = element("p", "muted", current?.definition.goal || "Questions have their own visual workspace. The blueprint stays separate.");
    explorationLibrary.replaceChildren(title, intro);
    if (current) {
      const back = element("button", "exploration-return", "Back to diagram ↑");
      back.type = "button";
      back.addEventListener("click", () => {
        document.querySelector('[data-collapse="dock"]')?.click();
        atlasPane.scrollIntoView({ block: "start" });
      });
      explorationLibrary.prepend(back);
    }
    const related = flows.filter((flow) => !selected || flow.definition.about?.includes(selected) || flow.definition.scene_ids?.includes(selected));
    for (const flow of related) {
      const entry = element("button", "exploration-entry");
      entry.type = "button";
      entry.dataset.explorationId = flow.definition.id;
      entry.setAttribute("aria-current", String(flow.definition.id === openedExploration));
      entry.append(element("strong", null, flow.definition.title));
      entry.append(element("span", null, flow.definition.goal));
      entry.append(element("small", null, `${flow.definition.beats.length} step${flow.definition.beats.length === 1 ? "" : "s"} · ${flow.definition.about?.map(objectTitle).join(" + ") || "Workspace"} →`));
      entry.addEventListener("click", () => openExploration(flow.definition.id));
      explorationLibrary.append(entry);
    }
    if (!related.length) explorationLibrary.append(element("p", "muted", selected
      ? "No visual explorations linked to this selection yet. Ask about it to begin."
      : "No visual explorations yet. Start with a question in Discuss."));
    if (selected) {
      const all = element("button", "exploration-show-all", "Show all explorations");
      all.type = "button";
      all.addEventListener("click", () => { selectSet([], true); renderExplorationNavigation(); });
      explorationLibrary.append(all);
    }
    if (current) {
      const questions = element("section", "exploration-questions");
      const ids = [...new Set([...(current.definition.scene_ids || []), ...(current.definition.about || [])])];
      const beat = current.definition.beats.find(item => item.id === (explorationPreview || current.state.current_beat));
      const targetId = beat?.evidence[0]?.target_id || ids[0];
      if (targetId) {
        const form = element("form", "exploration-question-form");
        const label = element("label", null, `About ${beat?.title || current.definition.title}`);
        const input = element("textarea", "exploration-question-input");
        input.rows = 3;
        input.required = true;
        input.placeholder = "What needs a closer look?";
        const draftKey = `${current.definition.id}:${beat?.id || "room"}`;
        input.value = explorationDrafts.get(draftKey) || "";
        input.addEventListener("input", () => explorationDrafts.set(draftKey, input.value));
        label.append(input);
        const actions = element("div", "exploration-question-actions");
        const kind = element("select", "exploration-question-kind");
        kind.setAttribute("aria-label", "Question or challenge");
        for (const [value, text] of [["?", "Question"], ["!", "Challenge"]]) {
          const option = element("option", null, text); option.value = value; kind.append(option);
        }
        kind.value = explorationDrafts.get(`${draftKey}:kind`) || "?";
        const send = element("button", "explanation-primary", "Post question");
        send.type = "submit";
        const updateKind = () => { send.textContent = kind.value === "!" ? "Post challenge" : "Post question"; explorationDrafts.set(`${draftKey}:kind`, kind.value); };
        kind.addEventListener("change", updateKind);
        updateKind();
        const result = element("p", "exploration-question-result");
        result.setAttribute("role", "status");
        actions.append(kind, send);
        form.append(label, actions, result);
        form.addEventListener("submit", event => {
          event.preventDefault();
          const text = input.value.trim();
          if (!text) return;
          const id = markTarget(targetId, kind.value, text);
          if (!id) { result.textContent = "Not saved. Check the replica connection and try again."; return; }
          explorationDrafts.delete(draftKey);
          input.value = "";
          result.textContent = "Saved to this exploration. Awaiting an agent response.";
        });
        questions.append(form);
      }
      const marks = atlas.marks.filter(mark => ids.includes(mark.target));
      const pending = marks.filter(mark => !mark.answer);
      questions.append(element("h3", null, pending.length ? `Awaiting response · ${pending.length}` : "No unanswered questions"));
      for (const mark of pending) questions.append(explorationMarkRow(mark));
      const answered = marks.filter(mark => mark.answer);
      if (answered.length) {
        const history = inspectorDisclosure(`exploration-history:${current.definition.id}`, `Answered questions · ${answered.length}`, []);
        history.classList.add("exploration-history");
        // Repeated questions retain every stored mark, without repeating the same prose.
        const groups = new Map();
        for (const mark of answered) {
          const key = JSON.stringify([mark.glyph, mark.text, mark.answer]);
          if (!groups.has(key)) groups.set(key, []);
          groups.get(key).push(mark);
        }
        for (const group of groups.values()) history.append(explorationMarkRow(group[0], group));
        questions.append(history);
      }
      explorationLibrary.append(questions);
    }
    for (const [id, entry] of nodeElements) {
      entry.root.querySelector(".node-explorations")?.remove();
      entry.root.querySelector(".node-exploration-menu")?.remove();
      if (id === focusedObject) continue;
      const linked = linkedDetailViews(atlas, id);
      const button = element("button", "node-explorations", linked.length ? `Explore · ${linked.length} view${linked.length === 1 ? "" : "s"}` : "Explore →");
      button.type = "button"; button.title = `Explore ${objectTitle(id)}. Double-click the card for its contents and connections.`;
      button.setAttribute("aria-label", `Explore ${objectTitle(id)}`);
      button.addEventListener("pointerdown", event => event.stopPropagation());
      button.addEventListener("dblclick", event => event.stopPropagation());
      button.addEventListener("click", event => { event.stopPropagation(); openObjectFocus(id); });
      entry.root.querySelector(".node-head").append(button);
    }
    if (focusedInput) {
      const input = explorationLibrary.querySelector("textarea");
      input?.focus({ preventScroll: true });
      input?.setSelectionRange(focusedInput.start, focusedInput.end);
    }
  }

  function explorationMarkRow(mark, instances = [mark]) {
    const row = element("article", "mark-row exploration-mark");
    row.dataset.markId = mark.id;
    row.append(element("small", "muted", `${mark.glyph === "!" ? "Challenge" : "Question"} · ${objectTitle(mark.target)}`));
    row.append(element("p", null, mark.text));
    if (mark.answer) row.append(inspectorDisclosure(`answer:${mark.id}`, "Read response", [element("p", "answer", mark.answer)]));
    if (instances.length > 1) {
      const repeated = inspectorDisclosure(`instances:${mark.id}`, `${instances.length} matching questions retained`, []);
      for (const instance of instances) repeated.append(element("p", "muted", `${objectTitle(instance.target)} · ${instance.id}`));
      row.append(repeated);
    }
    return row;
  }

  function openExplorationQuestions(challenge = false) {
    explorationLibrary.classList.remove("show-directory");
    document.querySelector('[data-dock-tab="guide"]')?.click();
    const kind = explorationLibrary.querySelector(".exploration-question-kind");
    if (kind) { kind.value = challenge ? "!" : "?"; kind.dispatchEvent(new Event("change")); }
    explorationLibrary.querySelector("textarea")?.focus({ preventScroll: true });
    if (matchMedia("(max-width: 960px)").matches) explorationLibrary.scrollIntoView({ block: "start" });
  }

  function renderExplanation() {
    const flow = atlas.explanation;
    const empty = document.querySelector(".guide-empty");
    if (empty) empty.hidden = !learnerMode || Boolean(flow);
    if (learnerMode && learnerView !== "lesson") {
      renderLearnerDestination(flow);
      return;
    }
    if (learnerMode) restoreLearnerLessonSurface();
    if (!flow || (!learnerMode && !openedExploration)) {
      view.classList.remove("has-explanation-flow");
      atlasPane?.classList.remove("has-explanation-flow");
      explanationPanel.hidden = true;
      explanationPlayer.hidden = true;
      explanationPlayer.replaceChildren();
      explanationPanel.replaceChildren();
      explanationSignature = "";
      explanationPanelSignature = "";
      explanationActionsExecuted.clear();
      clearExplanationHighlights();
      return;
    }
    view.classList.add("has-explanation-flow");
    atlasPane?.classList.add("has-explanation-flow");
    const { definition, state } = flow;
    const previewing = !learnerMode && explorationPreview && explorationPreview !== state.current_beat;
    const beatIndex = definition.beats.findIndex((candidate) => candidate.id === (previewing ? explorationPreview : state.current_beat));
    const beat = definition.beats[beatIndex];
    if (!beat) {
      explanationPlayer.hidden = true;
      explanationPanel.hidden = false;
      explanationPanel.replaceChildren(
        element("strong", "explanation-error", "This explanation points to a missing beat."),
      );
      return;
    }

    if (!learnerMode && !walkthroughStops(beat).some(item => item.target_id === walkthroughTarget)) {
      walkthroughTarget = walkthroughStops(beat)[0]?.target_id || null;
    }
    const panelSignature = `${definition.id}:${state.revision}:${state.status}:${beat.id}:${Boolean(previewing)}:${walkthroughTarget}`;
    if (panelSignature === explanationPanelSignature && explanationPanel.childElementCount > 0) {
      executeExplanationActions(flow, beat);
      return;
    }

    explanationPanel.hidden = false;
    explanationPanel.dataset.status = state.status;
    explanationPanel.dataset.flowId = definition.id;
    explanationPanel.dataset.beatId = beat.id;
    explanationPanel.dataset.revision = String(state.revision);

    const header = element("header", "explanation-header");
    const headingText = element("div", "explanation-heading-text");
    headingText.append(
      element("span", "explanation-eyebrow", "GUIDED EXPLANATION"),
      element("h2", "explanation-title", definition.title),
    );
    const progress = element(
      "span",
      "explanation-progress",
      `${Math.max(1, beatIndex + 1)} / ${definition.beats.length}`,
    );
    header.append(headingText, progress);

    const statusRow = element("div", "explanation-status-row");
    statusRow.append(
      element("span", `explanation-status ${state.status}`, state.status.toUpperCase()),
      element("span", "explanation-clock", `${explanationClock.mode} clock · revision ${state.revision}`),
    );

    const beatTitle = element("h3", "explanation-beat-title", beat.title);
    const cue = element("p", "explanation-cue", beat.cue);
    const intent = element("p", "explanation-intent", beat.intent);
    intent.prepend(element("span", null, "WHY THIS STEP  "));

    const evidence = element("section", "explanation-evidence");
    evidence.append(element("h4", null, "LOOK FOR THIS"));
    const evidenceList = element("ul", "explanation-evidence-list");
    for (const item of beat.evidence) {
      const row = element("li", "explanation-evidence-item");
      const target = element("button", "explanation-target-button", objectTitle(item.target_id));
      target.type = "button";
      target.dataset.targetId = item.target_id;
      const directTransition = directTransitionForTarget(beat, item.target_id);
      if (directTransition) {
        target.classList.add("explanation-answer-target");
        target.title = `Answer ${directTransition.label}`;
        target.setAttribute("aria-label", `Answer ${directTransition.label}`);
      } else {
        target.title = `Show ${objectTitle(item.target_id)} on the canvas`;
      }
      target.addEventListener("click", (event) => {
        if (!documentHasTarget(item.target_id)) return;
        if (learnerMode) focusLearnerTarget(item.target_id, !directTransition);
        else {
          if (!inExplorationView(item.target_id)) closeExploration();
          focusWalkthroughStop(flow, beat, item.target_id);
        }
        if (directTransition) {
          void submitDirectExplanation(flow, beat, directTransition, event, {
            source: "evidence-target",
            selectedTargetIds: [item.target_id],
          });
        }
      });
      row.append(target, element("span", null, item.detail));
      evidenceList.append(row);
    }
    evidence.append(evidenceList);

    const advance = element("section", "explanation-advance");
    if (beat.advance.prompt) {
      advance.append(element("p", "explanation-prompt", beat.advance.prompt));
    }
    const transitionRow = element("div", "explanation-transitions");
    if (state.status === "active" && beat.advance.mode === "continue") {
      const transition = beat.advance.transitions[0];
      const control = element("button", "explanation-primary", transition.label);
      control.type = "button";
      control.addEventListener("click", (event) => advanceExplanation(flow, transition, event));
      transitionRow.append(control);
    } else if (state.status === "active" && beat.advance.mode === "choice") {
      for (const transition of beat.advance.transitions) {
        const control = element("button", "explanation-choice", transition.label);
        control.type = "button";
        control.addEventListener("click", (event) => void submitDirectExplanation(
          flow,
          beat,
          transition,
          event,
          { source: "answer-button", selectedTargetIds: [] },
        ));
        transitionRow.append(control);
      }
    } else if (state.status === "active" && beat.advance.mode === "agent") {
      const directTransitions = beat.advance.transitions.filter((transition) => (
        transitionIsDirect(beat, transition)
      ));
      for (const transition of directTransitions) {
        const control = element("button", "explanation-choice explanation-direct-answer", transition.label);
        control.type = "button";
        control.dataset.transitionId = transition.id;
        control.addEventListener("click", (event) => void submitDirectExplanation(
          flow,
          beat,
          transition,
          event,
          { source: "answer-button", selectedTargetIds: [] },
        ));
        transitionRow.append(control);
      }
      if (directTransitions.length === 0 || directTransitions.length < beat.advance.transitions.length) {
        if (learnerMode) {
          transitionRow.append(learnerResponseForm(flow, beat));
        } else {
          const answer = element(
            "button",
            directTransitions.length ? "explanation-control" : "explanation-primary",
            directTransitions.length ? "Explain another answer" : "Write an explanation",
          );
          answer.type = "button";
          answer.addEventListener("click", openExplanationComposer);
          transitionRow.append(answer);
        }
      }
    }
    advance.append(transitionRow);

    const controls = element("div", "explanation-controls");
    if (state.status !== "paused" && state.status !== "stopped") {
      const replay = element("button", "explanation-control", "Replay step");
      replay.type = "button";
      replay.dataset.explanationReplay = "true";
      replay.addEventListener("click", () => {
        explanationSignature = "";
        clearWalkthroughFocus();
        executeExplanationActions(flow, beat);
      });
      controls.append(replay);
    }
    if (!learnerMode) {
      const discuss = element("button", "explanation-control", "Ask about this step");
      discuss.type = "button";
      discuss.addEventListener("click", () => {
        openExplorationQuestions();
      });
      controls.append(discuss);
      const challenge = element("button", "explanation-control", "Challenge this step");
      challenge.type = "button";
      challenge.addEventListener("click", () => {
        openExplorationQuestions(true);
      });
      controls.append(challenge);
    }
    if (state.status === "active") {
      const pause = element("button", "explanation-control", "Pause");
      pause.type = "button";
      pause.dataset.explanationCommand = "pause";
      pause.addEventListener("click", (event) => controlExplanation(flow, "pause", event));
      controls.append(pause);
    } else if (state.status === "paused") {
      const resume = element("button", "explanation-control", "Resume");
      resume.type = "button";
      resume.dataset.explanationCommand = "resume";
      resume.addEventListener("click", (event) => controlExplanation(flow, "resume", event));
      controls.append(resume);
    }
    if (["paused", "completed", "stopped"].includes(state.status)) {
      const restart = element("button", "explanation-control", "Restart");
      restart.type = "button";
      restart.dataset.explanationCommand = "restart";
      restart.addEventListener("click", (event) => controlExplanation(flow, "restart", event));
      controls.append(restart);
    }
    if (["active", "paused"].includes(state.status)) {
      const stop = element(
        "button",
        "explanation-control quiet",
        learnerMode ? "End explanation" : "Stop",
      );
      stop.type = "button";
      stop.dataset.explanationCommand = "stop";
      stop.addEventListener("click", (event) => controlExplanation(flow, "stop", event));
      controls.append(stop);
    }

    const lessonNavigation = element("nav", "explanation-lesson-navigation");
    lessonNavigation.setAttribute("aria-label", "Leave or replace this refresher");
    if (learnerMode) {
      const current = currentLearnerTopic();
      lessonNavigation.append(element("span", "explanation-navigation-label", "RETURN TO WORK"));
      lessonNavigation.append(
        learnerWorkspaceLink("Back to workspace", {}, "explanation-primary"),
        learnerNavigationButton("Switch refresher", "topics"),
      );
      if (current) {
        lessonNavigation.append(learnerWorkspaceLink(
          "Remove refresher",
          { removeId: current.id, openLearning: true },
          "explanation-control quiet danger",
        ));
      }
    }

    explanationPanel.replaceChildren(
      header,
      statusRow,
      beatTitle,
      cue,
      intent,
      evidence,
      advance,
      controls,
      ...(learnerMode ? [lessonNavigation] : []),
    );
    if (!learnerMode) {
      explanationPanel.hidden = true;
      const summary = element("div", "explanation-player-summary");
      summary.setAttribute("aria-live", "polite");
      summary.append(progress, beatTitle, cue);
      const actions = element("div", "explanation-player-actions");
      const stops = walkthroughStops(beat);
      const stopIndex = stops.findIndex(item => item.target_id === walkthroughTarget);
      const currentStop = stops[stopIndex];
      const choices = walkthroughChoices(beat, walkthroughTarget);
      const current = element("div", "walkthrough-current-summary");
      current.setAttribute("aria-live", "polite");
      current.append(
        element("span", "walkthrough-position", currentStop ? `ON THE CANVAS · ${stopIndex + 1} OF ${stops.length}` : "ON THE CANVAS"),
        element("strong", "walkthrough-object-title", currentStop ? objectTitle(walkthroughTarget) : beat.title),
        element("p", "walkthrough-object-detail", atlas.nodes.find(node => node.id === walkthroughTarget)?.note || currentStop?.detail || beat.cue),
      );
      const stopNavigation = element("nav", "walkthrough-stops");
      stopNavigation.setAttribute("aria-label", "Objects in this section");
      for (const [index, item] of stops.entries()) {
        const control = element("button", "walkthrough-stop");
        control.type = "button";
        control.dataset.targetId = item.target_id;
        control.append(element("span", "walkthrough-stop-number", String(index + 1)), element("span", null, objectTitle(item.target_id)));
        if (item.target_id === walkthroughTarget) control.setAttribute("aria-current", "step");
        control.addEventListener("click", () => {
          focusWalkthroughStop(flow, beat, item.target_id);
          explanationPlayer.querySelector(`.walkthrough-stop[data-target-id="${CSS.escape(item.target_id)}"]`)?.focus({ preventScroll: true });
        });
        stopNavigation.append(control);
      }
      const localNavigation = element("nav", "walkthrough-navigation");
      localNavigation.setAttribute("aria-label", "Follow the flow on your screen");
      const previousStep = element("button", "walkthrough-previous", "Previous");
      previousStep.type = "button";
      previousStep.disabled = walkthroughHistory.length === 0;
      previousStep.addEventListener("click", () => {
        const previous = walkthroughHistory.pop();
        if (previous) previewExplanation(previous.beat, previous.target, true);
        explanationPlayer.querySelector(".walkthrough-previous:not(:disabled), .walkthrough-next")?.focus({ preventScroll: true });
      });
      localNavigation.append(previousStep);
      const destinations = element("div", "walkthrough-destinations");
      destinations.append(element("span", "walkthrough-destinations-label", choices.length > 1 ? "CHOOSE A PATH" : choices.length ? "FOLLOW THE CONNECTION" : "END OF THIS PATH"));
      if (choices.length) {
        for (const [index, edge] of choices.entries()) {
          const next = element("button", index === 0 ? "walkthrough-next" : "walkthrough-branch");
          next.type = "button";
          next.dataset.targetId = edge.to;
          next.dataset.edgeId = edge.id;
          next.append(element("span", null, `${index === 0 ? "Next: " : ""}${objectTitle(edge.to)}`), element("small", null, edge.label || "Follow connection"), element("span", "walkthrough-direction", "→"));
          next.addEventListener("click", () => {
            focusWalkthroughStop(flow, beat, edge.to);
            explanationPlayer.querySelector(".walkthrough-next")?.focus({ preventScroll: true });
          });
          destinations.append(next);
        }
      } else {
        const upcoming = stops[stopIndex + 1];
        const nextBeat = definition.beats[(beatIndex + 1) % definition.beats.length];
        const next = element("button", "walkthrough-next", upcoming ? `Next object: ${objectTitle(upcoming.target_id)}` : `Next section: ${nextBeat.title}`);
        next.type = "button";
        next.addEventListener("click", () => {
          if (upcoming) focusWalkthroughStop(flow, beat, upcoming.target_id);
          else {
            walkthroughHistory.push({ beat: beat.id, target: walkthroughTarget });
            previewExplanation(nextBeat.id, null, true);
          }
          explanationPlayer.querySelector(".walkthrough-next")?.focus({ preventScroll: true });
        });
        destinations.append(next);
      }
      localNavigation.append(destinations);
      if (previewing) {
        progress.textContent = `Review · ${beatIndex + 1} / ${definition.beats.length}`;
        const follow = element("button", "explanation-primary exploration-follow", "Return to shared step");
        follow.type = "button";
        follow.addEventListener("click", () => previewExplanation(null));
        actions.append(follow);
      }
      const primaryControls = element("div", "explanation-primary-controls");
      const more = element("div", "explanation-more");
      const moreButton = element("button", "explanation-control", previewing ? "Step options" : "Shared controls");
      moreButton.type = "button";
      moreButton.setAttribute("aria-expanded", "false");
      more.append(moreButton);
      const moreItems = element("div", "explanation-more-items");
      moreItems.setAttribute("popover", "auto");
      moreButton.addEventListener("click", () => {
        moreItems.togglePopover();
        if (moreItems.matches(":popover-open")) {
          const anchor = moreButton.getBoundingClientRect();
          moreItems.style.left = `${Math.max(8, Math.min(innerWidth - moreItems.offsetWidth - 8, anchor.right - moreItems.offsetWidth))}px`;
          moreItems.style.top = `${Math.max(8, anchor.top - moreItems.offsetHeight - 8)}px`;
        }
      });
      moreItems.addEventListener("toggle", () => moreButton.setAttribute("aria-expanded", String(moreItems.matches(":popover-open"))));
      if (!previewing) {
        moreItems.append(element("p", "shared-control-note", "Advance, pause, restart, and stop affect everyone on this page."));
        if (beat.advance.mode === "continue") moreItems.append(advance);
        else actions.append(advance);
      }
      for (const control of [...controls.children]) {
        if (control.textContent === "Ask about this step") primaryControls.append(control);
        else if (!previewing || !control.dataset.explanationCommand) moreItems.append(control);
      }
      more.append(moreItems);
      const focus = element("button", "explanation-control", "Focus on this step");
      focus.type = "button";
      focus.addEventListener("click", () => {
        const id = walkthroughTarget || beat.actions.find(action => action.kind === "point")?.target_id || beat.evidence[0]?.target_id;
        if (id && inExplorationView(id)) revealTarget(id, { initiator: "human" });
        moreItems.hidePopover();
      });
      moreItems.prepend(focus);
      actions.append(primaryControls, more);
      const steps = element("ol", "exploration-steps");
      steps.setAttribute("aria-label", "Walkthrough sections");
      for (const [index, item] of definition.beats.entries()) {
        const step = element("li");
        const control = element("button", "exploration-step", item.title.replace(/^\d+\s*[·.]\s*/, ""));
        control.type = "button";
        control.dataset.beatId = item.id;
        control.prepend(element("span", "exploration-step-number", String(index + 1).padStart(2, "0")));
        control.title = item.id === state.current_beat ? "Follow the shared step" : "Review this step without changing the shared walkthrough";
        control.addEventListener("click", () => previewExplanation(item.id === state.current_beat ? null : item.id));
        step.append(control);
        if (index === beatIndex) step.setAttribute("aria-current", "step");
        steps.append(step);
      }
      const sectionLabel = element("div", "walkthrough-label");
      sectionLabel.append(element("span", null, "WALKTHROUGH SECTIONS"), element("span", "walkthrough-state", previewing ? "Reviewing locally" : "At shared step"));
      const evidenceDetails = element("details", "walkthrough-evidence");
      evidenceDetails.append(element("summary", null, "Section context and evidence"), summary, evidence);
      const strip = element("div", "walkthrough-strip");
      const minimize = element("button", "walkthrough-toggle", walkthroughCollapsed ? "Expand walkthrough" : "Minimize walkthrough");
      minimize.type = "button"; minimize.setAttribute("aria-expanded", String(!walkthroughCollapsed));
      minimize.setAttribute("aria-controls", "walkthrough-expanded-content");
      minimize.addEventListener("click", () => setWalkthroughCollapsed(!walkthroughCollapsed));
      const positionLabel = element("span", "walkthrough-strip-position", `${beatIndex + 1}/${definition.beats.length}`);
      const stripTitle = element("strong", "walkthrough-strip-title", currentStop && objectTitle(walkthroughTarget) !== beat.title ? `${beat.title} · ${objectTitle(walkthroughTarget)}` : beat.title);
      strip.append(positionLabel, stripTitle, minimize);
      const expanded = element("div", "walkthrough-expanded-content"); expanded.id = "walkthrough-expanded-content";
      stopNavigation.hidden = stops.length < 2;
      current.replaceChildren(element("p", "walkthrough-object-detail", beat.cue));
      expanded.append(steps, stopNavigation, current, actions, evidenceDetails);
      explanationPlayer.replaceChildren(strip, localNavigation, expanded);
      explanationPlayer.classList.toggle("is-collapsed", walkthroughCollapsed);
      explanationPlayer.hidden = false;
      explanationPlayer.dataset.beatId = beat.id;
      explanationPlayer.dataset.targetId = walkthroughTarget || "";
      revealActiveWalkthroughStop();
      explanationPlayer.dataset.status = previewing ? "reviewing" : state.status;
    }
    explanationPanelSignature = panelSignature;
    executeExplanationActions(flow, beat);
    if (!learnerMode && !explorationOverview && !explorationEditing) fitCameraToContent(true, true);
  }

  function previewExplanation(beatId, targetId = null, keepHistory = false) {
    explorationOverview = false;
    explorationPreview = beatId;
    walkthroughTarget = targetId;
    if (!keepHistory) {
      walkthroughHistory = [];
      walkthroughExtraStop = null;
    }
    const flow = explorationFlows().find(item => item.definition.id === openedExploration);
    const beat = flow?.definition.beats.find(item => item.id === (beatId || flow.state.current_beat));
    if (beat && targetId && documentHasTarget(targetId) && inExplorationView(targetId)
      && !walkthroughStops(beat).some(item => item.target_id === targetId)) {
      walkthroughExtraStop = { beat: beat.id, target_id: targetId, detail: atlas.nodes.find(node => node.id === targetId)?.note || objectTitle(targetId) };
    }
    saveLocalView();
    selectSet(targetId ? [targetId] : [], true);
    explanationPanelSignature = "";
    explanationSignature = "";
    explorationNavSignature = "";
    clearExplanationHighlights();
    renderExplorationNavigation();
    renderExplanation();
    explanationPlayer.scrollTop = 0;
  }

  function setExplanationClockMode(mode) {
    if (mode !== "event" && mode !== "speech") {
      throw new Error("explanation clock mode must be event or speech");
    }
    explanationClockMode = mode;
    explanationClock = { ...explanationClock, mode };
    return { ...explanationClock };
  }

  // Future browser TTS plugs in here. Audio timing stays outside the durable
  // graph: it reports a spoken character cursor, which releases only actions
  // whose optional cue span has begun. Interruption never advances the beat.
  function reportExplanationCueProgress(update) {
    const flow = atlas.explanation;
    if (!flow) throw new Error("the atlas has no explanation flow");
    if (update?.flowId !== flow.definition.id || Number(update?.revision) !== flow.state.revision) {
      throw new Error("speech progress does not match the current explanation revision");
    }
    const spokenChars = Number(update?.spokenChars);
    if (!Number.isFinite(spokenChars) || spokenChars < 0) {
      throw new Error("spokenChars must be a non-negative number");
    }
    explanationClockMode = "speech";
    explanationClock = {
      mode: "speech",
      spokenChars,
      status: typeof update?.status === "string" ? update.status : "playing",
      alignment: typeof update?.alignment === "string" ? update.alignment : "unknown",
    };
    const beat = flow.definition.beats.find((candidate) => candidate.id === flow.state.current_beat);
    if (beat) executeExplanationActions(flow, beat);
    return { ...explanationClock };
  }

  function render() {
    window.atlasValidation.invalidate();
    try {
      return atlasWasm.trace_browser_frame(renderFrame);
    } catch (error) {
      window.atlasValidation.error("render_error", error);
      throw error;
    }
  }

  function renderFrame() {
    const renderedValidation = JSON.parse(doc.validation());
    if (atlas.nodes.length || atlas.shapes.length) {
      panelElements.get("gesture-card")?.remove();
      panelElements.delete("gesture-card");
    }
    atlas = JSON.parse(doc.snapshot());
    focusVisible = focusedObject ? focusGraph(atlas, focusedObject)?.visible || null : null;
    if (focusedObject && !focusVisible) focusedObject = null;
    document.dispatchEvent(new CustomEvent("atlas-document-changed", { detail: atlas }));
    if (!learnerMode && openedExploration) {
      atlas.explanation = explorationFlows().find((flow) => flow.definition.id === openedExploration) || null;
    }
    painting = JSON.parse(doc.painting());
    stateMachinePaint = JSON.parse(doc.state_machine_paint());
    indexStateMachinePaint();
    ensureSegmentMaterializations();
    // A set outlives the loss of one member: prune it rather than blanking the
    // whole gesture, because the other five cards are still on the human's
    // screen and still what they are pointing at. A single selection that
    // vanished still clears, which is what stops a dead card from answering
    // for the next question.
    if (selection.length > 1) {
      const alive = new Set([
        ...atlas.nodes.map((node) => node.id),
        ...(atlas.constraints || []).map((constraint) => constraint.id),
        ...(painting.shapes || []).map((item) => item.shape.id),
      ]);
      const live = selection.filter((id) => alive.has(id));
      if (live.length !== selection.length) selectSet(live);
    }
    if (selected && !documentHasTarget(selected)) select(null);
    const marksByTarget = groupMarksByTarget();
    const live = new Set(atlas.nodes.map((node) => node.id));
    for (const [id, entry] of nodeElements) {
      if (!live.has(id)) {
        measurementObserver.unobserve(entry.root);
        entry.root.remove();
        nodeElements.delete(id);
        targets.unregister(id);
        if (isSelected(id)) selectSet(selection.filter((member) => member !== id));
      }
    }
    indexContainment();
    syncAltitudeToDocument();
    for (const node of atlas.nodes) renderNode(node, marksByTarget.get(node.id) || []);
    // After the cards, because a frame is measured from the cards it owns.
    renderFrames(marksByTarget);
    renderEdges();
    renderShapes();
    renderConstraintView(marksByTarget);
    renderExplorationNavigation();
    renderExplanation();
    fitCameraToContent();
    reportMeasurements();
    if (selected) renderInspector();
    agentInkRefresh?.();
    segmentControlsRefresh?.();
    segmentPicker?.refresh(atlas);
    if (seenDigest === null) scheduleBaseline();
    else refreshChanges();
    // Last, because they read what everything above just decided: whether
    // there is an explanation, whether anything changed, whether a constraint
    // is broken, and where the content now ends.
    applyPanelLayout();
    paintMinimap();
    const currentValidation = JSON.parse(doc.validation());
    if (currentValidation.revision === renderedValidation.revision) {
      window.atlasValidation.rendered(currentValidation);
    } else {
      // Measurements changed the document after routes were painted. Verify
      // the next render, which actually uses those dimensions.
      scheduleRender();
    }
  }

  // Tell the document how tall each card actually came out.
  //
  // Height is the one property of the shared page nobody can author: it falls
  // out of the text the two of us wrote. The agent picks x/y blind to it, so
  // without this the overlap it is told about is arithmetic on a guess. Only
  // changes are written, a measurement that repeats the stored value is a
  // wire frame and a merge for no new information.
  const measured = new Map();
  const measurementObserver = new ResizeObserver(() => {
    reportMeasurements();
    scheduleRender();
  });
  const observedMeasurements = new WeakSet();
  function reportMeasurements() {
    for (const node of atlas.nodes) {
      const entry = nodeElements.get(node.id);
      if (!entry) continue;
      if (!observedMeasurements.has(entry.root)) {
        observedMeasurements.add(entry.root);
        measurementObserver.observe(entry.root);
      }
      const height = entry.root.offsetHeight;
      if (!height || Math.abs((measured.get(node.id) ?? 0) - height) < 1) continue;
      measured.set(node.id, height);
      try {
        doc.measure(node.id, height);
      } catch (error) {
        window.atlasValidation.error("measurement_error", error);
        report(`could not record a measurement: ${error}`);
      }
    }
    for (const id of measured.keys()) {
      if (!nodeElements.has(id)) measured.delete(id);
    }
    measureTexts();
    flush();
  }

  // The same write for a text shape. Its height is however many lines its
  // words wrap to at its width, which is a fact about the rendering and not
  // about the document, so the same rule applies: only the side laying it
  // out knows, and the agent should be reasoning about the real number.
  const measuredTexts = new Map();
  function measureTexts() {
    const live = new Set();
    for (const item of painting.shapes || []) {
      if (item.shape.form !== "text") continue;
      live.add(item.shape.id);
      const body = frontLayer.querySelector(`g[data-id="${CSS.escape(item.shape.id)}"] .shape-text`);
      if (!body) continue;
      const height = body.scrollHeight;
      if (!height) continue;
      if (Math.abs((measuredTexts.get(item.shape.id) ?? 0) - height) < 1) continue;
      measuredTexts.set(item.shape.id, height);
      try {
        doc.measure_shape(item.shape.id, height);
      } catch (error) {
        window.atlasValidation.error("measurement_error", error);
        report(`could not record a measurement: ${error}`);
      }
    }
    for (const id of measuredTexts.keys()) {
      if (!live.has(id)) measuredTexts.delete(id);
    }
  }

  // Render on demand, never by polling requestAnimationFrame.
  //
  // A rAF poll looks equivalent and is not: browsers stop firing rAF in a
  // background or headless tab, so a surface built that way paints nothing
  // while it is not being watched, including when an agent drives a headless
  // browser to check its own work. Every path that can change the document
  // calls this instead, and a hidden tab falls back to a timer.
  let renderQueued = false;
  function scheduleRender() {
    if (renderQueued) return;
    renderQueued = true;
    const run = () => {
      renderQueued = false;
      if (doc.take_dirty()) render();
    };
    if (document.visibilityState === "visible") requestAnimationFrame(run);
    else setTimeout(run, 120);
  }
  document.addEventListener("visibilitychange", scheduleRender);

  // ── inspector ───────────────────────────────────────────────────────

  const inspectorTitle = document.getElementById("inspector-title");
  const inspectorBody = document.getElementById("inspector-body");
  let sourceRequest = 0;

  function appendMarkRows(body, target) {
    const marks = atlas.marks.filter((mark) => mark.target === target)
      .sort((a, b) => Number(Boolean(a.answer)) - Number(Boolean(b.answer)));
    if (marks.length) body.push(element("h3", "inspector-section-title inspector-history", "Questions and responses"));
    for (const mark of marks) {
      const row = element("div", "mark-row");
      const head = element("div");
      head.append(element("span", "glyph", `${mark.glyph} `), element("span", null, mark.text));
      row.append(head);
      const contexts = (atlas.context?.entries || []).filter(entry => (mark.context_ids || []).includes(entry.id));
      for (const context of contexts) row.append(element("small", "muted", `Asked at ${context.altitude} scope: ${context.question}`));
      if (mark.answer) {
        const answer = inspectorDisclosure(`answer:${mark.id}`, "Read response", [element("div", "answer", mark.answer)]);
        answer.classList.add("mark-response");
        row.append(answer);
      } else row.append(
        element(
          "div",
          "mark-awaiting",
          `Awaiting response · ${GLYPH_MEANING[mark.glyph] || "question"}`,
        ),
      );
      body.push(row);
    }
  }

  function markActions(targetId) {
    const row = element("div", "inspector-actions");
    for (const [glyph, label] of [["?", "Ask about this"]]) {
      const control = element("button", "quiet-button", label);
      control.type = "button";
      control.title = `${label}: add your reason to ${titleForTarget(targetId)}`;
      control.addEventListener("click", () => openMarkEditor(targetId, glyph));
      row.append(control);
    }
    if (documentHasTarget(targetId)) {
      const control = element("button", "quiet-button challenge-button", "challenge");
      control.type = "button";
      control.title = `Challenge the explanation of ${titleForTarget(targetId)}`;
      control.addEventListener("click", () => openChallenge(targetId));
      row.append(control);
    }
    if (atlas.nodes.some((node) => node.id === targetId)) {
      const claims = element("button", "quiet-button", "Request claims");
      claims.type = "button";
      claims.title = "Ask for one claim at a time with the basis for each";
      claims.addEventListener("click", () => requestClaims(targetId));
      row.append(claims);
    }
    return row;
  }

  /// What the shared model says this card chose, filtered exactly as the
  /// agent's read-back filters it. Asked of the replica rather than derived
  /// here, so the page and `atlas_read` cannot describe the same card
  /// differently.
  function styleRows(id) {
    try {
      return JSON.parse(doc.node_style_words(id));
    } catch (error) {
      console.error("atlas node_style_words failed", error);
      return [];
    }
  }

  const inspectorDisclosures = new Map();

  function inspectorDisclosure(key, title, children) {
    const details = element("details", "inspector-disclosure");
    details.open = inspectorDisclosures.get(key) === true;
    details.append(element("summary", null, title), ...children);
    details.addEventListener("toggle", () => {
      if (details.isConnected) inspectorDisclosures.set(key, details.open);
    });
    return details;
  }

  function cementControls(ids) {
    const nodes = ids.map((id) => atlas.nodes.find((node) => node.id === id));
    if (nodes.some((node) => !node)) return element("span");
    const key = JSON.stringify([...ids].sort());
    const section = element("section", "atlas-cement-controls");
    section.append(element("h3", null, "Decision record"));
    for (const node of nodes) {
      if (!node.cemented) continue;
      const path = node.cemented.split("\n")[0];
      const current = nodeChallenge(node.id).standing === "cemented";
      section.append(element("p", "muted", `${node.label}: ${current ? "Cemented" : "Previous cement"} as ${path}`));
    }
    const review = element("button", "quiet-button", "Review decision and G8 checks");
    review.type = "button";
    review.addEventListener("click", () => openDecisionReview(ids, ctx));
    section.append(review);
    return inspectorDisclosure(`cement:${key}`, "Record an agreed decision", [section]);
  }

  function renderInspector() {
    const active = document.activeElement;
    const editing = active?.dataset?.cementSelection;
    const cursor = editing ? [active.selectionStart, active.selectionEnd] : null;
    renderInspectorContent();
    if (editing) {
      const input = inspectorBody.querySelector("[data-cement-selection]");
      if (input?.dataset.cementSelection === editing) {
        input.focus({ preventScroll: true });
        input.setSelectionRange(...cursor);
      }
    }
  }

  function renderInspectorContent() {
    // A set is its own thing to look at, not the first member with the rest
    // hidden. The panel says how many and names them, and the mark actions
    // aim at all of them, which is what the ? and ! keys will do from here.
    if (selection.length > 1) return renderSetInspector();
    const node = atlas.nodes.find((candidate) => candidate.id === selected);
    if (!node) {
      const constraint = (atlas.constraints || []).find((candidate) => candidate.id === selected);
      if (constraint) return renderConstraintInspector(constraint);
      const drawn = (painting.shapes || []).find((item) => item.shape.id === selected);
      if (drawn) return renderShapeInspector(drawn);
      inspectorTitle.textContent = "Nothing selected";
      inspectorBody.replaceChildren(
        element("p", "muted", "Click a node to see who put it there and read the source it claims."),
      );
      return;
    }
    inspectorTitle.textContent = node.label;
    const list = element("dl");
    const rows = [
      ["id", node.id],
      ["tone", node.tone],
      ["status", node.status],
      ["created by", node.created_by],
      ["last edit", node.touched_by],
      ["position", `${Math.round(node.x)}, ${Math.round(node.y)}`],
    ];
    // The style register, in the read-back's own words and by the read-back's
    // own filter: a card that chose nothing adds no rows. Without this the
    // panel had a fixed six-row schema older than the register, so the agent
    // could publish a taxonomy the page rendered and could not name, leaving
    // the human to take the agent's word in chat for what a bright ring meant.
    for (const word of styleRows(node.id)) {
      const [key, value] = word.split("=");
      rows.push([key, value]);
    }
    if (node.path) rows.push(["source", sourceRef(node)]);
    for (const [key, value] of rows) {
      list.append(element("dt", null, key), element("dd", null, value));
    }
    const body = [markActions(node.id)];
    if (node.note) body.push(element("p", null, node.note));

    appendMarkRows(body, node.id);
    appendClaimRows(body, node);
    appendHiddenClaimRows(body, node);
    body.push(inspectorDisclosure(`details:${node.id}`, "Object details and provenance", [list]));
    body.push(cementControls([node.id]));

    if (node.path) {
      const head = element("div", "source-head");
      head.append(element("h3", null, `${sourceRef(node)}, read by the host`));
      body.push(head);
      const pre = element("pre", null, "loading…");
      body.push(pre);
      const token = ++sourceRequest;
      const query = new URLSearchParams({ path: node.path, lines: node.lines || "" });
      fetch(`/atlas/source?${query}`, { headers: { accept: "application/json" } })
        .then((response) => response.json())
        .then((payload) => {
          if (token !== sourceRequest) return;
          pre.textContent = payload.ok
            ? numbered(payload.text, payload.first_line)
            : `could not read it: ${payload.error}`;
        })
        .catch((error) => {
          if (token === sourceRequest) pre.textContent = `could not read it: ${error}`;
        });
    }
    inspectorBody.replaceChildren(...body);
  }

  function renderSetInspector() {
    inspectorTitle.textContent = `${selection.length} selected`;
    const body = [markActions(selected)];
    const segmentFamily = selectedSegmentFamily();
    if (segmentFamily) {
      const actions = element("div", "segment-set-motion-actions");
      const describe = element("button", "segment-describe-motion-button", "Describe motion...");
      describe.type = "button";
      describe.addEventListener("click", (event) => prefillMotionRequest(event));
      const replay = element("button", "segment-replay-motion-button", "Replay motion");
      replay.type = "button";
      replay.addEventListener("click", (event) => replaySelectedMotion(event));
      const stop = element("button", "segment-stop-motion-button", "Stop motion");
      stop.type = "button";
      const hasMotion = Boolean(
        motionOwnerFor(segmentFamily.segments[0].id)
        || motionOwnerFor(segmentFamily.owner.id)
      );
      replay.disabled = !hasMotion;
      stop.disabled = !hasMotion;
      stop.addEventListener("click", (event) => clearSelectedMotion(event));
      actions.append(describe, replay, stop);
      body.push(actions);
    }
    const list = element("ol", "selection-list");
    for (const id of selection) {
      const row = element("li", null, titleForTarget(id));
      row.addEventListener("click", () => select(id));
      row.title = "Show just this one";
      list.append(row);
    }
    body.push(list);
    body.push(cementControls([...selection]));
    body.push(element(
      "p",
      "muted",
      "Shift-click a member to drop it. Drag any of them to move all of them. "
        + (segmentFamily
          ? "Describe one motion for this exact set."
          : "A mark from here attaches to every one."),
    ));
    inspectorBody.replaceChildren(...body);
  }

  function renderConstraintInspector(constraint) {
    inspectorTitle.textContent = constraintStatement(constraint, (id) => id);
    const list = element("dl");
    const rows = [
      ["created by", constraint.created_by],
      ["status", constraint.status],
      ["salience", constraint.salience],
    ];
    for (const [key, value] of rows) {
      list.append(element("dt", null, key), element("dd", null, value));
    }
    const body = [markActions(constraint.id), list];
    appendMarkRows(body, constraint.id);
    inspectorBody.replaceChildren(...body);
  }

  // Selecting something you drew shows you the sentence the agent will read
  // about it. The point of the drawn layer is that a gesture survives as
  // meaning rather than as pixels; if the human cannot check what their
  // gesture was understood to mean, they have to take that on trust.
  const RELATION_WORDS = {
    encloses: "groups",
    "crosses-out": "strikes through",
    "points-at": "points at",
    "starts-at": "starts at, ending on nothing",
    connects: "connects",
    says: "sits over",
    free: "touches nothing",
  };

  function renderShapeInspector(drawn) {
    const { shape, reading } = drawn;
    inspectorTitle.textContent = shape.label || `${shape.created_by}'s ${shape.form}`;
    const list = element("dl");
    const rows = [
      ["id", shape.id],
      ["form", shape.form],
      ["drawn by", shape.created_by],
      ["last edit", shape.touched_by],
    ];
    if (shape.form === "segment") {
      rows.push(
        ["mask", shape.segment_status],
        ["generation", String(shape.segment_generation)],
        ["model", shape.segment_model_id],
        ["semantics", shape.segment_semantics_source],
      );
    } else {
      rows.splice(2, 0, ["ink", shape.ink]);
    }
    if (shape.from || shape.to) {
      rows.push(["bound", [shape.from, shape.to].filter(Boolean).join(" → ")]);
    }
    // Only what is not the default: a row that always reads "solid, 2px,
    // 100%" is noise the eye learns to skip, and then misses the one that
    // is not.
    if (shape.stroke_width > 0 && shape.stroke_width !== DEFAULT_STROKE_WIDTH) {
      rows.push(["stroke", `${shape.stroke_width}px`]);
    }
    if (shape.stroke_style && shape.stroke_style !== "solid") rows.push(["style", shape.stroke_style]);
    if (typeof shape.opacity === "number" && shape.opacity < 100) rows.push(["opacity", `${shape.opacity}%`]);
    if (shape.angle) rows.push(["rotated", `${Math.round((shape.angle * 180) / Math.PI)}°`]);
    if (shape.font_size > 0) rows.push(["type", `${shape.font_size}px`]);
    if (shape.groups?.length) rows.push(["groups", shape.groups.join(", ")]);
    if (shape.frame) {
      const holder = paintedShape(shape.frame);
      rows.push(["in frame", holder?.shape.label || shape.frame]);
    }
    if (shape.form === "frame") {
      const held = (painting.shapes || []).filter((item) => item.shape.frame === shape.id).length;
      if (held) rows.push(["holds", `${held} shape${held === 1 ? "" : "s"}`]);
    }
    for (const [key, value] of rows) {
      list.append(element("dt", null, key), element("dd", null, value));
    }
    const body = [markActions(shape.id), list];
    // The register split's repair, from the human side: a drawing that turned
    // out to mean something gets promoted into a card, and everything pointing
    // at it (marks, arrow bindings, constraints) follows the promotion.
    if (shape.form !== "arrow" && shape.form !== "line" && shape.form !== "segment") {
      const liftRow = element("div", "inspector-actions");
      const lift = element("button", "quiet-button", "make this a card");
      lift.type = "button";
      lift.title =
        "Turn this drawing into a card the agent can reason about. Its words, marks, and arrows come along.";
      lift.addEventListener("click", () => {
        const nodeId = liftShape(shape.id);
        if (nodeId) {
          select(nodeId);
          report("Now a card. Its marks and arrows followed it.");
        }
      });
      liftRow.append(lift);
      body.push(liftRow);
    }
    if (shape.form === "segment") {
      body.push(element(
        "p",
        "muted",
        "Drag the object to move it. Use Object control on the canvas to animate it.",
      ));
    }
    appendMarkRows(body, shape.id);
    const bindReceipt = arrowBindReceipt(drawn);
    if (bindReceipt) {
      body.push(
        element(
          "p",
          `arrow-bind-receipt${bindReceipt.warning ? " warning" : " bound"}`,
          bindReceipt.text,
        ),
      );
    }

    const labelsFor = (ids) => ids.map((id) => {
      const node = atlas.nodes.find((candidate) => candidate.id === id);
      if (node) return node.label;
      const item = paintedShape(id);
      return item?.shape.label || (item ? `${item.shape.form} ${id}` : id);
    });
    const labels = labelsFor(reading.targets);
    const verb = RELATION_WORDS[reading.relation];
    body.push(element("p", "eyebrow", "The agent reads this as"));
    if (shape.form === "text") {
      body.push(element("p", "reading", `saying “${shape.label}”`));
    } else {
      body.push(
        element(
          "p",
          verb && labels.length ? "reading" : "muted",
          verb && labels.length
            ? `${verb} ${labels.join(", ")}`
            : `This ${shape.form} is not linked to anything yet. Move or redraw it around or through a card to give it meaning, or ask about it.`,
        ),
      );
    }
    // The half-covered cards, in the same words the agent gets. This is the
    // one part of a reading a human cannot check by looking at the screen,
    // "did my loop include that card or not" is exactly the question the
    // drawing is ambiguous about.
    if ((reading.partial || []).length) {
      body.push(
        element(
          "p",
          "muted",
          `only partly covers ${labelsFor(reading.partial).join(", ")}, neither in nor out`,
        ),
      );
    }
    inspectorBody.replaceChildren(...body);
  }

  function arrowBindReceipt(drawn) {
    if (drawn?.shape?.form !== "arrow") return null;
    const { from, to } = drawn.shape;
    if (from && to) {
      return {
        text: `connects ${titleForTarget(from)} → ${titleForTarget(to)}`,
        warning: false,
      };
    }
    return {
      text: "Endpoint touches nothing. Drop it inside a card.",
      warning: true,
    };
  }

  function numbered(text, first) {
    return text
      .split("\n")
      .map((line, index) => `${String(first + index).padStart(4)} ${line}`)
      .join("\n");
  }

  function deepActiveElement() {
    let activeElement = document.activeElement;
    while (activeElement?.shadowRoot?.activeElement) {
      activeElement = activeElement.shadowRoot.activeElement;
    }
    return activeElement;
  }

  function focusedTextEntry() {
    const activeElement = deepActiveElement();
    return activeElement instanceof HTMLInputElement
      || activeElement instanceof HTMLTextAreaElement
      || activeElement instanceof HTMLSelectElement
      || Boolean(activeElement?.isContentEditable);
  }

  function titleForTarget(id) {
    const node = atlas.nodes.find((candidate) => candidate.id === id);
    if (node) return node.label;
    const drawn = paintedShape(id);
    if (drawn) return drawn.shape.label || `${drawn.shape.form} ${id}`;
    const constraint = (atlas.constraints || []).find((candidate) => candidate.id === id);
    if (constraint) return constraintStatement(constraint, objectTitle);
    return id;
  }

  function documentHasTarget(id) {
    if (!id) return false;
    return documentHasTargets([id]).length === 1;
  }

  // One snapshot parse for the whole set. Asking per member turned a marquee
  // echo into one full document parse per selected card.
  function documentHasTargets(ids) {
    if (!ids.length) return [];
    let snapshot;
    try {
      snapshot = JSON.parse(doc.snapshot());
    } catch {
      return [];
    }
    const live = new Set([
      ...snapshot.nodes.map((candidate) => candidate.id),
      ...snapshot.shapes.map((candidate) => candidate.id),
      ...(snapshot.constraints || []).map((candidate) => candidate.id),
    ]);
    return ids.filter((id) => live.has(id));
  }

  function selectionLabels() {
    return selection.map((id) => titleForTarget(id));
  }

  /// What a set is called in one phrase, everywhere a human reads it.
  ///
  /// Count first, then names. The count is the fact a wrong answer usually
  /// gets wrong, and it is the one that survives truncation.
  function selectionSummary() {
    if (selection.length === 0) return "";
    if (selection.length === 1) return titleForTarget(selection[0]);
    const names = selectionLabels();
    const shown = names.slice(0, 3).join(", ");
    return names.length > 3
      ? `${names.length} selected: ${shown}, and ${names.length - 3} more`
      : `${names.length} selected: ${shown}`;
  }

  function isSelected(id) {
    return Boolean(id) && selection.includes(id);
  }

  function restoreComposerAnchor() {
    if (!selected || typeof conversation?.setAnchor !== "function") return;
    conversation.setAnchor({ label: selectionSummary() });
  }

  function keepSelectionWhileTyping() {
    if (!selected || !focusedTextEntry()) return;
    restoreComposerAnchor();
    const now = performance.now();
    if (now - selectionRefreshAt < 900) return;
    selectionRefreshAt = now;
    targets.selectMany(selection).catch((error) => {
      report(`could not keep the target pinned: ${error.message || error}`);
    });
  }

  function select(id, publish = true) {
    selectSet(id ? [id] : [], publish);
  }

  /// Shift-click semantics: membership toggles, the rest of the set stays.
  function toggleSelected(id, publish = true) {
    if (!id) return;
    selectSet(
      selection.includes(id)
        ? selection.filter((member) => member !== id)
        : [...selection, id],
      publish,
    );
  }

  function selectSet(ids, publish = true) {
    const next = [];
    for (const candidate of ids || []) {
      if (candidate && !next.includes(candidate)) next.push(candidate);
    }
    if (sourcePopoverTarget && !next.includes(sourcePopoverTarget)) closeSourcePopover();
    selection = next;
    const id = next[0] || null;
    selected = id;
    const members = new Set(next);
    document.dispatchEvent(new CustomEvent("atlas-selection-changed", {
      detail: id
        ? {
          id,
          label: titleForTarget(id),
          count: next.length,
          ids: [...next],
          labels: selectionLabels(),
          summary: selectionSummary(),
        }
        : null,
    }));
    for (const [nodeId, entry] of nodeElements) {
      entry.root.classList.toggle("selected", members.has(nodeId));
    }
    paintDetail();
    for (const group of world.querySelectorAll(".shape")) {
      group.classList.toggle("selected", members.has(group.dataset.id));
    }
    for (const group of world.querySelectorAll(".constraint-glyph")) {
      group.classList.toggle("selected", members.has(group.dataset.constraintId));
    }
    // Selecting changes nothing in the document, so no render is scheduled,
    // which left the handles sitting on whatever was selected before, offering
    // grips that belonged to another shape.
    renderHandles();
    renderInspector();
    segmentControlsRefresh?.();
    renderExplorationNavigation();
    if (id) restoreComposerAnchor();
    // Dropping the composer anchor is itself a message to the host: the
    // shared composer answers `clearAnchor` by posting
    // /semantic-targets/clear. So an unpublished, local-only clear must not
    // touch it, or a marquee still reaches the host twice, once empty. That
    // empty first call is what a parked agent woke on, measured live.
    else if (publish) conversation?.clearAnchor?.();
    // A host echo updates selection too. It must not steal the caret from a
    // composer or inline editor, which is what used to close the editor and
    // make the visible target disappear while the human was typing.
    if (id && publish && !focusedTextEntry()) view.focus({ preventScroll: true });
    if (publish) {
      // The projection refreshes on render, so an object written a moment ago
      // is in the document before it is in `atlas`. Selecting it then must not
      // publish a clear: the echo comes back as select(null) and the target
      // the caller just created slips out of the human's hands.
      const live = next.filter((candidate) => (
        atlas.nodes.some((node) => node.id === candidate)
          || paintedShape(candidate)
          || (atlas.constraints || []).some((constraint) => constraint.id === candidate)
          || documentHasTarget(candidate)
      ));
      // One gesture, one call. Publishing a marquee member by member would
      // wake every parked agent once per card and read back as N selections
      // of one thing rather than one selection of N.
      const operation = live.length ? targets.selectMany(live) : targets.clear();
      operation.catch((error) => report(`could not bind semantic focus: ${error.message || error}`));
    }
  }

  targets.onSelection((target) => {
    if (!target && selected && focusedTextEntry()) {
      queueMicrotask(restoreComposerAnchor);
      return;
    }
    if (target && !documentHasTarget(target.targetId)) {
      select(null, false);
      targets.clear().catch((error) => report(`could not clear a missing target: ${error.message || error}`));
      return;
    }
    // The echo carries the whole set. Reading only the primary here is what
    // would make a marquee shrink to one card as soon as the host answered.
    const echoed = target
      ? (Array.isArray(target.targetIds) && target.targetIds.length
        ? target.targetIds
        : [target.targetId])
      : [];
    selectSet(documentHasTargets(echoed), false);
  });
  document.addEventListener("focusin", () => queueMicrotask(keepSelectionWhileTyping), true);
  document.addEventListener("input", keepSelectionWhileTyping, true);
  conversation?.shadowRoot?.addEventListener("focusin", () => queueMicrotask(keepSelectionWhileTyping));
  conversation?.shadowRoot?.addEventListener("input", keepSelectionWhileTyping);

  // ── inline text entry (no browser dialogs: they block the page) ──────

  function askFor({ x, y, glyph, value = "", placeholder, targetLabel = "", receipt = null, onCommit }) {
    settleDirectEditor(false);
    closeInline();
    const box = element("div", "inline-input");
    // Kept inside the pane. An input hanging off the right-hand edge is what
    // makes the browser scroll a container that is not supposed to scroll,
    // see `toWorld`.
    const pane = view.getBoundingClientRect();
    const editorWidth = targetLabel || receipt ? 390 : INLINE_WIDTH;
    box.style.left = `${Math.max(4, Math.min(x, pane.width - editorWidth))}px`;
    box.style.top = `${Math.max(4, Math.min(y, pane.height - 40))}px`;
    if (glyph) box.append(element("span", "glyph", glyph));
    if (targetLabel) {
      box.append(element("span", "inline-target", `${placeholder} · ${targetLabel}`));
    }
    if (receipt) {
      box.append(
        element(
          "span",
          `inline-receipt${receipt.warning ? " warning" : " bound"}`,
          receipt.text,
        ),
      );
    }
    const input = document.createElement("input");
    input.type = "text";
    input.value = value;
    input.placeholder = targetLabel ? "Add your reason…" : placeholder || "";
    input.maxLength = 400;
    box.append(input);
    view.append(box);
    inlineInput = box;
    input.focus();
    input.select();
    let settled = false;
    const finishInline = (commit) => {
      if (settled) return;
      settled = true;
      const text = input.value.trim();
      closeInline("settled");
      if (commit && text) onCommit(text);
    };
    box._atlasFinish = finishInline;
    input.addEventListener("keydown", (event) => {
      event.stopPropagation();
      if (event.key === "Enter") {
        finishInline(true);
        view.focus({ preventScroll: true });
      } else if (event.key === "Escape") {
        finishInline(false);
        view.focus({ preventScroll: true });
      }
    });
    // Blur remains a keyboard-navigation fallback. Pointer click-away is
    // settled explicitly below because an unfocusable canvas does not make a
    // focused input blur when a human clicks it.
    input.addEventListener("blur", () => finishInline(box.dataset.closeMode !== "discard"));
  }

  // Clear the handle BEFORE detaching the box, not after.
  //
  // Removing a focused element makes Chrome fire `blur` synchronously, and the
  // blur listener calls this function again. With the handle still set, that
  // reentrant call removes a node that is already mid-detach, `NotFoundError`,
  // thrown back through the keydown handler, which then never reaches
  // `onCommit`. Every human write that goes through `askFor` (new node, rename,
  // mark) silently did nothing while the input visibly closed.
  function closeInline(mode = "discard") {
    const box = inlineInput;
    inlineInput = null;
    if (box) box.dataset.closeMode = mode;
    box?.remove();
  }

  function settleInlineEditor(commit) {
    if (inlineDirect) {
      inlineDirect.finish(commit);
      return;
    }
    inlineInput?._atlasFinish?.(commit);
  }

  function settleDirectEditor(commit) {
    inlineDirect?.finish(commit);
  }

  function selectElementText(target) {
    const selection = window.getSelection();
    if (!selection) return;
    const range = document.createRange();
    range.selectNodeContents(target);
    selection.removeAllRanges();
    selection.addRange(range);
  }

  // Direct manipulation for labels already on the canvas. The visible words
  // become the editor, rather than opening a detached text box nearby. A
  // segment badge temporarily shows only its authored label; the generation
  // receipt returns as soon as the edit settles.
  function editTextInPlace(target, value, ariaLabel, onCommit) {
    if (!target) return false;
    settleInlineEditor(true);
    const originalText = target.textContent;
    const runningAnimations = (target.closest?.(".segment-object")?.getAnimations() || [])
      .filter((animation) => animation.playState === "running");
    for (const animation of runningAnimations) animation.pause();
    let settled = false;
    const finish = (commit) => {
      if (settled) return;
      settled = true;
      const text = (target.textContent || "").replace(/\s*\n+\s*/g, " ").trim().slice(0, 400);
      inlineDirect = null;
      target.removeEventListener("keydown", onKeydown);
      target.removeEventListener("blur", onBlur);
      target.removeEventListener("input", onInput);
      target.removeAttribute("contenteditable");
      target.removeAttribute("role");
      target.removeAttribute("aria-label");
      target.classList.remove("inline-direct-editor");
      target.textContent = originalText;
      for (const animation of runningAnimations) {
        try {
          animation.play();
        } catch {
          // A CRDT render may have replaced an animation while the label was
          // being edited. Its replacement already starts from document state.
        }
      }
      if (commit && text) onCommit(text);
    };
    const onKeydown = (event) => {
      event.stopPropagation();
      if (event.key === "Enter") {
        event.preventDefault();
        finish(true);
        view.focus({ preventScroll: true });
      } else if (event.key === "Escape") {
        event.preventDefault();
        finish(false);
        view.focus({ preventScroll: true });
      }
    };
    const onBlur = () => finish(true);
    const onInput = () => {
      if ((target.textContent || "").length <= 400) return;
      target.textContent = (target.textContent || "").slice(0, 400);
      selectElementText(target);
    };
    inlineDirect = { element: target, finish };
    target.classList.add("inline-direct-editor");
    target.setAttribute("contenteditable", "plaintext-only");
    target.setAttribute("role", "textbox");
    target.setAttribute("aria-label", ariaLabel);
    target.spellcheck = true;
    target.textContent = value;
    target.addEventListener("keydown", onKeydown);
    target.addEventListener("blur", onBlur);
    target.addEventListener("input", onInput);
    target.focus({ preventScroll: true });
    selectElementText(target);
    return true;
  }

  // A single pointer press outside either editor settles it before the canvas
  // starts its own select, marquee, or drag gesture. Nonempty drafts commit;
  // empty drafts close without ever entering the shared document.
  document.addEventListener("pointerdown", (event) => {
    const editor = inlineDirect?.element || inlineInput;
    if (!editor || editor.contains(event.target)) return;
    settleInlineEditor(true);
  }, { capture: true });

  function targetElement(targetId) {
    // A container's card is hidden, so its frame is the thing to anchor an
    // editor to and the thing to flash during a marquee.
    if (frameElements.has(targetId)) return frameElements.get(targetId).root;
    return nodeElements.get(targetId)?.root
      || Array.from(world.querySelectorAll(".shape"))
        .find((candidate) => candidate.dataset.id === targetId)
      || constraintTargetElements.get(targetId)
      || null;
  }

  function openMarkEditor(targetId, glyph, anchor = null) {
    const target = anchor || targetElement(targetId);
    if (!Object.prototype.hasOwnProperty.call(GLYPH_MEANING, glyph)) return false;
    if (!target) {
      reportRejectedGesture(glyph, targetId, "the selected target no longer exists");
      return false;
    }
    closeContextMenu();
    if (!isSelected(targetId)) select(targetId);
    // A mark aimed at a member of the current set pins to the WHOLE set. The
    // alternative, quietly marking the one card the pointer happened to be
    // over, answers a question the human did not ask.
    const pinned = isSelected(targetId) && selection.length > 1 ? [...selection] : [targetId];
    const label = pinned.length > 1
      ? `${pinned.length} selected: ${pinned.map((id) => titleForTarget(id)).join(", ")}`
      : titleForTarget(targetId);
    const box = view.getBoundingClientRect();
    const rect = target.getBoundingClientRect();
    askFor({
      x: rect.left - box.left,
      y: rect.bottom - box.top + 6,
      glyph,
      placeholder: GLYPH_MEANING[glyph],
      targetLabel: label,
      onCommit: (text) => (pinned.length > 1
        ? markEach(pinned, glyph, text)
        : markTarget(targetId, glyph, text)),
    });
    report(`${glyph} ${GLYPH_MEANING[glyph]} editor opened for ${label}. Escape cancels.`);
    return true;
  }

  function closeContextMenu() {
    contextMenu?.remove();
    contextMenu = null;
  }

  function openContextMenu(nodeElement, event) {
    event.preventDefault();
    event.stopPropagation();
    closeContextMenu();
    const targetId = nodeElement.dataset.id;
    select(targetId);
    const pane = view.getBoundingClientRect();
    const menu = element("div", "atlas-context-menu");
    menu.setAttribute("role", "menu");
    menu.setAttribute("aria-label", `Actions for ${titleForTarget(targetId)}`);
    menu.append(element("strong", "atlas-context-title", titleForTarget(targetId)));
    for (const [glyph, label] of [["?", "Ask about this"], ["!", "Challenge"], ["*", "Mark important"]]) {
      const control = element("button", "atlas-context-action", label);
      control.type = "button";
      control.setAttribute("role", "menuitem");
      control.addEventListener("click", (click) => {
        click.stopPropagation();
        openMarkEditor(targetId, glyph, nodeElement);
      });
      menu.append(control);
    }
    if (atlas.nodes.some((node) => node.id === targetId)) {
      const control = element("button", "atlas-context-action", "Rename · Enter");
      control.type = "button";
      control.setAttribute("role", "menuitem");
      control.title = "Edit the card title";
      control.addEventListener("click", (click) => {
        click.stopPropagation();
        closeContextMenu();
        editNodeLabel(targetId);
      });
      menu.append(control);
    }
    menu.addEventListener("contextmenu", (value) => value.preventDefault());
    view.append(menu);
    menu.style.left = `${Math.max(6, Math.min(event.clientX - pane.left, pane.width - menu.offsetWidth - 6))}px`;
    menu.style.top = `${Math.max(6, Math.min(event.clientY - pane.top, pane.height - menu.offsetHeight - 6))}px`;
    contextMenu = menu;
  }

  // ── gestures ────────────────────────────────────────────────────────

  let drag = null;
  let sketching = null;

  // The bindable card or drawing shape under the pointer right now, if any.
  // Asked of the DOM rather
  // than recomputed from card bounds, the browser already knows, and a
  // second implementation of "is this point on that card" is a second
  // implementation that can disagree.
  function bindingTargetUnder(event) {
    const preview = sketching?.preview;
    if (preview) preview.style.display = "none";
    const hit = document.elementFromPoint(event.clientX, event.clientY);
    if (preview) preview.style.display = "";
    const node = hit?.closest?.(".node")?.dataset.id;
    if (node) return node;
    const shapeId = hit?.closest?.(".shape")?.dataset.id;
    const item = shapeId ? paintedShape(shapeId) : null;
    return item && (isBoxForm(item.shape.form) || item.shape.form === "text") ? shapeId : null;
  }

  function startSketch(event) {
    const start = toWorld(event);
    const form = active.form;
    // A text is placed, not dragged out: you point at where the words go and
    // then write them. Nothing is put in the document until there is
    // something to put there, an empty text would be an invisible object on
    // a page whose whole premise is that both sides can read everything, and
    // the core crate refuses to store one.
    if (form === "text") {
      event.preventDefault();
      backToSelect();
      const box = view.getBoundingClientRect();
      askFor({
        x: event.clientX - box.left,
        y: event.clientY - box.top,
        placeholder: "type some text…",
        onCommit: (label) => {
          const id = placeShape({
            form: "text",
            x: Math.round(start.x),
            y: Math.round(start.y),
            label,
            ink: inkColor,
          });
          if (id) select(id);
        },
      });
      return;
    }
    const preview = document.createElementNS(SVG_NS, "g");
    preview.setAttribute("class", "shape sketching");
    (form === "ink" || form === "line" || form === "arrow" ? frontLayer : backLayer).append(preview);
    sketching = {
      form,
      start,
      end: start,
      points: [[0, 0]],
      preview,
      from: form === "arrow" || form === "line" ? bindingTargetUnder(event) : null,
    };
    try {
      view.setPointerCapture(event.pointerId);
    } catch {
      // No live pointer to capture (synthetic events, or one already gone).
      // The stroke still commits on pointerup; capture only keeps it tracking
      // once the cursor leaves the element.
    }
    event.preventDefault();
  }

  function moveSketch(event) {
    const point = toWorld(event);
    sketching.end = point;
    if (sketching.form === "ink") {
      const last = sketching.points[sketching.points.length - 1];
      const [dx, dy] = [point.x - sketching.start.x, point.y - sketching.start.y];
      // Drop samples the hand did not really move between; the writer
      // simplifies too, but there is no reason to carry them until then.
      if (Math.hypot(dx - last[0], dy - last[1]) < 1.2) return;
      sketching.points.push([dx, dy]);
    }
    drawPreview();
  }

  // The preview is the same geometry the committed shape will have, ink goes
  // through the very same `outline` the core crate uses. A preview that only
  // approximates the result makes the stroke jump at the moment it lands,
  // which reads as the page disagreeing with itself about what you just drew.
  function drawPreview() {
    const { form, start, end, points, preview } = sketching;
    preview.replaceChildren();
    const color = inkOf(inkColor);
    if (form === "ink") {
      const path = document.createElementNS(SVG_NS, "path");
      path.setAttribute("d", polygonPath(JSON.parse(ink_outline(encodePoints(points)))));
      path.setAttribute("fill", color);
      path.setAttribute("transform", `translate(${start.x} ${start.y})`);
      preview.append(path);
      return;
    }
    if (form === "line" || form === "arrow") {
      const line = document.createElementNS(SVG_NS, "line");
      line.setAttribute("x1", start.x);
      line.setAttribute("y1", start.y);
      line.setAttribute("x2", end.x);
      line.setAttribute("y2", end.y);
      line.setAttribute("stroke", color);
      line.setAttribute("stroke-width", "2");
      line.setAttribute("stroke-linecap", "round");
      preview.append(line);
      if (form === "arrow") {
        preview.append(arrowhead([start.x, start.y], [end.x, end.y], "triangle", color));
      }
      return;
    }
    const box = normalized(start, end);
    const node = document.createElementNS(SVG_NS, "rect");
    node.setAttribute("x", box.x);
    node.setAttribute("y", box.y);
    node.setAttribute("width", box.w);
    node.setAttribute("height", box.h);
    node.setAttribute("rx", form === "ellipse" ? Math.min(box.w, box.h) / 2 : 10);
    node.setAttribute("stroke", color);
    node.setAttribute("stroke-width", "2");
    node.setAttribute("stroke-dasharray", "6 4");
    node.setAttribute("fill", "none");
    preview.append(node);
  }

  // A stroke lands as ONE write when the pen lifts, not at the drag rate a
  // card move uses. A card move is a position that keeps being true; a stroke
  // is a single gesture, and streaming a growing point array 15 times a second
  // would put the same stroke on the wire dozens of times over.
  function endSketch(event) {
    const { form, start, end, points, preview, from } = sketching;
    preview.remove();
    const to = form === "arrow" || form === "line" ? bindingTargetUnder(event) : null;
    sketching = null;

    let id = null;
    if (form === "ink") {
      if (points.length >= 2) {
        id = placeShape({ form, x: start.x, y: start.y, points: encodePoints(points), ink: inkColor, ...strokeOf() });
      }
    } else if (form === "line" || form === "arrow") {
      if (Math.hypot(end.x - start.x, end.y - start.y) < 6) return backToSelect();
      // Dragged from one card to another: bind it, so the arrow keeps meaning
      // what it meant after either card is moved.
      const patch = from && to && from !== to
        ? { form, from, to, ink: inkColor, ...strokeOf() }
        : {
            form,
            x: start.x,
            y: start.y,
            points: encodePoints([[0, 0], [end.x - start.x, end.y - start.y]]),
            ink: inkColor,
            ...strokeOf(),
            ...(from ? { from } : {}),
            ...(to ? { to } : {}),
          };
      id = placeShape(patch);
    } else {
      const box = normalized(start, end);
      if (box.w < 12 || box.h < 12) return backToSelect();
      id = placeShape({ form, x: box.x, y: box.y, w: box.w, h: box.h, ink: inkColor, ...strokeOf() });
    }
    if (id) {
      select(id);
      // Drawing a box and then typing is one gesture, not two. Excalidraw
      // behaves this way and it is the reason a shape ever gets named: put
      // the caret in it now and the label happens, ask the human to find a
      // menu later and it does not. `Esc` leaves the shape unlabelled.
      if (form !== "ink") editShapeLabel(id);
    }
    backToSelect();
  }

  function editNodeLabel(id) {
    const node = atlas.nodes.find((candidate) => candidate.id === id);
    const label = nodeElements.get(id)?.label;
    if (!node || !label) return false;
    return editTextInPlace(label, node.label, "Edit node text", (value) => place({ id, label: value }));
  }

  // Put a caret directly into text that is already visible. Box and connector
  // captions still use the anchored editor because their SVG label does not
  // receive pointer events; text shapes and segment badges edit in place.
  function editShapeLabel(id, directTarget = null) {
    let item = paintedShape(id);
    // A shape drawn a moment ago is in the document but not yet in the
    // projection: rendering is scheduled for the next frame, and this runs in
    // the same tick as the write. Catch the projection up rather than waiting,
    // or "draw a box and type" quietly does nothing on the very gesture it
    // exists for.
    if (!item) {
      render();
      item = paintedShape(id);
    }
    if (!item) return;
    const shapeSelector = `g.shape[data-id="${CSS.escape(id)}"]`;
    const editable = directTarget
      || (item.shape.form === "segment"
        ? world.querySelector(`${shapeSelector} > foreignObject.segment-badge-frame .segment-badge`)
        : item.shape.form === "text"
          ? world.querySelector(`${shapeSelector} > foreignObject > .shape-text`)
          : null);
    if (editable && editTextInPlace(
      editable,
      item.shape.label,
      item.shape.form === "segment" ? "Edit object name" : "Edit canvas text",
      (label) => placeShape({ id, label }),
    )) return;
    const bounds = shapeBounds(item.shape, item.ends, item.path);
    const receipt = arrowBindReceipt(item);
    // World to view: the inverse of `toWorld`, which is the only other place
    // this transform is written down. `askFor` keeps it inside the pane.
    askFor({
      x: bounds.x * camera.scale + camera.x - view.scrollLeft + 8,
      y: bounds.y * camera.scale + camera.y - view.scrollTop + 8,
      value: item.shape.label,
      placeholder: item.shape.form === "text" ? "type some text…" : "name this shape…",
      receipt,
      onCommit: (label) => placeShape({ id, label }),
    });
  }

  // The pen stays down so you can keep drawing; every other tool is a
  // one-shot, which is what makes "draw a box, then move it" work without
  // having to remember to switch back.
  function backToSelect() {
    if (active.form !== "ink") setTool(TOOLS[0]);
  }

  const encodePoints = (points) => points.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");

  function normalized(a, b) {
    return {
      x: Math.min(a.x, b.x),
      y: Math.min(a.y, b.y),
      w: Math.abs(b.x - a.x),
      h: Math.abs(b.y - a.y),
    };
  }

  // ── moving and reshaping what is already drawn ──────────────────────
  //
  // Every gesture below is an ordinary `place_shape` patch, the same call the
  // agent makes and the same one that created the shape. Nothing new was
  // needed in the shared model to make a drawing editable; the browser simply
  // never offered the gesture, so a shape could only be deleted and drawn
  // again.

  function paintedShape(id) {
    return (painting.shapes || []).find((item) => item.shape.id === id) || null;
  }

  function segmentDescendants(id, seen = new Set()) {
    const out = [];
    for (const child of atlas.shapes.filter((shape) => shape.segment_parent_id === id)) {
      if (seen.has(child.id)) continue;
      seen.add(child.id);
      out.push(child, ...segmentDescendants(child.id, seen));
    }
    return out;
  }

  function startShapeDrag(event, element) {
    const id = element.dataset.id;
    select(id);
    const item = paintedShape(id);
    if (!item) return;
    const { shape } = item;
    // A fully bound arrow has no position of its own: both ends are resolved
    // from the cards every time it is drawn. Dragging it would write x/y that
    // nothing reads, and the arrow would sit exactly where it was, a gesture
    // that looks broken because it silently did nothing.
    if (shape.from && shape.to) {
      report("this connector follows two bound objects. Drag an end into open space to detach it");
      return;
    }
    const start = toWorld(event);
    const followers = shape.form === "segment"
      ? segmentDescendants(id).map((child) => ({
        id: child.id,
        element: [...world.querySelectorAll(".shape")]
          .find((candidate) => candidate.dataset.id === child.id) || null,
        x: child.x,
        y: child.y,
        originX: child.x,
        originY: child.y,
      }))
      : [];
    drag = {
      shape: id,
      element,
      offsetX: start.x - shape.x,
      offsetY: start.y - shape.y,
      x: shape.x,
      y: shape.y,
      originX: shape.x,
      originY: shape.y,
      followers,
      lastWrite: 0,
    };
    view.setPointerCapture(event.pointerId);
    event.preventDefault();
  }

  function startHandleDrag(event, handle) {
    const id = handle.dataset.id;
    const item = paintedShape(id);
    if (!item) return;
    drag = {
      handle: handle.dataset.grip,
      shape: id,
      item,
      // The box as it was when the grab started. Resizing against the LIVE
      // box would compound every frame's rounding into a shape that drifts
      // while you hold still.
      box: shapeBounds(item.shape, item.ends, item.path),
      lastWrite: 0,
    };
    view.setPointerCapture(event.pointerId);
    event.preventDefault();
  }

  // Resize/repoint, resolved to a patch. Returns null when the gesture has not
  // moved far enough to mean anything yet.
  function reshapePatch(drag, point) {
    const { item, box, handle } = drag;
    const { shape } = item;

    if (shape.form === "line" || shape.form === "arrow") {
      const ends = item.ends.map(([x, y]) => [x, y]);
      ends[handle === "a" ? 0 : 1] = [point.x, point.y];
      const origin = { x: ends[0][0], y: ends[0][1] };
      const patch = {
        id: shape.id,
        x: Math.round(origin.x),
        y: Math.round(origin.y),
        points: encodePoints(ends.map(([x, y]) => [x - origin.x, y - origin.y])),
      };
      // Dropping an end on a card binds it there; dropping it on empty space
      // lets go. Both directions have to exist or a binding is a one-way
      // door, which is the specific thing the README called out as missing.
      const over = bindingTargetUnder({ clientX: drag.clientX, clientY: drag.clientY });
      patch[handle === "a" ? "from" : "to"] = over || "";
      return patch;
    }

    if (shape.form === "text") {
      return { id: shape.id, w: Math.max(MIN_SIZE, Math.round(point.x - box.x)) };
    }

    const left = handle === "nw" || handle === "sw" ? point.x : box.x;
    const top = handle === "nw" || handle === "ne" ? point.y : box.y;
    const right = handle === "ne" || handle === "se" ? point.x : box.x + box.w;
    const bottom = handle === "se" || handle === "sw" ? point.y : box.y + box.h;
    const next = {
      x: Math.min(left, right),
      y: Math.min(top, bottom),
      w: Math.max(MIN_SIZE, Math.abs(right - left)),
      h: Math.max(MIN_SIZE, Math.abs(bottom - top)),
    };

    if (isBoxForm(shape.form)) {
      return {
        id: shape.id,
        x: Math.round(next.x),
        y: Math.round(next.y),
        w: Math.round(next.w),
        h: Math.round(next.h),
      };
    }

    // Ink. There is no w/h to write, a stroke IS its points, so the box the
    // handle describes is applied to them as a scale about the new corner.
    if (box.w < 1 || box.h < 1) return null;
    const scaleX = next.w / box.w;
    const scaleY = next.h / box.h;
    const moved = (shape.points || []).map(([px, py]) => [
      next.x + (shape.x + px - box.x) * scaleX,
      next.y + (shape.y + py - box.y) * scaleY,
    ]);
    if (moved.length < 2) return null;
    const origin = moved[0];
    return {
      id: shape.id,
      x: Math.round(origin[0]),
      y: Math.round(origin[1]),
      points: encodePoints(moved.map(([x, y]) => [x - origin[0], y - origin[1]])),
    };
  }

  /// Which frame's grab band a world point is in, innermost first.
  ///
  /// Depth order matters: a nested frame's edge sits inside its parent's
  /// interior, and the human aiming at it means the child.
  function frameAtWorldPoint(point) {
    const hits = [...frameElements.keys()].filter((id) => frameEdgeHit(id, point.x, point.y));
    if (hits.length === 0) return null;
    return hits.sort((left, right) => depthOf(right) - depthOf(left))[0];
  }

  function nodeAtClientPoint(clientX, clientY) {
    const entries = [...nodeElements.values()].reverse();
    return entries.find(({ root }) => {
      const rect = root.getBoundingClientRect();
      return rect.width > 0
        && rect.height > 0
        && clientX >= rect.left
        && clientX <= rect.right
        && clientY >= rect.top
        && clientY <= rect.bottom;
    })?.root || null;
  }

  function startNodeDrag(event, nodeElement) {
    const id = nodeElement.dataset.id;
    const node = atlas.nodes.find((candidate) => candidate.id === id);
    if (!node) return;
    // Excalidraw's three rules, in the order they are decided: shift toggles
    // membership and starts no drag; grabbing something already in the set
    // moves the whole set; grabbing anything else selects just that one.
    if (event.shiftKey) {
      toggleSelected(id);
      event.preventDefault();
      return;
    }
    if (!isSelected(id)) select(id);
    const start = toWorld(event);
    // Companions ride at their own offsets so the set keeps its shape. Only
    // nodes appear here: a shape or constraint may be in the set, and it stays
    // selected, it simply has no node position for this gesture to change.
    const followers = dragCompanions(id)
      .map((member) => atlas.nodes.find((candidate) => candidate.id === member))
      .filter(Boolean)
      .map((member) => ({
        id: member.id,
        element: nodeElements.get(member.id)?.root || null,
        offsetX: start.x - member.x,
        offsetY: start.y - member.y,
        x: member.x,
        y: member.y,
      }));
    drag = {
      id,
      element: nodeElement,
      // A frame has no position of its own to write. Its members carry it, and
      // pushing `style.left` onto the frame element would fight the geometry
      // `renderFrames` derives half a frame later.
      frame: frameElements.has(id),
      followers,
      offsetX: start.x - node.x,
      offsetY: start.y - node.y,
      x: node.x,
      y: node.y,
      startClientX: event.clientX,
      startClientY: event.clientY,
      moved: false,
      lastWrite: 0,
    };
    nodeElement.classList.add("dragging");
    try {
      nodeElement.setPointerCapture(event.pointerId);
    } catch {
      // Synthetic replay events have no live pointer to capture.
    }
    startDragAttention(id);
    event.preventDefault();
  }

  function nodeDragDistance(candidate, event) {
    return Math.hypot(
      event.clientX - candidate.startClientX,
      event.clientY - candidate.startClientY,
    );
  }

  /// Everything that has to travel with the node being dragged.
  ///
  /// Two sources, and the second is what makes a frame behave like a frame:
  /// the rest of the current selection, and every descendant of any container
  /// being moved. Positions are absolute, so listing a parent and its child
  /// both is correct rather than a double move: each gets the same delta once.
  function dragCompanions(id) {
    const movers = new Set([id, ...descendantsOf(id)]);
    for (const member of selection) {
      if (member === id) continue;
      movers.add(member);
      for (const child of descendantsOf(member)) movers.add(child);
    }
    movers.delete(id);
    return [...movers];
  }

  function dragPatches(candidate) {
    return [
      { id: candidate.id, x: candidate.x, y: candidate.y },
      ...(candidate.followers || []).map((follower) => ({
        id: follower.id,
        x: follower.x,
        y: follower.y,
      })),
    ];
  }

  // ── marquee: one drag, a set of things ───────────────────────────────

  function startMarquee(event) {
    const box = element("div", "atlas-marquee");
    box.hidden = true;
    view.append(box);
    drag = {
      marquee: box,
      // Shift keeps what was already selected and adds to it, the same rule
      // shift-click follows, so the two ways of building a set agree.
      base: event.shiftKey ? [...selection] : [],
      pane: view.getBoundingClientRect(),
      startClientX: event.clientX,
      startClientY: event.clientY,
      clientX: event.clientX,
      clientY: event.clientY,
      preview: [],
      moved: false,
    };
    view.setPointerCapture(event.pointerId);
    event.preventDefault();
  }

  function marqueeRect(candidate) {
    return {
      left: Math.min(candidate.startClientX, candidate.clientX),
      top: Math.min(candidate.startClientY, candidate.clientY),
      right: Math.max(candidate.startClientX, candidate.clientX),
      bottom: Math.max(candidate.startClientY, candidate.clientY),
    };
  }

  /// Every selectable object and the box it occupies ON SCREEN.
  ///
  /// Screen coordinates rather than world ones, deliberately. The human drew
  /// the box over what they can see, and cards, drawn shapes and constraint
  /// glyphs live in three different coordinate systems (a CSS-transform
  /// camera and two SVG layers). Asking the DOM where each one actually landed
  /// is the single answer all three agree on, and it stays correct at any zoom
  /// without a second opinion about the camera.
  function selectableBoxes() {
    const boxes = [];
    for (const [id, entry] of nodeElements) {
      boxes.push([id, entry.root.getBoundingClientRect()]);
    }
    for (const group of world.querySelectorAll(".shape")) {
      if (group.dataset.id) boxes.push([group.dataset.id, group.getBoundingClientRect()]);
    }
    for (const [id, group] of constraintTargetElements) {
      if (group.isConnected) boxes.push([id, group.getBoundingClientRect()]);
    }
    return boxes;
  }

  // Excalidraw parity: touching the box is enough, full containment is not
  // required. A human who drags a box across three cards means those three.
  function marqueeHits(candidate) {
    const box = marqueeRect(candidate);
    const hits = [];
    for (const [id, rect] of selectableBoxes()) {
      if (rect.width <= 0 && rect.height <= 0) continue;
      const touches = rect.right >= box.left
        && rect.left <= box.right
        && rect.bottom >= box.top
        && rect.top <= box.bottom;
      if (touches && !hits.includes(id)) hits.push(id);
    }
    // A frame follows the same rule its pointer does: the border is the frame,
    // the interior is whatever the interior holds. A box drawn wholly inside a
    // container therefore selects the members it crossed and NOT the container,
    // which is what lets a human say "these two of the six" at all. A box that
    // crosses the border means the container as well.
    for (const [id, frame] of frameElements) {
      const rect = frame.root.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) continue;
      const touches = rect.right >= box.left
        && rect.left <= box.right
        && rect.bottom >= box.top
        && rect.top <= box.bottom;
      if (!touches) continue;
      const edge = FRAME_EDGE * camera.scale;
      const header = FRAME_HEADER_H * camera.scale;
      const whollyInside = box.left > rect.left + edge
        && box.right < rect.right - edge
        && box.top > rect.top + header + edge
        && box.bottom < rect.bottom - edge;
      if (!whollyInside && !hits.includes(id)) hits.push(id);
    }
    return hits;
  }

  // Preview only. The real selection is published once, on release, so a drag
  // across a board does not post a new gesture every animation frame.
  function previewMarquee(candidate, hits) {
    for (const id of candidate.preview) {
      if (!hits.includes(id)) targetElement(id)?.classList.remove("marquee-hit");
    }
    for (const id of hits) targetElement(id)?.classList.add("marquee-hit");
    candidate.preview = hits;
  }

  function learnerTargetFromEvent(event) {
    const nodeElement = nodeAtClientPoint(event.clientX, event.clientY);
    if (nodeElement?.dataset.id) return nodeElement.dataset.id;
    const constraintElement = event.target.closest?.("[data-constraint-id]");
    if (constraintElement?.dataset.constraintId) return constraintElement.dataset.constraintId;
    const shapeElement = event.target.closest?.(".shape");
    if (shapeElement?.dataset.id) return shapeElement.dataset.id;
    const frameElement = event.target.closest?.(".atlas-frame");
    if (frameElement?.dataset.id) return frameElement.dataset.id;
    return null;
  }

  function answerFromLearnerTarget(targetId, event) {
    const flow = atlas.explanation;
    if (!targetId || !flow) return;
    const beat = flow.definition.beats.find((candidate) => candidate.id === flow.state.current_beat);
    if (!beat) return;
    const transition = flow.state.status === "active"
      ? directTransitionForTarget(beat, targetId)
      : null;
    focusLearnerTarget(targetId, !transition);
    if (transition) {
      void submitDirectExplanation(flow, beat, transition, event, {
        source: "canvas-target",
        selectedTargetIds: [targetId],
      });
    }
  }

  view.addEventListener("click", (event) => {
    if (!learnerMode) return;
    if (event.target.closest?.(".zoom-controls")) return;
    const targetId = learnerTargetFromEvent(event);
    if (!targetId) return;
    event.preventDefault();
    answerFromLearnerTarget(targetId, event);
  }, { capture: true });

  view.addEventListener("pointerdown", (event) => {
    // Button 1 is the middle button, which every canvas tool treats as pan.
    // It is admitted here and nowhere else: everything below this line still
    // requires the primary button.
    if (event.button !== 0 && event.button !== 1) return;
    // The toolbar lives INSIDE the view, so its buttons are on this handler's
    // path. Falling through to the pan branch below called
    // `view.setPointerCapture()`, and a captured pointer retargets `pointerup`
    // AND the `click` that follows it to the capturing element, so the button
    // under the cursor never saw the click and no tool was ever selected by
    // mouse. The keyboard shortcuts kept working, which is exactly why this
    // read as "the palette is decorative" rather than as a bug.
    if (event.target.closest?.(".draw-tools, .zoom-controls, .minimap, .gesture-card, .atlas-changes, .source-popover, .atlas-context-menu, .agent-ink-panel, .segment-flap-panel, .segment-tree-panel, .segment-picker, .inline-input, .inline-direct-editor")) return;
    if (learnerMode) {
      if (event.button === 1 || spaceHeld) return startPan(event);
      if (learnerTargetFromEvent(event)) {
        event.preventDefault();
        return;
      }
      return startPan(event);
    }
    // Hand first: with it armed, a left drag moves the board wherever it
    // starts, over a card as readily as over empty canvas. That is the whole
    // of the tool, and checking it after the card hit test would make it a
    // tool that works everywhere except on the thing you are looking at.
    if (event.button === 1 || spaceHeld || active.tool === "hand") return startPan(event);
    if (active.tool === "draw") return startSketch(event);

    const handle = event.target.closest?.(".handle");
    if (handle) return startHandleDrag(event, handle);

    // The visible card wins even when a foreground stroke crosses it. DOM
    // stacking cannot express that semantic priority because strokes still
    // need to render above cards. Geometry can, deterministically.
    const nodeElement = nodeAtClientPoint(event.clientX, event.clientY);
    if (nodeElement) {
      if (event.target.closest?.(".node-source")) {
        if (event.shiftKey) toggleSelected(nodeElement.dataset.id);
        else select(nodeElement.dataset.id);
        return;
      }
      return startNodeDrag(event, nodeElement);
    }

    const markTarget = event.target.closest?.("[data-mark-target]")?.dataset.markTarget;
    if (markTarget) {
      if (event.shiftKey) toggleSelected(markTarget);
      else select(markTarget);
      event.preventDefault();
      return;
    }

    // Constraint glyphs own their click and select through renderConstraints.
    // Returning here prevents the pane from capturing the pointer as a pan,
    // which would retarget the click away from the glyph.
    if (event.target.closest?.("[data-constraint-id]")) return;

    const shapeElement = event.target.closest?.(".shape");
    if (shapeElement) {
      if (event.target.closest?.(".segment-badge")) {
        select(shapeElement.dataset.id);
        event.preventDefault();
        return;
      }
      if (event.shiftKey) {
        toggleSelected(shapeElement.dataset.id);
        event.preventDefault();
        return;
      }
      return startShapeDrag(event, shapeElement);
    }

    // A frame is held by its edge and its title, never by its interior.
    // Grabbing the interior would mean a human could not marquee inside a
    // container or drag a card out of one without first aiming around the
    // box that owns it, which is the opposite of what a frame is for.
    const frameId = frameAtWorldPoint(toWorld(event));
    if (frameId) {
      if (event.shiftKey) {
        toggleSelected(frameId);
        event.preventDefault();
        return;
      }
      const frame = frameElements.get(frameId);
      if (frame) return startNodeDrag(event, frame.root);
    }

    // Select draws a marquee; hand, Space, and middle mouse pan. Shift adds
    // to the existing selection. The same gestures work on empty frame interiors.
    startMarquee(event);
  });

  function startPan(event, clearOnClick = false) {
    view.focus({ preventScroll: true });
    // The pan carries the last pointer position, not an origin offset against
    // the camera: the delta is what the core takes, and it is the only shape
    // of this gesture that survives the camera moving underneath it.
    drag = { pan: true, clearOnClick, moved: false, startX: event.clientX, startY: event.clientY, lastX: event.clientX, lastY: event.clientY };
    view.classList.add("panning");
    view.setPointerCapture(event.pointerId);
    event.preventDefault();
  }

  view.addEventListener("contextmenu", (event) => {
    if (learnerMode) {
      event.preventDefault();
      return;
    }
    if (event.target.closest?.(".atlas-context-menu")) {
      event.preventDefault();
      return;
    }
    const nodeElement = nodeAtClientPoint(event.clientX, event.clientY);
    if (!nodeElement) return;
    openContextMenu(nodeElement, event);
  });
  document.addEventListener("pointerdown", (event) => {
    if (contextMenu && !event.target.closest?.(".atlas-context-menu")) closeContextMenu();
  }, true);

  view.addEventListener("pointermove", (event) => {
    if (sketching) return moveSketch(event);
    if (!drag) return;
    if (drag.marquee) {
      drag.clientX = event.clientX;
      drag.clientY = event.clientY;
      // The same threshold a card drag uses. Below it this is still a click on
      // empty canvas, and a click must not paint a box the human did not draw.
      if (!drag.moved && nodeDragDistance(drag, event) < NODE_DRAG_THRESHOLD_PX) return;
      drag.moved = true;
      const box = marqueeRect(drag);
      drag.marquee.style.left = `${box.left - drag.pane.left}px`;
      drag.marquee.style.top = `${box.top - drag.pane.top}px`;
      drag.marquee.style.width = `${box.right - box.left}px`;
      drag.marquee.style.height = `${box.bottom - box.top}px`;
      drag.marquee.hidden = false;
      previewMarquee(drag, marqueeHits(drag));
      return;
    }
    if (drag.pan) {
      if (Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) >= NODE_DRAG_THRESHOLD_PX) drag.moved = true;
      atlasCamera.pan(event.clientX - drag.lastX, event.clientY - drag.lastY);
      drag.lastX = event.clientX;
      drag.lastY = event.clientY;
      applyCamera();
      scheduleAttention();
      return;
    }

    if (drag.handle) {
      // Kept for `nodeUnder`, which needs client coordinates to ask the DOM
      // what is under the pointer rather than recomputing hit-testing here.
      drag.clientX = event.clientX;
      drag.clientY = event.clientY;
      drag.patch = reshapePatch(drag, toWorld(event));
      const now = performance.now();
      if (drag.patch && now - drag.lastWrite > DRAG_WRITE_MS) {
        drag.lastWrite = now;
        placeShape(drag.patch);
      }
      return;
    }

    if (drag.shape) {
      const point = toWorld(event);
      drag.x = Math.round(point.x - drag.offsetX);
      drag.y = Math.round(point.y - drag.offsetY);
      const deltaX = drag.x - drag.originX;
      const deltaY = drag.y - drag.originY;
      // Preview by translating the group we already drew. A pure translation
      // of the committed geometry, not a second opinion about where the shape
      // is, the same deal a card's `style.left` preview is on.
      drag.element.setAttribute(
        "transform",
        `translate(${drag.x - drag.originX} ${drag.y - drag.originY})`,
      );
      for (const follower of drag.followers || []) {
        follower.x = follower.originX + deltaX;
        follower.y = follower.originY + deltaY;
        follower.element?.setAttribute("transform", `translate(${deltaX} ${deltaY})`);
      }
      const now = performance.now();
      if (now - drag.lastWrite > DRAG_WRITE_MS) {
        drag.lastWrite = now;
        placeShapeSet([
          { id: drag.shape, x: drag.x, y: drag.y },
          ...(drag.followers || []).map((follower) => ({
            id: follower.id,
            x: follower.x,
            y: follower.y,
          })),
        ]);
      }
      return;
    }

    if (!drag.moved && nodeDragDistance(drag, event) < NODE_DRAG_THRESHOLD_PX) return;
    drag.moved = true;
    const point = toWorld(event);
    drag.x = Math.round(point.x - drag.offsetX);
    drag.y = Math.round(point.y - drag.offsetY);
    // Paint locally at pointer rate, publish to the CRDT at a calmer rate:
    // every committed transaction is a frame on the wire and a merge on the
    // host, and the host re-clones its whole document per inbound frame.
    if (!drag.frame) {
      drag.element.style.left = `${drag.x}px`;
      drag.element.style.top = `${drag.y}px`;
    }
    for (const follower of drag.followers || []) {
      follower.x = Math.round(point.x - follower.offsetX);
      follower.y = Math.round(point.y - follower.offsetY);
      if (!follower.element) continue;
      follower.element.style.left = `${follower.x}px`;
      follower.element.style.top = `${follower.y}px`;
    }
    // Frames are measured from the cards, so they have to be recomputed after
    // the cards move and before the human sees the next paint.
    renderFrames();
    renderEdges();
    const now = performance.now();
    if (now - drag.lastWrite > DRAG_WRITE_MS) {
      drag.lastWrite = now;
      placeSet(dragPatches(drag));
    }
  });

  function endDrag(event) {
    if (sketching) {
      try {
        view.releasePointerCapture(event.pointerId);
      } catch {
        // Already released; the commit below is what matters.
      }
      endSketch(event);
      return;
    }
    if (!drag) return;
    const endedNodeDrag = !drag.pan && !drag.handle && !drag.shape && !drag.marquee;
    if (drag.marquee) {
      previewMarquee(drag, []);
      drag.marquee.remove();
      // The one publish for this gesture. A box that moved selects what it
      // touched; a click on empty canvas publishes the clear that pointerdown
      // only did locally. Either way the host hears about this once.
      //
      // F9's rule still holds: neither branch writes to the document.
      selectSet(drag.moved ? [...drag.base, ...marqueeHits(drag)] : drag.base);
    } else if (drag.pan) {
      view.classList.remove("panning");
      if (drag.clearOnClick && !drag.moved && event.type === "pointerup") select(null);
    } else if (drag.handle) {
      if (drag.patch) {
        placeShape(drag.patch);
        captureAgentInk(drag.patch.id, drag.patch);
      }
    } else if (drag.shape) {
      drag.element.removeAttribute("transform");
      for (const follower of drag.followers || []) follower.element?.removeAttribute("transform");
      const patches = [
        { id: drag.shape, x: drag.x, y: drag.y },
        ...(drag.followers || []).map((follower) => ({
          id: follower.id,
          x: follower.x,
          y: follower.y,
        })),
      ];
      placeShapeSet(patches);
      for (const patch of patches) captureAgentInk(patch.id, patch);
    } else {
      drag.element.classList.remove("dragging");
      if (!drag.moved && nodeDragDistance(drag, event) >= NODE_DRAG_THRESHOLD_PX) {
        const point = toWorld(event);
        drag.x = Math.round(point.x - drag.offsetX);
        drag.y = Math.round(point.y - drag.offsetY);
        drag.moved = true;
      }
      if (drag.moved) {
        const patches = dragPatches(drag);
        placeSet(patches);
        for (const patch of patches) captureAgentInk(patch.id, patch);
      }
    }
    if (event && event.pointerId !== undefined) {
      try {
        view.releasePointerCapture(event.pointerId);
      } catch {
        // The capture may already be gone; nothing to release.
      }
    }
    drag = null;
    if (endedNodeDrag) {
      endDragAttention();
      view.focus({ preventScroll: true });
    }
  }

  view.addEventListener("pointerup", endDrag);
  view.addEventListener("pointercancel", endDrag);

  // Space is only a pan modifier while nobody is typing it into something.
  // Listening on the document rather than the view is deliberate: the human
  // may be holding space before the pointer ever enters the canvas.
  document.addEventListener("keydown", (event) => {
    if (event.code !== "Space" || focusedTextEntry()) return;
    spaceHeld = true;
    view.classList.add("pan-ready");
  });
  document.addEventListener("keyup", (event) => {
    if (event.code !== "Space") return;
    spaceHeld = false;
    view.classList.remove("pan-ready");
  });
  window.addEventListener("blur", () => {
    spaceHeld = false;
    view.classList.remove("pan-ready");
  });

  view.addEventListener("dblclick", (event) => {
    if (learnerMode) {
      event.preventDefault();
      return;
    }
    if (event.target.closest?.(".draw-tools, .zoom-controls, .minimap, .gesture-card, .atlas-changes, .source-popover, .atlas-context-menu, .agent-ink-panel, .segment-flap-panel, .segment-tree-panel, .segment-picker, .inline-input, .inline-direct-editor")) return;
    if (event.target.closest?.("button, [popover]")) return;
    const nodeElement = nodeAtClientPoint(event.clientX, event.clientY);
    if (nodeElement) {
      const id = nodeElement.dataset.id;
      const node = atlas.nodes.find((candidate) => candidate.id === id);
      if (!node) return;
      if (!explorationEditing) {
        openObjectFocus(id);
      } else {
        select(id);
        editNodeLabel(id);
      }
      return;
    }
    const shapeElement = event.target.closest?.(".shape");
    if (shapeElement) {
      select(shapeElement.dataset.id);
      editShapeLabel(
        shapeElement.dataset.id,
        event.target.closest?.(".segment-badge, .shape-text"),
      );
      return;
    }
    // Pointer capture can retarget a title's double click to the canvas. Use
    // the same canonical frame grab-band hit test as dragging.
    const frameId = frameAtWorldPoint(toWorld(event));
    if (frameId && inExplorationView(frameId)) {
      if (explorationEditing) { select(frameId); editNodeLabel(frameId); }
      else openObjectFocus(frameId);
      return;
    }
    const point = toWorld(event);
    const box = view.getBoundingClientRect();
    askFor({
      x: event.clientX - box.left,
      y: event.clientY - box.top,
      placeholder: "new node…",
      onCommit: (label) => {
        const id = place({ label, x: Math.round(point.x), y: Math.round(point.y), tone: "question" });
        if (id) select(id);
      },
    });
  });

  view.addEventListener("keydown", (event) => {
    // A letter typed into an editor is text, never a canvas command. The view
    // itself must own focus before any single-key tool or mark shortcut fires.
    if (event.target !== view) return;
    if (learnerMode) {
      if (event.key === "Escape") select(null);
      return;
    }
    if (!event.metaKey && !event.ctrlKey && !event.altKey && ["[", "]"].includes(event.key)) {
      event.preventDefault();
      stepAltitude(event.key === "[" ? -1 : 1);
      return;
    }
    // Tool shortcuts come first: they have to work with nothing selected,
    // which the old handler's early return made impossible.
    if (!event.metaKey && !event.ctrlKey && event.altKey && event.key.toLowerCase() === "b") {
      event.preventDefault();
      toggleConstraintSalience();
      return;
    }
    if (!event.metaKey && !event.ctrlKey && !event.altKey) {
      const key = event.key.toLowerCase();
      const shortcut = TOOL_DIGITS[key] || key;
      const spec = TOOL_SHORTCUTS.has(shortcut)
        ? TOOLS.find((candidate) => candidate.key === shortcut)
        : null;
      if (spec) {
        event.preventDefault();
        setTool(spec, spec !== active);
        return;
      }
    }
    if (event.key === "Escape") {
      select(null);
      closeInline();
      closeContextMenu();
      setTool(TOOLS[0]);
      return;
    }
    if (Object.prototype.hasOwnProperty.call(GLYPH_MEANING, event.key)) {
      event.preventDefault();
      if (!selected) {
        reportRejectedGesture(event.key, null, "select a card before using this shortcut");
        return;
      }
      openMarkEditor(selected, event.key);
      return;
    }
    if (!selected) return;
    if (event.key === "Backspace" || event.key === "Delete") {
      event.preventDefault();
      const doomed = [...selection];
      select(null);
      write(() => {
        for (const id of doomed) doc.remove(id);
      });
      return;
    }
    // `Enter` on a selected shape opens its caption. The other half of "draw
    // it, then type", the half that still works after you dismissed the
    // caret, or clicked something you drew an hour ago.
    //
    // Deliberately Enter and not "any letter": the single-letter keys are the
    // tool palette, and quietly stealing them whenever something is selected
    // would trade one broken affordance for another.
    if (paintedShape(selected)) {
      if (event.key === "Enter") {
        event.preventDefault();
        editShapeLabel(selected);
      }
      return;
    }
    if (atlas.nodes.some((node) => node.id === selected) && event.key === "Enter") {
      event.preventDefault();
      editNodeLabel(selected);
    }
  });

  // ── "do we agree?", compare both read-backs ────────────────────────

  const agreeButton = document.getElementById("agree-button");
  const agreeDialog = document.getElementById("agree-dialog");
  agreeButton?.addEventListener("click", async () => {
    const mine = doc.describe();
    let theirs = "";
    try {
      const response = await fetch("/atlas/describe", { headers: { accept: "application/json" } });
      const payload = await response.json();
      theirs = payload.ok ? payload.text : `error: ${payload.error}`;
    } catch (error) {
      theirs = `error: ${error}`;
    }
    const verdict = document.getElementById("agree-verdict");
    const same = mine.trim() === theirs.trim();
    verdict.textContent = same
      ? "Identical. Your replica and the agent's read-back are the same text, character for character."
      : "Different. The two replicas have not converged, or one is mid-sync.";
    verdict.className = same ? "verdict-same" : "verdict-different";
    document.getElementById("agree-mine").textContent = mine;
    document.getElementById("agree-theirs").textContent = theirs;
    agreeDialog.showModal();
  });
  document.getElementById("close-agree")?.addEventListener("click", () => agreeDialog.close());

  // ── .excalidraw, both directions ────────────────────────────────────
  //
  // The forms, the point convention and the arrow bindings were kept
  // compatible with Excalidraw's element schema from the start. This is the
  // part that cashes that in: a diagram made elsewhere arrives as real,
  // movable shapes rather than as a picture of one.
  //
  // Both directions go through the replica, not through the host. An import
  // is something a person did, and posting it to a server endpoint would make
  // it land as somebody else's write, quietly breaking the one property this
  // whole example exists to demonstrate.

  document.getElementById("export-button")?.addEventListener("click", () => {
    let document_text;
    try {
      document_text = doc.export_excalidraw();
    } catch (error) {
      report(`could not export: ${error}`);
      return;
    }
    const url = URL.createObjectURL(new Blob([document_text], { type: "application/json" }));
    const link = element("a");
    link.href = url;
    link.download = "atlas.excalidraw";
    link.click();
    URL.revokeObjectURL(url);
    report("exported this page as atlas.excalidraw");
  });

  const importFile = document.getElementById("import-file");
  document.getElementById("import-button")?.addEventListener("click", () => importFile?.click());
  importFile?.addEventListener("change", async () => {
    const file = importFile.files?.[0];
    importFile.value = ""; // so re-picking the same file fires `change` again
    if (!file) return;
    let summary;
    try {
      summary = JSON.parse(doc.import_excalidraw(await file.text()));
    } catch (error) {
      report(`could not import: ${error}`);
      return;
    }
    flush();
    scheduleRender();
    // Never "imported ✓" on its own: an element that could not come across is
    // named, because a document that half-arrived while the page said success
    // is exactly the failure this surface is built to make impossible. What
    // landed changed (a bound arrow arriving loose) is a different sentence
    // from "not brought", and the two are never mixed.
    const parts = [`imported ${summary.landed} shape(s) from ${file.name}`];
    if (summary.bound_connectors) {
      parts.push(`preserved bindings on ${summary.bound_connectors} connector(s)`);
    }
    if (summary.skipped.length) {
      parts.push(`${summary.skipped.length} not brought, ${summary.skipped[0]}`);
    }
    if ((summary.notes || []).length) {
      parts.push(summary.notes[0]);
    }
    report(parts.join("; "));
  });

  applyCamera();
  const attentionResizeObserver = new ResizeObserver(() => {
    applyPanelLayout();
    fitCameraToContent(Boolean(openedExploration && !explorationEditing));
    revealActiveWalkthroughStop();
    scheduleAttention();
  });
  attentionResizeObserver.observe(view);
  scheduleAttention();
  view.focus();

  // A hackable surface should be inspectable: the live replica is reachable
  // from the console for anyone poking at it (including an agent driving a
  // real browser to check convergence).
  window.atlas = {
    doc,
    openSource: id => {
      const node = atlas.nodes.find(candidate => candidate.id === id);
      if (node) openSourcePopover(node);
    },
    openDecisionHistory: () => openDecisionHistory(ctx),
    setContext: (input) => write(() => doc.set_context(JSON.stringify(input)), true),
    proposeArchitecture: (input) => write(() => doc.propose_architecture(JSON.stringify(input)), true),
    architectureReview: () => JSON.parse(doc.architecture_review()),
    saveDecisionDraft: (input) => write(() => doc.save_decision_draft(JSON.stringify(input)), true),
    get atlas() { return atlas; },
    camera,
    get focus() { return { subject: focusedObject, visible: focusVisible ? [...focusVisible] : null, exploration: openedExploration }; },
    place,
    markNode: markTarget,
    markTarget,
    removeObject,
    typedDiagram: {
      setKind: (id, kind) => write(() => doc.set_diagram_kind(id, kind)),
      setStateRoles: (id, initial = false, terminal = false) => (
        write(() => doc.set_state_roles(id, Boolean(initial), Boolean(terminal)))
      ),
      transition: (from, to, event, guard = undefined) => (
        write(() => doc.transition(from, to, event, guard))
      ),
      setCollapsed: (id, collapsed = true) => setContainerCollapsed(id, Boolean(collapsed)),
      collapsed: () => [...collapsedContainers],
      renderedTransitions: () => [...edgeLayer.querySelectorAll(".atlas-transition-edge")]
        .map((item) => ({
          id: item.dataset.edgeId,
          back: item.classList.contains("atlas-transition-back-edge"),
        })),
    },
    interaction: {
      active: () => drag ? {
        shape: drag.shape || null,
        node: drag.id || null,
        x: Number.isFinite(drag.x) ? drag.x : null,
        y: Number.isFinite(drag.y) ? drag.y : null,
        originX: Number.isFinite(drag.originX) ? drag.originX : null,
        originY: Number.isFinite(drag.originY) ? drag.originY : null,
        followers: (drag.followers || []).map((follower) => follower.id),
      } : null,
      editor: () => inlineDirect
        ? {
          kind: "direct",
          value: inlineDirect.element.textContent || "",
          ariaLabel: inlineDirect.element.getAttribute("aria-label"),
        }
        : inlineInput
          ? {
            kind: "draft",
            value: inlineInput.querySelector("input")?.value || "",
            placeholder: inlineInput.querySelector("input")?.placeholder || "",
          }
          : null,
    },
    learning: {
      workspaceUrl: () => learnerWorkspaceUrl().href,
      catalog: () => JSON.parse(JSON.stringify(learnerCatalog)),
      visibleTopics: () => visibleWorkspaceTopics().map((topic) => ({ ...topic })),
      requests: () => tangentRequestLog.map((request) => ({ ...request })),
      remove: (id) => {
        hideTangent(String(id || ""));
        renderLearningWorkspace();
      },
    },
    segment: {
      snapshot: () => JSON.parse(doc.snapshot()).shapes.filter((shape) => shape.form === "segment"),
      receipts: () => Object.fromEntries(segmentReceipts),
      results: () => Object.fromEntries(segmentResults),
      errors: () => [...segmentErrors],
      inputEvents: () => [...segmentInputEvents],
      picker: () => segmentPicker?.receipt() || null,
      replay: (ownerId) => replaySegmentMotion(ownerId),
      inflight: () => Object.fromEntries(segmentInflight),
      rendered: () => [...view.querySelectorAll(".segment-object")].map((element) => ({
        status: element.dataset.segmentStatus,
        id: element.dataset.segmentId,
        parentId: element.dataset.segmentParentId || null,
        pivot: element.dataset.segmentPivot,
        motion: element.dataset.motionOwner ? {
          ownerId: element.dataset.motionOwner,
          label: element.dataset.motionLabel,
          track: element.dataset.motionTrack,
          targetIndex: Number(element.dataset.motionTargetIndex),
          animationId: element.dataset.motionAnimationId,
        } : null,
        computedTransform: getComputedStyle(element.closest("g.shape") || element).transform,
        computedOpacity: getComputedStyle(element.closest("g.shape") || element).opacity,
        computedVisibility: getComputedStyle(element.closest("g.shape") || element).visibility,
        badge: element.closest("g.shape")
          ?.querySelector(":scope > foreignObject.segment-badge-frame .segment-badge")
          ?.textContent || "",
        canvas: {
          width: element.querySelector(".segment-body-canvas")?.width || 0,
          height: element.querySelector(".segment-body-canvas")?.height || 0,
          maskPainted: element.querySelector(".segment-body-canvas")?.dataset.maskPainted || "false",
          paintError: element.querySelector(".segment-body-canvas")?.dataset.paintError || null,
          wingsSubtracted: element.querySelector(".segment-body-canvas")?.dataset.wingsSubtracted || "0",
          hierarchyPartsSubtracted: element.querySelector(".segment-body-canvas")?.dataset.hierarchyPartsSubtracted || "0",
          sourcePreserved: element.querySelector(".segment-body-canvas")?.dataset.sourcePreserved || "false",
          redundantSourceOverlay: element.querySelector(".segment-body-canvas")?.dataset.redundantSourceOverlay || "false",
        },
        wings: [...element.querySelectorAll(".segment-wing-canvas")].map((wing) => ({
          role: wing.dataset.segmentRole,
          width: wing.width,
          height: wing.height,
          maskPainted: wing.dataset.maskPainted || "false",
          flap: wing.dataset.flap || "0",
          transform: wing.style.transform,
        })),
        slider: {
          value: view.querySelector(".segment-flap-slider")?.value || null,
          visible: !view.querySelector(".segment-flap-panel")?.hidden,
        },
        animations: (element.closest("g.shape")?.getAnimations() || []).map((animation) => ({
          id: animation.id || animation.animationName || "running",
          playState: animation.playState,
          currentTime: animation.currentTime,
        })),
      })),
      tree: () => [...view.querySelectorAll(".segment-tree-item")].map((item) => ({
        id: item.dataset.segmentId,
        label: item.textContent,
        part: item.classList.contains("segment-tree-part"),
        depth: Number(item.dataset.depth || 0),
        hasChildren: item.dataset.hasChildren === "true",
        expanded: item.dataset.hasChildren === "true"
          ? item.getAttribute("aria-expanded") === "true"
          : null,
        selected: item.classList.contains("selected"),
      })),
      collapsed: () => [...collapsedSegments],
    },
    explanation: {
      snapshot: () => JSON.parse(doc.snapshot()).explanation || null,
      actionLog: () => [...explanationActionLog],
      inputEvents: () => [...explanationInputEvents],
      setClockMode: (mode) => setExplanationClockMode(mode),
      cueProgress: (update) => reportExplanationCueProgress(update),
      clock: () => ({ ...explanationClock }),
      panel: () => explanationPanel.hidden ? null : {
        flowId: explanationPanel.dataset.flowId,
        beatId: explanationPanel.dataset.beatId,
        revision: Number(explanationPanel.dataset.revision || 0),
        status: explanationPanel.dataset.status,
        text: explanationPanel.textContent,
      },
      navigation: {
        view: () => learnerView,
        catalog: () => JSON.parse(JSON.stringify(learnerCatalog)),
        open: (destination) => setLearnerView(destination),
      },
      },
    attentionResizeObserver,
    get selection() {
      return selected
        ? {
          id: selected,
          label: titleForTarget(selected),
          count: selection.length,
          ids: [...selection],
          labels: selectionLabels(),
          summary: selectionSummary(),
        }
        : null;
    },
    get schema() {
      return { host: hostSchemaVersion, wasm: wasmSchemaVersion, ready: schemaReady };
    },
  };
  return window.atlas;
}

const segmentSourceImages = new Map();

async function sourceImage(url) {
  const existing = segmentSourceImages.get(url);
  if (existing) return existing;
  const loading = new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error(`could not render segment source ${url}`));
    image.src = url;
  });
  segmentSourceImages.set(url, loading);
  try {
    return await loading;
  } catch (error) {
    segmentSourceImages.delete(url);
    throw error;
  }
}

function positionSegmentPart(canvas, part, bodyBox) {
  const [bodyLeft, bodyTop, bodyRight, bodyBottom] = bodyBox;
  const [left, top, right, bottom] = part.mask_source_box;
  const bodyWidth = bodyRight - bodyLeft;
  const bodyHeight = bodyBottom - bodyTop;
  canvas.style.left = `${((left - bodyLeft) / bodyWidth) * 100}%`;
  canvas.style.top = `${((top - bodyTop) / bodyHeight) * 100}%`;
  canvas.style.width = `${((right - left) / bodyWidth) * 100}%`;
  canvas.style.height = `${((bottom - top) / bodyHeight) * 100}%`;
  canvas.style.transformOrigin = part.role === "left-wing" ? "88% 12%" : "12% 12%";
}

function applySegmentFlapElement(element, value) {
  const flap = Math.max(-1, Math.min(1, Number(value) || 0));
  for (const wing of element.querySelectorAll(".segment-wing-canvas")) {
    const direction = wing.dataset.segmentRole === "left-wing" ? 1 : -1;
    wing.style.transform = `rotate(${direction * flap * 35}deg)`;
    wing.dataset.flap = String(flap);
  }
}

function applySegmentFlap(id, value) {
  const element = [...document.querySelectorAll(".segment-object")]
    .find((candidate) => candidate.dataset.segmentId === id);
  if (element) applySegmentFlapElement(element, value);
}

function protectSegmentJoint(mask, origin) {
  if (Math.min(mask.width, mask.height) < 64) return mask;
  const protectedMask = document.createElement("canvas");
  protectedMask.width = mask.width;
  protectedMask.height = mask.height;
  const context = protectedMask.getContext("2d");
  if (!context) throw new Error("segment joint mask context is unavailable");
  context.drawImage(mask, 0, 0);
  const x = (origin?.[0] ?? 0.5) * mask.width;
  const y = (origin?.[1] ?? 0) * mask.height;
  const radius = Math.max(2, Math.min(mask.width, mask.height) * 0.16);
  context.globalCompositeOperation = "destination-out";
  context.beginPath();
  context.arc(x, y, radius, 0, Math.PI * 2);
  context.fill();
  context.globalCompositeOperation = "source-over";
  return protectedMask;
}

async function paintSegmentComposition(
  bodyCanvas,
  wingEntries,
  shape,
  childSegments = [],
  overlayTexture = false,
) {
  try {
    const image = await sourceImage(shape.segment_source_url);
    const body = {
      mask_width: shape.segment_mask_width,
      mask_height: shape.segment_mask_height,
      mask_runs: shape.segment_mask_runs,
      mask_source_box: shape.segment_mask_source_box,
    };
    const preserveOwnerImage = overlayTexture || (shape.segment_motion?.schema === "atlas-segment-motion-v1"
      && Array.isArray(shape.segment_motion?.tracks)
      && shape.segment_motion.child_compositing === "overlay");
    const bodyMask = preserveOwnerImage
      ? paintSegmentSource(bodyCanvas, body, image, shape.segment_occlusion_box)
      : paintSegmentMask(bodyCanvas, body, image, shape.segment_occlusion_box);
    const cutouts = wingEntries.map(([canvas, part]) => ({
      material: part,
      mask: paintSegmentMask(canvas, part, image, null),
      expansion: 4,
    }));
    for (const child of childSegments) {
      const scratch = document.createElement("canvas");
      const rawMask = paintSegmentMask(scratch, child.material, image, null);
      cutouts.push({
        material: child.material,
        mask: protectSegmentJoint(rawMask, child.jointOrigin),
        expansion: 0,
      });
    }
    if (!bodyCanvas.isConnected) return;
    const context = bodyCanvas.getContext("2d");
    if (!context) throw new Error("segment body canvas 2D context is unavailable");
    const [bodyLeft, bodyTop, bodyRight, bodyBottom] = body.mask_source_box;
    const bodySourceWidth = bodyRight - bodyLeft;
    const bodySourceHeight = bodyBottom - bodyTop;
    context.globalCompositeOperation = "destination-out";
    for (const { material, mask, expansion } of cutouts) {
      const [left, top, right, bottom] = material.mask_source_box;
      const x = ((left - bodyLeft) / bodySourceWidth) * bodyCanvas.width;
      const y = ((top - bodyTop) / bodySourceHeight) * bodyCanvas.height;
      const width = ((right - left) / bodySourceWidth) * bodyCanvas.width;
      const height = ((bottom - top) / bodySourceHeight) * bodyCanvas.height;
      const offsets = expansion > 0
        ? [
          [0, 0], [-expansion, 0], [expansion, 0], [0, -expansion], [0, expansion],
          [-3, -3], [3, -3], [-3, 3], [3, 3],
        ]
        : [[0, 0]];
      for (const [offsetX, offsetY] of offsets) {
        context.drawImage(mask, x + offsetX, y + offsetY, width, height);
      }
    }
    context.globalCompositeOperation = "source-over";
    bodyCanvas.dataset.maskPainted = "true";
    bodyCanvas.dataset.wingsSubtracted = String(wingEntries.length);
    bodyCanvas.dataset.hierarchyPartsSubtracted = String(childSegments.length);
    bodyCanvas.dataset.wingCutoutExpansion = "4";
    bodyCanvas.dataset.sourcePreserved = String(preserveOwnerImage);
    void bodyMask;
  } catch (error) {
    bodyCanvas.dataset.maskPainted = "false";
    bodyCanvas.dataset.paintError = error instanceof Error ? error.message : String(error);
    console.error("Atlas segment texture render failed", error);
  }
}

function paintSegmentSource(canvas, materialization, image, occlusion) {
  const maskWidth = materialization.mask_width;
  const maskHeight = materialization.mask_height;
  const [left, top, right, bottom] = materialization.mask_source_box;
  if (![maskWidth, maskHeight, left, top, right, bottom].every(Number.isFinite)) {
    throw new Error("segment source geometry is incomplete");
  }
  const width = Math.max(1, Math.round(right - left));
  const height = Math.max(1, Math.round(bottom - top));
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("segment source canvas 2D context is unavailable");
  context.clearRect(0, 0, width, height);
  context.drawImage(image, left, top, right - left, bottom - top, 0, 0, width, height);
  if (Array.isArray(occlusion) && occlusion.length === 4) {
    context.fillStyle = "rgb(22, 31, 50)";
    context.fillRect(
      ((occlusion[0] - left) / (right - left)) * width,
      ((occlusion[1] - top) / (bottom - top)) * height,
      ((occlusion[2] - occlusion[0]) / (right - left)) * width,
      ((occlusion[3] - occlusion[1]) / (bottom - top)) * height,
    );
  }
  canvas.dataset.maskPainted = "true";
  return canvas;
}

function paintSegmentMask(canvas, materialization, image, occlusion) {
    const maskWidth = materialization.mask_width;
    const maskHeight = materialization.mask_height;
    const [left, top, right, bottom] = materialization.mask_source_box;
    if (![maskWidth, maskHeight, left, top, right, bottom].every(Number.isFinite)) {
      throw new Error("segment mask geometry is incomplete");
    }
    const alpha = decodeMaskRuns(materialization.mask_runs, maskWidth * maskHeight);
    const width = Math.max(1, Math.round(right - left));
    const height = Math.max(1, Math.round(bottom - top));
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("segment canvas 2D context is unavailable");
    context.clearRect(0, 0, width, height);
    context.drawImage(image, left, top, right - left, bottom - top, 0, 0, width, height);
    if (Array.isArray(occlusion) && occlusion.length === 4) {
      context.fillStyle = "rgb(22, 31, 50)";
      context.fillRect(
        ((occlusion[0] - left) / (right - left)) * width,
        ((occlusion[1] - top) / (bottom - top)) * height,
        ((occlusion[2] - occlusion[0]) / (right - left)) * width,
        ((occlusion[3] - occlusion[1]) / (bottom - top)) * height,
      );
    }
    const mask = document.createElement("canvas");
    mask.width = maskWidth;
    mask.height = maskHeight;
    const maskContext = mask.getContext("2d");
    if (!maskContext) throw new Error("segment mask canvas 2D context is unavailable");
    const pixels = maskContext.createImageData(width, height);
    for (let index = 0; index < alpha.length; index += 1) {
      pixels.data[(index * 4) + 3] = alpha[index] ? 255 : 0;
    }
    maskContext.putImageData(pixels, 0, 0);
    context.globalCompositeOperation = "destination-in";
    context.imageSmoothingEnabled = true;
    context.drawImage(mask, 0, 0, width, height);
    context.globalCompositeOperation = "source-over";
    canvas.dataset.maskPainted = "true";
    return mask;
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/// A browser-local boolean preference. Storage can be denied outright, in
/// which case the preference is simply this session's default rather than an
/// error a person has to read.
function readStoredFlag(key, fallback) {
  try {
    const value = localStorage.getItem(key);
    return value === null ? fallback : value === "true";
  } catch {
    return fallback;
  }
}

function writeStoredFlag(key, value) {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    // Denied storage is a preference that lasts this session. Not an error.
  }
}

function readStoredStringArray(key) {
  try {
    const value = JSON.parse(localStorage.getItem(key) || "[]");
    return Array.isArray(value) ? value.filter((item) => typeof item === "string") : [];
  } catch {
    return [];
  }
}

function readStoredAltitudeLevel(key) {
  try {
    const value = localStorage.getItem(key);
    if (value === null) return null;
    if (value === "all") return "all";
    const numeric = Number(value);
    return Number.isFinite(numeric) && numeric >= 0 ? Math.floor(numeric) : null;
  } catch {
    return null;
  }
}

function svg(className) {
  const node = document.createElementNS(SVG_NS, "svg");
  node.setAttribute("class", className);
  return node;
}

// Constraints are a fifth document kind, but they are not a second layout
// engine. This renderer consumes their pinned snapshot fields and member
// geometry, then emits fixed SVG geometry. It never moves, relaxes, smooths,
// or otherwise interprets the scene on the document's behalf.
export function renderConstraints(snapshot, context) {
  const {
    backLayer,
    foreLayer,
    diagLayer,
    /// Where the broken-constraint strip docks. `.diag-root` still owns the
    /// full-pane unsat register, which is a modal and not a docked panel.
    diagStripHost = null,
    scene,
    boundsFor,
    titleFor,
    marksByTarget,
    selected = null,
    selection = null,
    backConstraintsForward = false,
    onSelect = null,
    registerConstraint = null,
  } = context;
  const constraints = snapshot.constraints;
  const constraintState = snapshot.constraint_state;

  backLayer.replaceChildren();
  foreLayer.replaceChildren();
  diagLayer.replaceChildren();

  const unsat = constraintState.status === "unsat";
  scene.inert = unsat;
  if (unsat) {
    diagLayer.append(unsatRegister(constraintState.core, constraints, titleFor));
    return;
  }

  const anchors = new Map();
  const broken = [];
  for (const constraint of constraints) {
    if (constraint.status === "broken") {
      broken.push(constraint);
      continue;
    }
    const memberBounds = constraint.members.map((id) => {
      const bounds = boundsFor(id);
      if (!bounds) throw new Error(`constraint ${constraint.id} has no geometry for ${id}`);
      return bounds;
    });
    const group = document.createElementNS(SVG_NS, "g");
    group.dataset.id = constraint.id;
    const visualSalience = backConstraintsForward && constraint.salience === "back"
      ? "fore"
      : constraint.salience;
    group.setAttribute(
      "class",
      `constraint-glyph constraint-${constraint.op} constraint-${visualSalience}${
        (selection || [selected]).includes(constraint.id) ? " selected" : ""
      }`,
    );
    const layer = visualSalience === "fore" ? foreLayer : backLayer;
    layer.append(group);
    const anchor = constraintElements(group, constraint, memberBounds);
    for (const element of [group, ...group.querySelectorAll("*")]) {
      element.dataset.constraintId = constraint.id;
    }
    group.addEventListener("click", (event) => {
      event.stopPropagation();
      // The event travels so the caller can read shift, which is what makes a
      // constraint glyph joinable to a set the same way a card is.
      onSelect?.(constraint.id, event);
    });
    registerConstraint?.(constraint, group);
    anchors.set(constraint.id, anchor);
  }

  for (const constraint of constraints) {
    const marks = marksByTarget.get(constraint.id) || [];
    const anchor = anchors.get(constraint.id);
    if (anchor && marks.length) {
      foreLayer.append(constraintMarks(constraint.id, anchor, marks, onSelect));
    }
  }

  if (diagStripHost) {
    diagStripHost.querySelector(".diag-strip")?.remove();
    if (broken.length) diagStripHost.append(brokenStrip(broken, titleFor));
  } else if (broken.length) {
    diagLayer.append(brokenStrip(broken, titleFor));
  }
}

function constraintElements(group, constraint, memberBounds) {
  switch (constraint.op) {
    case "group":
      return groupConstraint(group, memberBounds);
    case "sequence":
      return sequenceConstraint(group, memberBounds);
    case "attaches":
      return attachesConstraint(group, memberBounds);
    case "voids":
      return voidsConstraint(group, constraint, memberBounds[0]);
    case "labels":
      return labelsConstraint(group, constraint, memberBounds[0]);
    default:
      throw new Error(`unknown constraint op ${constraint.op}`);
  }
}

function groupConstraint(group, bounds) {
  const left = Math.min(...bounds.map((item) => item.x)) - 8;
  const top = Math.min(...bounds.map((item) => item.y)) - 8;
  const right = Math.max(...bounds.map((item) => item.x + item.w)) + 8;
  const bottom = Math.max(...bounds.map((item) => item.y + item.h)) + 8;
  const hull = document.createElementNS(SVG_NS, "rect");
  hull.setAttribute("class", "constraint-group-hull");
  hull.setAttribute("x", left);
  hull.setAttribute("y", top);
  hull.setAttribute("width", right - left);
  hull.setAttribute("height", bottom - top);
  group.append(hull);
  return { x: left, y: top };
}

function sequenceConstraint(group, bounds) {
  const orderOffset = 16;
  const segments = bounds.slice(1).map((toBounds, index) => {
    const fromBounds = bounds[index];
    return {
      from: edgeToward(fromBounds, centerOf(toBounds)),
      to: edgeToward(toBounds, centerOf(fromBounds)),
    };
  });
  const segmentMidpoints = segments.map(({ from, to }, index) => {
    const rail = document.createElementNS(SVG_NS, "line");
    rail.setAttribute("class", "constraint-sequence-rail");
    rail.setAttribute("x1", from.x);
    rail.setAttribute("y1", from.y);
    rail.setAttribute("x2", to.x);
    rail.setAttribute("y2", to.y);
    group.append(rail);

    const dx = to.x - from.x;
    const dy = to.y - from.y;
    const length = Math.hypot(dx, dy) || 1;
    const midpoint = {
      x: (from.x + to.x) / 2,
      y: (from.y + to.y) / 2,
    };
    const x = midpoint.x + (dy / length) * orderOffset;
    const y = midpoint.y - (dx / length) * orderOffset;
    const order = document.createElementNS(SVG_NS, "g");
    order.setAttribute("class", "constraint-sequence-order");
    const chip = document.createElementNS(SVG_NS, "rect");
    chip.setAttribute("x", x - 12);
    chip.setAttribute("y", y - 10);
    chip.setAttribute("width", 24);
    chip.setAttribute("height", 20);
    chip.setAttribute("rx", 6);
    const numeral = document.createElementNS(SVG_NS, "text");
    numeral.setAttribute("x", x);
    numeral.setAttribute("y", y + 4);
    numeral.textContent = String(index + 1);
    order.append(chip, numeral);
    group.append(order);
    return midpoint;
  });
  const anchor = polylineMidpoint(segmentMidpoints);
  return {
    x: anchor.x,
    y: Math.min(...bounds.map((item) => item.y)) - 8,
  };
}

function attachesConstraint(group, bounds) {
  const [first, second] = bounds;
  const a = edgeToward(first, centerOf(second));
  const b = edgeToward(second, centerOf(first));
  const connector = document.createElementNS(SVG_NS, "line");
  connector.setAttribute("class", "constraint-attaches-line");
  connector.setAttribute("x1", a.x);
  connector.setAttribute("y1", a.y);
  connector.setAttribute("x2", b.x);
  connector.setAttribute("y2", b.y);
  group.append(connector, tickAt(a, b, 0.46), tickAt(a, b, 0.54));
  return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
}

function voidsConstraint(group, constraint, bounds) {
  const patternId = `constraint-hatch-${idCode(constraint.id)}`;
  const defs = document.createElementNS(SVG_NS, "defs");
  const pattern = document.createElementNS(SVG_NS, "pattern");
  pattern.setAttribute("id", patternId);
  pattern.setAttribute("patternUnits", "userSpaceOnUse");
  pattern.setAttribute("width", 8);
  pattern.setAttribute("height", 8);
  const slash = document.createElementNS(SVG_NS, "path");
  slash.setAttribute("class", "constraint-voids-slash");
  slash.setAttribute("d", "M-2,2 L2,-2 M0,8 L8,0 M6,10 L10,6");
  pattern.append(slash);
  defs.append(pattern);
  const overlay = document.createElementNS(SVG_NS, "rect");
  overlay.setAttribute("class", "constraint-voids-overlay");
  overlay.setAttribute("x", bounds.x);
  overlay.setAttribute("y", bounds.y);
  overlay.setAttribute("width", bounds.w);
  overlay.setAttribute("height", bounds.h);
  overlay.setAttribute("fill", `url(#${patternId})`);
  group.append(defs, overlay);
  return { x: bounds.x + bounds.w, y: bounds.y };
}

function labelsConstraint(group, constraint, bounds) {
  const height = 24;
  const centerX = bounds.x + bounds.w / 2;
  const y = bounds.y + bounds.h + 8;
  const chip = document.createElementNS(SVG_NS, "g");
  chip.setAttribute("class", "constraint-label-chip");
  const background = document.createElementNS(SVG_NS, "rect");
  const text = document.createElementNS(SVG_NS, "text");
  text.setAttribute("x", centerX);
  text.setAttribute("y", y + 16);
  text.setAttribute("text-anchor", "middle");
  text.textContent = constraint.text;
  chip.append(background, text);
  group.append(chip);
  const width = text.getComputedTextLength() + 18;
  background.setAttribute("x", centerX - width / 2);
  background.setAttribute("y", y);
  background.setAttribute("width", width);
  background.setAttribute("height", height);
  background.setAttribute("rx", 8);
  return { x: centerX, y: y + height };
}

function constraintMarks(id, anchor, marks, onSelect) {
  const root = document.createElementNS(SVG_NS, "foreignObject");
  root.dataset.id = id;
  root.dataset.markTarget = id;
  root.setAttribute("class", "constraint-mark-anchor");
  const height = marks.length * 38;
  root.setAttribute("x", anchor.x);
  root.setAttribute("y", anchor.y - height - 8);
  root.setAttribute("width", 240);
  root.setAttribute("height", height);
  const stack = document.createElementNS(XHTML_NS, "div");
  stack.setAttribute("class", "constraint-mark-stack");
  for (const mark of marks) {
    const chip = element("span", `mark-chip${mark.answer ? " answered" : ""}`);
    chip.textContent = `${mark.glyph} ${mark.text}`;
    chip.title = mark.answer ? `answered: ${mark.answer}` : "unanswered";
    stack.append(chip);
  }
  root.append(stack);
  root.addEventListener("click", (event) => {
    event.stopPropagation();
    onSelect?.(id);
  });
  return root;
}

function brokenStrip(constraints, titleFor) {
  const strip = element("div", "diag-strip");
  strip.setAttribute("role", "status");
  // This register owns its scrollbar. Letting these events bubble would turn
  // a wheel over diagnostics into a camera zoom, or a scrollbar drag into a
  // canvas pan.
  for (const event of ["pointerdown", "pointermove", "pointerup", "click", "dblclick", "wheel"]) {
    strip.addEventListener(event, (value) => value.stopPropagation());
  }
  strip.append(element("strong", "diag-strip-title", "BROKEN CONSTRAINT"));
  for (const constraint of constraints) {
    strip.append(element("span", "diag-chip", constraintStatement(constraint, titleFor)));
  }
  return strip;
}

function unsatRegister(core, constraints, titleFor) {
  const register = element("div", "diag-unsat");
  register.setAttribute("role", "alert");
  register.setAttribute("aria-label", "Unsatisfiable constraint system");
  for (const event of ["pointerdown", "pointermove", "pointerup", "click", "dblclick", "wheel"]) {
    register.addEventListener(event, (value) => value.stopPropagation());
  }
  const panel = element("section", "diag-panel");
  panel.append(
    element("div", "diag-kicker", "DIAGNOSTIC REGISTER"),
    element("h2", "diag-title", "UNSAT"),
    element("p", "diag-summary", "The scene is frozen. Conflicting core:"),
  );
  const list = element("ol", "diag-core");
  const byId = new Map(constraints.map((constraint) => [constraint.id, constraint]));
  for (const id of core) {
    const constraint = byId.get(id);
    list.append(element("li", "diag-row", constraint ? constraintStatement(constraint, titleFor) : id));
  }
  panel.append(list);
  register.append(panel);
  return register;
}

function schemaRegister(hostVersion, wasmVersion) {
  const waiting = hostVersion === "waiting";
  const register = element("div", "diag-unsat diag-schema");
  register.dataset.register = "schema";
  register.setAttribute("role", "alert");
  register.setAttribute("aria-label", waiting
    ? "Checking atlas document schema"
    : "Atlas document schema mismatch");
  for (const event of ["pointerdown", "pointermove", "pointerup", "click", "dblclick", "wheel"]) {
    register.addEventListener(event, (value) => value.stopPropagation());
  }
  const panel = element("section", "diag-panel");
  panel.append(
    element("div", "diag-kicker", "DIAGNOSTIC REGISTER"),
    element("h2", "diag-title", waiting ? "SCHEMA CHECK" : "SCHEMA MISMATCH"),
    element(
      "p",
      "diag-summary",
      waiting
        ? "The replica is stopped until the host schema arrives."
        : "The replica is stopped. Rebuild web/pkg before using the atlas.",
    ),
  );
  const list = element("ol", "diag-core");
  list.append(
    element("li", "diag-row", `Host document schema: ${hostVersion}`),
    element("li", "diag-row", `WASM replica schema: ${wasmVersion}`),
  );
  panel.append(list);
  register.append(panel);
  return register;
}

function constraintStatement(constraint, titleFor) {
  const members = constraint.members.map(titleFor);
  switch (constraint.op) {
    case "group":
      return `${constraint.id} group: ${members.join(", ")}`;
    case "sequence":
      return `${constraint.id} sequence[${constraint.axis}]: ${members.join(" -> ")}`;
    case "attaches":
      return `${constraint.id} attaches: ${members.join(" <-> ")}`;
    case "voids":
      return `${constraint.id} voids: ${members[0]}`;
    case "labels":
      return `${constraint.id} labels: ${members[0]} "${constraint.text}"`;
    default:
      return `${constraint.id} ${constraint.op}: ${members.join(", ")}`;
  }
}

function centerOf(bounds) {
  return { x: bounds.x + bounds.w / 2, y: bounds.y + bounds.h / 2 };
}

function edgeToward(bounds, target) {
  const center = centerOf(bounds);
  const dx = target.x - center.x;
  const dy = target.y - center.y;
  if (dx === 0 && dy === 0) return center;
  const scaleX = dx === 0 ? Infinity : (bounds.w / 2) / Math.abs(dx);
  const scaleY = dy === 0 ? Infinity : (bounds.h / 2) / Math.abs(dy);
  const scale = Math.min(scaleX, scaleY);
  return { x: center.x + dx * scale, y: center.y + dy * scale };
}

function tickAt(a, b, amount) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const length = Math.hypot(dx, dy) || 1;
  const x = a.x + dx * amount;
  const y = a.y + dy * amount;
  const px = (-dy / length) * 6;
  const py = (dx / length) * 6;
  const tick = document.createElementNS(SVG_NS, "line");
  tick.setAttribute("class", "constraint-attaches-tick");
  tick.setAttribute("x1", x - px);
  tick.setAttribute("y1", y - py);
  tick.setAttribute("x2", x + px);
  tick.setAttribute("y2", y + py);
  return tick;
}

function polylineMidpoint(points) {
  if (points.length === 1) return points[0];
  const segments = points.slice(1).map((point, index) => ({
    from: points[index],
    to: point,
    length: Math.hypot(point.x - points[index].x, point.y - points[index].y),
  }));
  const total = segments.reduce((sum, segment) => sum + segment.length, 0);
  let remaining = total / 2;
  for (const segment of segments) {
    if (remaining <= segment.length) {
      const amount = segment.length === 0 ? 0 : remaining / segment.length;
      return {
        x: segment.from.x + (segment.to.x - segment.from.x) * amount,
        y: segment.from.y + (segment.to.y - segment.from.y) * amount,
      };
    }
    remaining -= segment.length;
  }
  return points[points.length - 1];
}

function idCode(id) {
  return Array.from(id).map((value) => value.codePointAt(0).toString(16)).join("-");
}
