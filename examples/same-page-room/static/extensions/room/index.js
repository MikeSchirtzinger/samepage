// The room's renderer.
//
// This module is the only thing in the system that turns a view node into a DOM
// node, and it is the reason the agent can author arbitrary UI safely: every
// string it receives lands in `textContent` or in a property this file chose,
// never in `innerHTML` and never in an event handler. There is no path from a
// pane's contents to script execution, so "generate me a new interface" costs
// nothing more than "write me a paragraph".
//
// A renderer that quietly drops a node it does not recognise would make the
// agent's read-back a lie, so an unknown kind renders as a visible complaint.

const EVENT = "surface.room";

const TONES = new Set(["neutral", "muted", "strong", "accent", "good", "warn", "bad"]);

/// Pane geometry. These have to agree with `layout.rs`, which clamps anything
/// arriving from here — the browser is where a size is *chosen*, but the server
/// is where it is decided, so a disagreement shows up as a pane that springs
/// back rather than as a corrupt document.
const MIN_W = 200;
const MIN_H = 120;
const GUTTER = 20;

/// How near an edge has to be before it catches, and the base grid everything
/// falls back to when there is nothing better to line up with.
///
/// The old model had exactly two widths and four heights, so "snapping" meant
/// there was nowhere else to go. Here the pane can be any size at all and the
/// snaps are *offers*: line up with a neighbour's edge, its centre, or the gap
/// its neighbour already uses — and drag a little further to refuse all of them.
const SNAP = 9;
const GRID = 8;
/// Empty space left past the furthest pane so there is always canvas to drag
/// into without having to make room first.
const SLACK = 600;

/// A short stable key for a string, so a frame's URL changes exactly when its
/// content does. Not a cryptographic hash and does not need to be.
function contentKey(text) {
  let hash = 5381;
  const source = String(text || "");
  for (let i = 0; i < source.length; i++) hash = ((hash * 33) ^ source.charCodeAt(i)) >>> 0;
  return hash.toString(36);
}

/// Every position along one axis that it would look deliberate to land on.
///
/// Each candidate is `{ at, guide }`: `at` is where the dragged edge would go,
/// `guide` is where to draw the line that explains why. Returning both is what
/// keeps the feedback honest — a guide is only ever drawn for a snap that
/// actually applied, so the person is never shown a line they did not catch.
function candidates(others, axis, size) {
  const out = [];
  const near = axis === "x"
    ? (spot) => ({ low: spot.x, high: spot.x + spot.w })
    : (spot) => ({ low: spot.y, high: spot.y + spot.h });
  for (const spot of others) {
    const { low, high } = near(spot);
    const middle = (low + high) / 2;
    // Leading edge against their leading edge, trailing edge, and centre.
    out.push({ at: low, guide: low });
    out.push({ at: high, guide: high });
    out.push({ at: high + GUTTER, guide: high });
    out.push({ at: middle - size / 2, guide: middle });
    // Trailing edge against theirs, so two panes can end flush as easily as
    // they can start flush.
    out.push({ at: low - size, guide: low });
    out.push({ at: low - size - GUTTER, guide: low });
    out.push({ at: high - size, guide: high });
  }
  return out;
}

/// Snap one edge, preferring a real neighbour over the fallback grid.
///
/// Returns `{ value, guide }` — `guide` is null when the grid caught it, since
/// "you are on the grid" is not worth a line on screen.
function snapAxis(wanted, others, axis, size) {
  let best = null;
  for (const candidate of candidates(others, axis, size)) {
    const gap = Math.abs(candidate.at - wanted);
    if (gap > SNAP) continue;
    if (!best || gap < best.gap) best = { gap, value: candidate.at, guide: candidate.guide };
  }
  if (best) return { value: best.value, guide: best.guide };
  return { value: Math.round(wanted / GRID) * GRID, guide: null };
}

/// Snap a size, which lines up with a neighbour's *dimensions* rather than its
/// position: matching two panes' widths is a thing people do deliberately and
/// cannot do at all by eye.
function snapSize(wanted, others, axis, edge) {
  const dimension = axis === "x" ? "w" : "h";
  let best = null;
  const offer = (value, guide) => {
    const gap = Math.abs(value - wanted);
    if (gap > SNAP || (best && gap >= best.gap)) return;
    best = { gap, value, guide };
  };
  for (const spot of others) {
    offer(spot[dimension], null);
    // Or end flush with a neighbour's far edge.
    const far = axis === "x" ? spot.x + spot.w : spot.y + spot.h;
    offer(far - edge, far);
  }
  if (best) return { value: best.value, guide: best.guide };
  return { value: Math.round(wanted / GRID) * GRID, guide: null };
}

const MARKS = [
  { id: "question", glyph: "?", title: "I don't follow this" },
  { id: "important", glyph: "!", title: "This matters" },
  { id: "agree", glyph: "✓", title: "Agreed" },
  { id: "disagree", glyph: "✗", title: "I think this is wrong" },
];

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function toneClass(tone) {
  return TONES.has(tone) ? `tone-${tone}` : "tone-neutral";
}

const SVG_NS = "http://www.w3.org/2000/svg";

/** `el` for SVG. A namespaced element is not optional here: `createElement`
 *  builds an HTML element that happens to be named `rect`, and it renders as
 *  nothing at all. */
function svgEl(tag, className, attributes) {
  const node = document.createElementNS(SVG_NS, tag);
  if (className) node.setAttribute("class", className);
  for (const [name, value] of Object.entries(attributes || {})) {
    node.setAttribute(name, String(value));
  }
  return node;
}

/** Arrowhead marker ids have to be unique per document, not per diagram. */
let markerSeq = 0;

/** A pane-scoped context so `field` values can reach the `button` beside them. */
function paneScope() {
  return { fields: new Map() };
}

function collect(scope) {
  const lines = [];
  for (const [key, input] of scope.fields) {
    const value = input.value.trim();
    if (value) lines.push(`${key}: ${value}`);
  }
  return lines;
}

// ── node rendering ────────────────────────────────────────────────────────

/// Which face each deck is showing, per pane, surviving re-renders. This is
/// the reader's own position — like scroll, it never enters the document.
const deckPositions = new Map();

/// The pointer bridge that reports clicks inside an `html` pane lives
/// server-side (`POINTER_BRIDGE_DOC` in room.rs): srcdoc would inherit this
/// page's CSP and never run a script, so each html node is served as its own
/// document from `/room/pane-html` and framed with `sandbox="allow-scripts"`.

function renderNode(node, ctx, scope) {
  if (!node || typeof node !== "object") return el("div", "node-error", "empty node");
  switch (node.kind) {
    case "stack":
    case "row": {
      const box = el("div", node.kind === "row" ? "n-row" : "n-stack");
      if (node.kind === "row" && node.wrap === false) box.classList.add("no-wrap");
      if (Number.isFinite(node.gap)) box.style.setProperty("gap", `${Math.min(24, Math.max(0, node.gap))}px`);
      for (const child of node.children || []) box.append(renderNode(child, ctx, scope));
      return box;
    }
    case "html": {
      const wrap = el("div", "n-html");
      const frame = document.createElement("iframe");
      frame.className = "html-frame";
      // No allow-same-origin: even though the URL is same-origin, the sandbox
      // strips it to an opaque origin, so nothing inside can read this page,
      // its storage, or its credentials. The bridge is the only way out.
      frame.setAttribute("sandbox", "allow-scripts");
      frame.setAttribute("referrerpolicy", "no-referrer");
      const height = Number.isFinite(node.height)
        ? Math.min(2000, Math.max(120, node.height))
        : 320;
      frame.style.height = `${height}px`;
      frame.dataset.paneId = ctx.paneId || "";
      const index = ctx.htmlIndex?.get(node) ?? 0;
      // Keyed to what this frame *contains*, never to the room's revision. A
      // click inside a sandbox becomes a pane note, which bumps the revision —
      // and a revision in this URL meant every point reloaded the very thing
      // that was pointed at, resetting it. The document only reloads when the
      // agent actually rewrites the html.
      frame.src = `/room/pane-html?pane=${encodeURIComponent(ctx.paneId || "")}&index=${index}&v=${contentKey(node.html)}`;
      wrap.append(frame);
      return wrap;
    }
    case "deck": {
      const children = node.children || [];
      const box = el("div", "n-deck");
      scope.deckSeq = (scope.deckSeq || 0) + 1;
      const key = `${ctx.paneId || ""}:deck:${scope.deckSeq}`;
      let index = Math.min(deckPositions.get(key) ?? 0, Math.max(0, children.length - 1));
      const nav = el("div", "deck-nav");
      const prev = el("button", "icon-button deck-prev", "‹");
      prev.type = "button";
      const next = el("button", "icon-button deck-next", "›");
      next.type = "button";
      const counter = el("span", "deck-counter");
      nav.append(prev, counter, next);
      const body = el("div", "deck-body");
      const show = () => {
        deckPositions.set(key, index);
        const title = node.titles && node.titles[index];
        counter.textContent = title
          ? `${index + 1}/${children.length} · ${title}`
          : `${index + 1} of ${children.length}`;
        prev.disabled = index === 0;
        next.disabled = index >= children.length - 1;
        body.replaceChildren();
        if (children[index]) body.append(renderNode(children[index], ctx, scope));
      };
      prev.addEventListener("click", () => { if (index > 0) { index -= 1; show(); } });
      next.addEventListener("click", () => { if (index < children.length - 1) { index += 1; show(); } });
      show();
      box.append(nav, body);
      return box;
    }
    case "heading": {
      const level = [1, 2, 3].includes(node.level) ? node.level : 2;
      return el(`h${level + 1}`, `n-heading level-${level}`, node.text);
    }
    case "text": {
      const paragraph = el("p", `n-text ${toneClass(node.tone)}`);
      // Blank lines are the only structure plain text carries; honour them
      // without ever parsing the text as markup.
      const chunks = String(node.text ?? "").split(/\n{2,}/);
      chunks.forEach((chunk, index) => {
        if (index) paragraph.append(el("span", "n-break"));
        paragraph.append(document.createTextNode(chunk));
      });
      return paragraph;
    }
    case "code": {
      const box = el("div", "n-code");
      if (node.lang) box.append(el("span", "code-lang", node.lang));
      box.append(el("pre", "", node.text));
      return box;
    }
    case "list": {
      const list = el(node.ordered ? "ol" : "ul", "n-list");
      for (const item of node.items || []) list.append(el("li", "", item));
      return list;
    }
    case "kv": {
      const box = el("dl", "n-kv");
      for (const item of node.items || []) {
        box.append(el("dt", "", item.label), el("dd", "", item.value));
      }
      return box;
    }
    case "table": {
      const wrap = el("div", "n-table-wrap");
      const table = el("table", "n-table");
      const head = el("thead");
      const headRow = el("tr");
      for (const column of node.columns || []) headRow.append(el("th", "", column));
      head.append(headRow);
      const body = el("tbody");
      for (const row of node.rows || []) {
        const tr = el("tr");
        for (const cell of row) tr.append(el("td", "", cell));
        body.append(tr);
      }
      table.append(head, body);
      wrap.append(table);
      return wrap;
    }
    case "diagram": {
      // The host already laid this out — see `view.rs::place`. Everything here
      // is transcription: one <rect> per node, one <line> per edge, at the
      // coordinates that arrived. Deliberately no layout logic, so there is
      // never a second (and disagreeing) implementation of the geometry.
      if (node.error) return el("div", "node-error", node.error);
      const placed = node.resolved;
      if (!placed) return el("div", "node-error", "this diagram was not laid out");

      const figure = el("figure", "n-diagram");
      const [vx, vy, vw, vh] = placed.view_box || [0, 0, 1, 1];
      const svg = svgEl("svg", "diagram-svg", {
        viewBox: `${vx} ${vy} ${vw} ${vh}`,
        preserveAspectRatio: "xMidYMid meet",
        role: "img",
      });
      // Height follows the aspect ratio so a wide pipeline stays short and a
      // deep tree gets the room it needs.
      svg.style.aspectRatio = `${Math.max(vw, 1)} / ${Math.max(vh, 1)}`;

      const markerId = `room-arrow-${++markerSeq}`;
      const defs = svgEl("defs");
      const marker = svgEl("marker", null, {
        id: markerId,
        viewBox: "0 0 10 10",
        refX: 9,
        refY: 5,
        markerWidth: 6,
        markerHeight: 6,
        orient: "auto-start-reverse",
      });
      marker.append(svgEl("path", "diagram-head", { d: "M 0 0 L 10 5 L 0 10 z" }));
      defs.append(marker);
      svg.append(defs);

      const textSize = Number(placed.text_size) || 12;

      // Edges first so the boxes sit on top of the wire ends.
      for (const edge of placed.edges || []) {
        const line = svgEl("line", "diagram-edge", {
          x1: edge.x1, y1: edge.y1, x2: edge.x2, y2: edge.y2,
        });
        if (edge.arrow !== false) line.setAttribute("marker-end", `url(#${markerId})`);
        svg.append(line);
      }

      for (const box of placed.nodes || []) {
        const group = svgEl("g", `diagram-node ${toneClass(box.tone)}`);
        group.append(svgEl("rect", "diagram-box", {
          x: box.x, y: box.y, width: box.w, height: box.h,
          rx: Math.min(6, box.h * 0.2),
        }));
        const label = svgEl("text", "diagram-label", {
          x: box.x + box.w / 2,
          y: box.y + box.h / 2,
          "font-size": textSize,
          "text-anchor": "middle",
          "dominant-baseline": "central",
        });
        label.textContent = box.label;
        group.append(label);

        // Clicking a box is how "this" gets bound to something specific: it
        // asks about that node by name, so the reply is about the part of the
        // picture the person actually pointed at.
        group.setAttribute("tabindex", "0");
        group.setAttribute("role", "button");
        const ask = () => document.dispatchEvent(new CustomEvent("room:ask", {
          detail: { text: `About "${box.label}" in the diagram — tell me more about that one.` },
        }));
        group.addEventListener("click", ask);
        group.addEventListener("keydown", (event) => {
          if (event.key === "Enter" || event.key === " ") { event.preventDefault(); ask(); }
        });
        const title = svgEl("title");
        title.textContent = `${box.label} — ask about this`;
        group.append(title);
        svg.append(group);
      }

      // Edge labels last. A label sits at the midpoint of the gap between two
      // boxes, and it is routinely wider than that gap — drawn with the wires
      // it ends up buried under whichever box it overlaps. On top, plus the
      // halo `.diagram-edge-label` paints, it stays readable over both.
      for (const edge of placed.edges || []) {
        if (!edge.label) continue;
        const label = svgEl("text", "diagram-edge-label", {
          x: (edge.x1 + edge.x2) / 2,
          y: (edge.y1 + edge.y2) / 2 - textSize * 0.35,
          "font-size": textSize * 0.8,
          "text-anchor": "middle",
        });
        label.textContent = edge.label;
        svg.append(label);
      }

      figure.append(svg);
      if (node.caption) figure.append(el("figcaption", "diagram-caption", node.caption));
      return figure;
    }
    case "badge":
      return el("span", `n-badge ${toneClass(node.tone)}`, node.text);
    case "divider":
      return el("hr", "n-divider");
    case "button": {
      const button = el("button", `n-button ${toneClass(node.tone)}`, node.label);
      button.type = "button";
      button.addEventListener("click", () => {
        const extra = collect(scope);
        const ask = extra.length ? `${node.ask}\n\n${extra.join("\n")}` : node.ask;
        document.dispatchEvent(new CustomEvent("room:ask", { detail: { text: ask } }));
      });
      return button;
    }
    case "field": {
      const wrap = el("label", "n-field");
      if (node.label) wrap.append(el("span", "field-label", node.label));
      const input = node.multiline ? el("textarea") : el("input");
      if (!node.multiline) input.type = "text";
      if (node.multiline) input.rows = 3;
      if (node.placeholder) input.placeholder = node.placeholder;
      input.maxLength = 4000;
      scope.fields.set(node.key, input);
      wrap.append(input);
      return wrap;
    }
    case "link": {
      const link = el("a", "n-link", node.label);
      link.href = node.url;
      link.target = "_blank";
      link.rel = "noreferrer noopener";
      link.title = node.url;
      return link;
    }
    case "image": {
      const image = el("img", "n-image");
      image.src = node.src;
      image.alt = node.alt || "";
      image.loading = "lazy";
      return image;
    }
    case "source":
      return renderSource(node);
    case "options":
      return renderOptions(node, ctx);
    case "embed":
      return renderEmbed(node, ctx);
    default:
      return el("div", "node-error", `this room cannot render a “${node.kind}” node`);
  }
}

function renderSource(node) {
  const box = el("div", "n-source");
  const head = el("div", "source-head");
  const range = node.resolved
    ? `${node.resolved.path}:${node.resolved.first_line}-${node.resolved.last_line}`
    : node.path;
  head.append(el("span", "source-path", range));
  if (node.resolved) {
    head.append(el("span", "source-meta", `${node.resolved.total_lines} lines in file`));
    if (node.resolved.truncated) head.append(el("span", "source-meta warn", "clipped"));
  }
  box.append(head);
  if (node.error) {
    box.append(el("p", "node-error", node.error));
    return box;
  }
  const pre = el("pre", "source-body");
  const first = node.resolved?.first_line ?? 1;
  const lines = String(node.resolved?.text ?? "").split("\n");
  lines.forEach((line, index) => {
    const row = el("span", "source-line");
    row.append(el("span", "source-gutter", first + index), el("span", "source-text", line));
    pre.append(row);
  });
  box.append(pre);
  return box;
}

function renderOptions(node, ctx) {
  const box = el("div", "n-options");
  if (node.error) {
    box.append(el("p", "node-error", node.error));
    return box;
  }
  const options = node.resolved?.options || [];
  if (!options.length) {
    box.append(el("p", "n-text tone-muted", "No runnable packages matched."));
    return box;
  }
  for (const option of options) {
    const row = el("div", "option");
    const head = el("div", "option-head");
    const dot = el("span", "option-dot");
    if (option.listening) dot.classList.add("live");
    dot.title = option.listening
      ? `something answered on port ${option.port} just now`
      : option.port
        ? `nothing is listening on port ${option.port}`
        : "no default port";
    head.append(dot, el("strong", "", option.package));
    if (option.port) head.append(el("span", "option-port", `:${option.port}`));
    row.append(head);
    if (option.description) row.append(el("p", "option-description", option.description));

    const actions = el("div", "option-actions");
    const command = el("code", "option-command", option.command);
    actions.append(command);
    const copy = el("button", "chip", "Copy");
    copy.type = "button";
    copy.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(option.command);
        copy.textContent = "Copied";
        setTimeout(() => (copy.textContent = "Copy"), 1200);
      } catch {
        copy.textContent = "Copy failed";
      }
    });
    actions.append(copy);

    if (option.listening && option.url) {
      const open = el("button", "chip accent", "Open here");
      open.type = "button";
      open.addEventListener("click", () => ctx.openInRoom(option));
      actions.append(open);
    } else if (option.port) {
      const ask = el("button", "chip", "Start it");
      ask.type = "button";
      ask.addEventListener("click", () => {
        document.dispatchEvent(new CustomEvent("room:ask", {
          detail: { text: `Start ${option.package} for me (${option.command}), then put it in the room once it is listening on port ${option.port}.` },
        }));
      });
      actions.append(ask);
    }
    row.append(actions);
    if (option.prepare) {
      row.append(el("p", "option-note", `needs a build step first: ${option.prepare}`));
    }
    if (option.contested_by?.length) {
      row.append(el("p", "option-note",
        `wants the same port as ${option.contested_by.join(", ")} — only one can bind it${
          option.port_env ? `; override with ${option.port_env}=` : ""}`));
    }
    if (option.extensions?.length) {
      const tags = el("div", "option-tags");
      for (const extension of option.extensions) tags.append(el("span", "tag", extension));
      row.append(tags);
    }
    box.append(row);
  }
  return box;
}

function renderEmbed(node, ctx) {
  const box = el("div", "n-embed");
  const head = el("div", "embed-head");
  head.append(el("span", "embed-url", node.url));
  box.append(head);

  const height = Math.min(2000, Math.max(120, node.height || 420));
  const frame = () => {
    const iframe = el("iframe", "embed-frame");
    // No allow-same-origin: the framed site cannot reach this page's origin,
    // its storage, or the runtime's control plane.
    iframe.setAttribute("sandbox", "allow-scripts allow-forms allow-popups");
    iframe.setAttribute("referrerpolicy", "no-referrer");
    iframe.loading = "lazy";
    iframe.style.setProperty("height", `${height}px`);
    iframe.src = node.url;
    return iframe;
  };

  // An embed the agent wrote is a proposal to load a site, not permission to.
  // One the person typed is already their decision.
  if (ctx.paneAuthor === "agent") {
    const gate = el("div", "embed-gate");
    gate.style.setProperty("height", `${height}px`);
    gate.append(el("p", "n-text tone-muted", "The agent wants to open this site in the room."));
    const load = el("button", "n-button tone-accent", "Load it");
    load.type = "button";
    load.addEventListener("click", () => gate.replaceWith(frame()));
    gate.append(load);
    box.append(gate);
  } else {
    box.append(frame());
  }
  return box;
}

// ── pane and room rendering ───────────────────────────────────────────────

export async function activate(ctx) {
  const root = el("div", "room");
  const canvas = el("div", "room-canvas");
  root.append(canvas);
  ctx.mount.replaceChildren(root);

  let state = null;
  let focused = null;
  /// Which pane, if any, is filling the page. Local to this browser and never
  /// written back: two people in the room can be looking at different things
  /// without fighting over the document.
  let expanded = null;
  /// Live pane elements by id, with the key they were built from.
  const panes = new Map();

  /// What a pane's DOM is actually built from. Marks, notes, titles, bylines,
  /// pins and sizes are all refreshed in place by `__update`, so none of them
  /// belong here — only the view, whose shape decides the body's structure.
  function paneKey(pane) {
    return JSON.stringify(pane.view);
  }

  /// A pane's rectangle, defended against a document that predates the field
  /// or a hand-edit that mangled it. A pane with no usable spot is still a
  /// pane; it just starts at the origin rather than vanishing.
  function spotOf(pane) {
    const spot = pane.spot || {};
    return {
      x: Number.isFinite(spot.x) ? spot.x : 0,
      y: Number.isFinite(spot.y) ? spot.y : 0,
      w: Math.max(MIN_W, Number.isFinite(spot.w) ? spot.w : 420),
      h: Math.max(MIN_H, Number.isFinite(spot.h) ? spot.h : 280),
    };
  }

  function applySpot(article, spot) {
    article.style.setProperty("--x", `${spot.x}px`);
    article.style.setProperty("--y", `${spot.y}px`);
    article.style.setProperty("--w", `${spot.w}px`);
    article.style.setProperty("--h", `${spot.h}px`);
  }

  /// Everyone else's rectangle — what a drag snaps against.
  function otherSpots(exceptId) {
    return (state?.panes || [])
      .filter((pane) => pane.id !== exceptId)
      .map(spotOf);
  }

  /// Grow the canvas to hold the furthest pane, plus somewhere to drag next.
  function resizeCanvas() {
    let right = 0;
    let bottom = 0;
    for (const pane of state?.panes || []) {
      const spot = spotOf(pane);
      right = Math.max(right, spot.x + spot.w);
      bottom = Math.max(bottom, spot.y + spot.h);
    }
    canvas.style.setProperty("--canvas-w", `${Math.round(right + SLACK)}px`);
    canvas.style.setProperty("--canvas-h", `${Math.round(bottom + SLACK)}px`);
  }

  // Grab the empty canvas to pan it. Scrollbars already work, but a canvas you
  // cannot pull is a canvas you keep losing your place on — and the gesture has
  // to start on bare canvas, so it can never be confused with moving a pane.
  canvas.addEventListener("pointerdown", (event) => {
    if (event.target !== canvas || event.button !== 0) return;
    const scroller = canvas.closest(".room-pane") || canvas.parentElement;
    if (!scroller) return;
    event.preventDefault();
    const origin = {
      x: event.clientX,
      y: event.clientY,
      left: scroller.scrollLeft,
      top: scroller.scrollTop,
    };
    canvas.setPointerCapture(event.pointerId);
    canvas.style.cursor = "grabbing";

    const onMove = (move) => {
      scroller.scrollLeft = origin.left - (move.clientX - origin.x);
      scroller.scrollTop = origin.top - (move.clientY - origin.y);
    };
    const finish = () => {
      canvas.removeEventListener("pointermove", onMove);
      canvas.removeEventListener("pointerup", finish);
      canvas.removeEventListener("pointercancel", finish);
      canvas.style.cursor = "";
    };
    canvas.addEventListener("pointermove", onMove);
    canvas.addEventListener("pointerup", finish);
    canvas.addEventListener("pointercancel", finish);
  });

  // Guides live on the canvas and only exist mid-drag.
  const guides = [];
  function showGuides(lines) {
    while (guides.length < lines.length) {
      const guide = el("div", "snap-guide");
      canvas.append(guide);
      guides.push(guide);
    }
    guides.forEach((guide, index) => {
      const line = lines[index];
      if (!line) {
        guide.style.display = "none";
        return;
      }
      guide.style.display = "";
      guide.className = `snap-guide ${line.axis === "x" ? "v" : "h"}`;
      if (line.axis === "x") {
        guide.style.left = `${line.at}px`;
        guide.style.top = `${line.from}px`;
        guide.style.height = `${line.to - line.from}px`;
        guide.style.width = "";
      } else {
        guide.style.top = `${line.at}px`;
        guide.style.left = `${line.from}px`;
        guide.style.width = `${line.to - line.from}px`;
        guide.style.height = "";
      }
    });
  }
  function clearGuides() {
    for (const guide of guides) guide.style.display = "none";
  }

  /// A guide long enough to reach whatever it is lining up with, so the line
  /// visibly connects the two panes rather than floating near one of them.
  function guideSpan(axis, at, moving, others) {
    let from = axis === "x" ? moving.y : moving.x;
    let to = axis === "x" ? moving.y + moving.h : moving.x + moving.w;
    for (const spot of others) {
      const edges = axis === "x"
        ? [spot.x, spot.x + spot.w, (spot.x + spot.x + spot.w) / 2]
        : [spot.y, spot.y + spot.h, (spot.y + spot.y + spot.h) / 2];
      if (!edges.some((edge) => Math.abs(edge - at) <= 1)) continue;
      from = Math.min(from, axis === "x" ? spot.y : spot.x);
      to = Math.max(to, axis === "x" ? spot.y + spot.h : spot.x + spot.w);
    }
    return { from: from - 12, to: to + 12 };
  }

  function expand(id) {
    expanded = id;
    document.body.classList.toggle("has-expanded", Boolean(id));
    for (const node of canvas.querySelectorAll(".pane")) {
      node.classList.toggle("expanded", node.dataset.paneId === id);
    }
  }

  // Escape is the way out of anything that took over the screen.
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && expanded) {
      event.preventDefault();
      expand(null);
    }
  });

  const openInRoom = async (option) => {
    if (!state) return;
    await queued(() => ["room_put_pane", {
      id: `run-${option.package}`.slice(0, 48),
      title: option.package,
      size: "large",
      place: "end",
      view: {
        kind: "stack",
        children: [
          { kind: "text", tone: "muted", text: `${option.package}, running at ${option.url}.` },
          { kind: "embed", url: option.url, height: 520 },
        ],
      },
    }]);
  };

  async function call(name, args) {
    try {
      return await ctx.action(name, args);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      document.dispatchEvent(new CustomEvent("room:error", { detail: { message } }));
      throw cause;
    }
  }

  // Every mutation carries the revision it was based on, so two controls
  // clicked in the same tick would make the second one stale — nudge a slider
  // twice and the room rejects the second nudge. Serialising here means each
  // command reads the revision that the one before it produced. The conflict
  // check still does its real job: catching the *agent* writing underneath the
  // person, which no amount of local queueing can order.
  let chain = Promise.resolve();
  function queued(build) {
    chain = chain
      .then(() => {
        if (!state) return undefined;
        const [name, args] = build(state);
        return name ? call(name, { expected_revision: state.revision, ...args }) : undefined;
      })
      .catch(() => {});
    return chain;
  }

  // The receiving half of the html-pane pointer bridge. A click inside a
  // sandboxed frame arrives here as a message; the frame's pane id says where
  // it happened, and the label becomes that pane's note — the same human-only
  // channel the note input uses, because the click *is* the person.
  window.addEventListener("message", (event) => {
    const label = event.data && typeof event.data.aguiPoint === "string"
      ? event.data.aguiPoint.trim().slice(0, 560)
      : "";
    if (!label) return;
    const frame = [...document.querySelectorAll("iframe.html-frame")]
      .find((candidate) => candidate.contentWindow === event.source);
    const paneId = frame?.dataset.paneId;
    if (!paneId) return;
    const note = `pointed: ${label}`;
    // An unchanged note is not re-written. Without this, a script that
    // reports on load re-fires on every re-render — each write bumps the
    // revision, the new snapshot re-renders, the frame refetches, and the
    // room spins revisions forever.
    const current = state?.panes?.find((pane) => pane.id === paneId)?.note;
    if (current === note) return;
    queued(() => ["room_annotate_pane", { id: paneId, note }]);
  });

  function renderPane(pane) {
    const article = el("article", "pane");
    article.dataset.paneId = pane.id;
    article.dataset.author = pane.author;
    applySpot(article, spotOf(pane));
    if (pane.mark && pane.mark !== "none") article.classList.add(`marked-${pane.mark}`);
    if (focused === pane.id) article.classList.add("focused");

    const head = el("header", "pane-head");
    // Pointer events rather than HTML5 drag-and-drop. The old reorder drag
    // moved a pane's *place in a list*; this moves the pane, so it has to
    // follow the pointer exactly, and a drag image floating alongside the real
    // thing is precisely the wrong feedback.
    head.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || event.target.closest("button, input")) return;
      if (expanded === pane.id) return;
      event.preventDefault();
      const start = spotOf(livePane(pane.id) || pane);
      const origin = { x: event.clientX, y: event.clientY };
      const others = otherSpots(pane.id);
      let placed = start;
      article.classList.add("moving");
      head.setPointerCapture(event.pointerId);

      const onMove = (move) => {
        const wantedX = start.x + (move.clientX - origin.x);
        const wantedY = start.y + (move.clientY - origin.y);
        // Shift drags free: an escape hatch for the one time in ten that the
        // magnet is holding a pane somewhere you do not want it.
        const x = move.shiftKey
          ? { value: Math.max(0, wantedX), guide: null }
          : snapAxis(Math.max(0, wantedX), others, "x", start.w);
        const y = move.shiftKey
          ? { value: Math.max(0, wantedY), guide: null }
          : snapAxis(Math.max(0, wantedY), others, "y", start.h);
        placed = { x: Math.max(0, x.value), y: Math.max(0, y.value), w: start.w, h: start.h };
        applySpot(article, placed);
        showGuides([
          x.guide === null ? null : { axis: "x", at: x.guide, ...guideSpan("x", x.guide, placed, others) },
          y.guide === null ? null : { axis: "y", at: y.guide, ...guideSpan("y", y.guide, placed, others) },
        ]);
      };

      const finish = () => {
        head.removeEventListener("pointermove", onMove);
        head.removeEventListener("pointerup", finish);
        head.removeEventListener("pointercancel", finish);
        article.classList.remove("moving");
        clearGuides();
        if (placed.x === start.x && placed.y === start.y) return;
        resizeCanvas();
        queued(() => ["room_arrange", { panes: [{ id: pane.id, spot: placed }] }]);
      };

      head.addEventListener("pointermove", onMove);
      head.addEventListener("pointerup", finish);
      head.addEventListener("pointercancel", finish);
    });

    const title = el("h3", "pane-title", pane.title);
    // Print the author the host recorded rather than collapsing everything
    // that is not the person into "agent" — "companion" is a distinct claim
    // (an assistant reaching in from outside the room) and flattening it here
    // would throw away the byline the server went to the trouble of keeping.
    // Show the name whoever wrote this announced, falling back to the bare
    // category. The category stays in the class either way, so "agent" and
    // "companion" keep looking different at a glance no matter what an agent
    // decided to call itself, and the tooltip always states it in words — a
    // chosen label never gets to stand in for the claim the host is making.
    const by = el(
      "span",
      `pane-by by-${pane.author}${pane.by_name ? " named" : ""}`,
      pane.by_name || pane.author,
    );
    by.title = pane.by_name ? `${pane.by_name} — ${pane.author}` : pane.author;
    head.append(title, by);

    const marks = el("div", "pane-marks");
    for (const mark of MARKS) {
      const button = el("button", "mark", mark.glyph);
      button.type = "button";
      button.title = mark.title;
      button.dataset.mark = mark.id;
      if (pane.mark === mark.id) button.classList.add("on");
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        queued((live) => {
          const current = live.panes.find((entry) => entry.id === pane.id);
          if (!current) return [null];
          return ["room_annotate_pane", {
            id: pane.id,
            mark: current.mark === mark.id ? "none" : mark.id,
          }];
        });
      });
      marks.append(button);
    }
    // Text, not a glyph: ✎ and 📌 fall back to colour emoji in this font stack,
    // which reads as decoration next to the monochrome ? ! ✓ ✗.
    const noteButton = el("button", "mark wide-mark secondary", "note");
    noteButton.type = "button";
    noteButton.title = "Leave a note on this pane";
    noteButton.addEventListener("click", (event) => {
      event.stopPropagation();
      article.classList.toggle("noting");
      const field = article.querySelector(".note-input");
      if (field) field.focus();
    });
    marks.append(noteButton);

    const pin = el("button", "mark wide-mark secondary", pane.pinned ? "pinned" : "pin");
    pin.type = "button";
    pin.title = pane.pinned ? "Unpin" : "Pin so it cannot be removed";
    if (pane.pinned) pin.classList.add("on");
    pin.addEventListener("click", (event) => {
      event.stopPropagation();
      queued((live) => {
        const current = live.panes.find((entry) => entry.id === pane.id);
        if (!current) return [null];
        return ["room_arrange", { panes: [{ id: pane.id, pinned: !current.pinned }] }];
      });
    });
    marks.append(pin);

    // Was a width cycle, which could only ever offer one other size; widths and
    // heights are the corner grip's job now. This is the thing the button
    // always looked like it did.
    const wider = el("button", "mark secondary", "⤢");
    wider.type = "button";
    wider.title = expanded === pane.id ? "Back to the room (esc)" : "Fill the page";
    wider.addEventListener("click", (event) => {
      event.stopPropagation();
      expand(expanded === pane.id ? null : pane.id);
    });
    marks.append(wider);

    const close = el("button", "mark danger secondary", "×");
    close.type = "button";
    close.title = pane.pinned ? "Pinned panes cannot be removed" : "Take this pane down";
    close.disabled = Boolean(pane.pinned);
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      queued(() => ["room_remove_pane", { id: pane.id }]);
    });
    marks.append(close);
    head.append(marks);

    const noteRow = el("div", "note-row");
    const noteInput = el("input", "note-input");
    noteInput.type = "text";
    noteInput.maxLength = 600;
    noteInput.placeholder = "a note the agent will read…";
    noteInput.value = pane.note || "";
    noteInput.addEventListener("keydown", (event) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      queued(() => ["room_annotate_pane", { id: pane.id, note: noteInput.value }]);
      article.classList.remove("noting");
    });
    noteRow.append(noteInput);

    const body = el("div", "pane-body");
    const scope = paneScope();
    // Tree-order index for every html node — including deck faces that are
    // not mounted — so the renderer and the pane-html route agree on which
    // served document is which.
    const htmlIndex = new Map();
    (function walk(node) {
      if (!node || typeof node !== "object") return;
      if (node.kind === "html") htmlIndex.set(node, htmlIndex.size);
      for (const child of node.children || []) walk(child);
    })(pane.view);
    body.append(renderNode(pane.view, {
      ...ctx,
      openInRoom,
      paneAuthor: pane.author,
      paneId: pane.id,
      htmlIndex,
      revision: state?.revision,
    }, scope));

    article.append(head, noteRow);
    if (pane.note) {
      const shown = el("p", "pane-note");
      shown.append(el("span", "pane-note-mark", "you"), document.createTextNode(pane.note));
      article.append(shown);
    }
    article.append(body);

    // Drag the corner to resize. Any size at all, down to the point where the
    // header stops being readable — a pane nobody can identify is a pane
    // nobody can talk about, which is the only reason there is a floor here.
    const grip = el("div", "pane-grip");
    grip.title = "Drag to resize · shift to ignore the guides · double-click for the whole page";

    grip.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      const start = spotOf(livePane(pane.id) || pane);
      const origin = { x: event.clientX, y: event.clientY };
      const others = otherSpots(pane.id);
      let placed = start;
      article.classList.add("resizing");
      grip.setPointerCapture(event.pointerId);

      const onMove = (move) => {
        const wantedW = Math.max(MIN_W, start.w + (move.clientX - origin.x));
        const wantedH = Math.max(MIN_H, start.h + (move.clientY - origin.y));
        const w = move.shiftKey
          ? { value: wantedW, guide: null }
          : snapSize(wantedW, others, "x", start.x);
        const h = move.shiftKey
          ? { value: wantedH, guide: null }
          : snapSize(wantedH, others, "y", start.y);
        placed = {
          x: start.x,
          y: start.y,
          w: Math.max(MIN_W, w.value),
          h: Math.max(MIN_H, h.value),
        };
        applySpot(article, placed);
        showGuides([
          w.guide === null ? null : { axis: "x", at: w.guide, ...guideSpan("x", w.guide, placed, others) },
          h.guide === null ? null : { axis: "y", at: h.guide, ...guideSpan("y", h.guide, placed, others) },
        ]);
      };

      const finish = () => {
        grip.removeEventListener("pointermove", onMove);
        grip.removeEventListener("pointerup", finish);
        grip.removeEventListener("pointercancel", finish);
        article.classList.remove("resizing");
        clearGuides();
        if (placed.w === start.w && placed.h === start.h) return;
        resizeCanvas();
        queued(() => ["room_arrange", { panes: [{ id: pane.id, spot: placed }] }]);
      };

      grip.addEventListener("pointermove", onMove);
      grip.addEventListener("pointerup", finish);
      grip.addEventListener("pointercancel", finish);
    });

    // The whole page, for when a pane is the only thing worth looking at.
    // Deliberately local and unrecorded, like flipping a deck: how you are
    // looking at the room is not a change to the room.
    grip.addEventListener("dblclick", (event) => {
      event.preventDefault();
      event.stopPropagation();
      expand(pane.id);
    });
    head.addEventListener("dblclick", (event) => {
      if (event.target.closest("button")) return;
      event.preventDefault();
      expand(expanded === pane.id ? null : pane.id);
    });
    article.append(grip);
    if (expanded === pane.id) article.classList.add("expanded");

    // Everything a mark or a note changes, refreshed without touching the body.
    // The body holds the html sandboxes, and rebuilding one throws away
    // whatever the person had going inside it — which is exactly what pointing
    // at it used to do.
    article.__update = (next) => {
      // Position and size are refreshed here rather than keyed, so moving or
      // resizing a pane does not reload what is inside it either. This is what
      // lets a running html sandbox survive being dragged across the canvas.
      if (!article.classList.contains("moving") && !article.classList.contains("resizing")) {
        applySpot(article, spotOf(next));
      }
      title.textContent = next.title;
      by.className = `pane-by by-${next.author}${next.by_name ? " named" : ""}`;
      by.textContent = next.by_name || next.author;
      by.title = next.by_name ? `${next.by_name} — ${next.author}` : next.author;
      article.dataset.author = next.author;

      for (const mark of MARKS) {
        const button = marks.querySelector(`[data-mark="${mark.id}"]`);
        if (button) button.classList.toggle("on", next.mark === mark.id);
        article.classList.toggle(`marked-${mark.id}`, next.mark === mark.id);
      }
      pin.textContent = next.pinned ? "pinned" : "pin";
      close.disabled = Boolean(next.pinned);

      // Never overwrite what they are in the middle of typing.
      if (document.activeElement !== noteInput) noteInput.value = next.note || "";

      let shown = article.querySelector(":scope > .pane-note");
      if (next.note) {
        if (!shown) {
          shown = el("p", "pane-note");
          article.insertBefore(shown, body);
        }
        shown.replaceChildren(
          el("span", "pane-note-mark", "you"),
          document.createTextNode(next.note),
        );
      } else if (shown) {
        shown.remove();
      }

      article.classList.toggle("focused", focused === next.id);
      article.classList.toggle("expanded", expanded === next.id);
      wider.title = expanded === next.id ? "Back to the room (esc)" : "Fill the page";
    };

    // Clicking a pane is the deixis gesture: it makes "this" mean this pane.
    article.addEventListener("mousedown", () => {
      focused = pane.id;
      canvas.querySelectorAll(".pane.focused").forEach((node) => node.classList.remove("focused"));
      article.classList.add("focused");
      ctx.semantic("room.pane.focused", { id: pane.id }).catch(() => {});
      document.dispatchEvent(new CustomEvent("room:focus", {
        detail: { id: pane.id, label: pane.title },
      }));
    });

    return article;
  }

  /// The pane as the room currently has it, which is what a drag must start
  /// from — the closure's `pane` is a snapshot from whenever this element was
  /// built, and starting a second drag from a stale rectangle would teleport it.
  function livePane(id) {
    return state?.panes?.find((entry) => entry.id === id);
  }

  function render(value) {
    if (!value || value.error) {
      canvas.replaceChildren(el("p", "node-error", value?.error || "the room failed to load"));
      return;
    }
    state = value;
    const theme = value.theme || {};
    const style = document.documentElement.style;
    if (theme.accent) style.setProperty("--accent", theme.accent);
    if (Number.isFinite(theme.radius)) style.setProperty("--radius", `${theme.radius}px`);
    if (Number.isFinite(theme.scale)) style.setProperty("--scale", String(theme.scale));
    document.documentElement.dataset.surface = theme.surface || "ink";
    document.documentElement.dataset.density = theme.density || "cozy";

    if (!value.panes.length) {
      panes.clear();
      canvas.replaceChildren(
        el("p", "room-empty", "The room is empty. Ask for something and it will appear here."),
      );
      resizeCanvas();
      document.dispatchEvent(new CustomEvent("room:state", { detail: value }));
      return;
    }
    canvas.querySelector(".room-empty")?.remove();

    // Reuse a pane's element whenever the things it is built from are the same,
    // so a mark or a note refreshes in place. Rebuilding would reload every
    // iframe in the room, and an iframe reload is a sandbox losing its state.
    // Document order no longer decides anything visual — every pane is placed
    // by its own rectangle — so panes are only ever appended or removed, never
    // re-inserted. That closes the last path by which a re-render could reload
    // a sandbox: moving a node reloads its iframes, and now nothing moves.
    const live = new Set();
    for (const pane of value.panes) {
      const key = paneKey(pane);
      const cached = panes.get(pane.id);
      if (cached && cached.key === key) {
        cached.article.__update(pane);
      } else {
        const article = renderPane(pane);
        if (cached) cached.article.remove();
        panes.set(pane.id, { article, key });
        canvas.append(article);
      }
      live.add(pane.id);
    }
    for (const [id, entry] of [...panes.entries()]) {
      if (live.has(id)) continue;
      entry.article.remove();
      panes.delete(id);
    }

    resizeCanvas();
    document.dispatchEvent(new CustomEvent("room:state", { detail: value }));
  }

  // The shell owns the header controls; it asks the extension to act because
  // the extension is what owns these actions.
  const onCommand = (event) => {
    const detail = event.detail || {};
    if (!state) return;
    if (detail.type === "configure") {
      queued(() => ["room_configure", detail.args]);
    } else if (detail.type === "note") {
      queued(() => ["room_put_pane", {
        id: detail.id,
        title: detail.title,
        place: "start",
        view: { kind: "stack", children: [{ kind: "text", text: detail.text }] },
      }]);
    }
  };
  document.addEventListener("room:command", onCommand);

  const unsubscribe = ctx.client.on(EVENT, (event) => render(event.value));
  return {
    dispose() {
      unsubscribe();
      document.removeEventListener("room:command", onCommand);
    },
  };
}
