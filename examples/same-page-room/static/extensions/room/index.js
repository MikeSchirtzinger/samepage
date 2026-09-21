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
/// arriving from here, the browser is where a size is *chosen*, but the server
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
/// its neighbour already uses, and drag a little further to refuse all of them.
const SNAP = 9;
const GRID = 8;
/// Empty space left past the furthest pane so there is always canvas to drag
/// into without having to make room first.
const SLACK = 600;
/// How long one pane stays quiet after a refused gesture. A script forging
/// points is looping by construction, so the refusal has to be visible once
/// and then stop, or the alert becomes the denial of service. Longer than the
/// nine seconds #room-alert stays up, so the person never sees a queue.
const REFUSAL_QUIET_MS = 10_000;
/// Mirrors MAX_POINT_DEPTH and MAX_POINT_FIELD in room.rs. Kept in step by a
/// test there that reads this file, because the host refuses anything past
/// them and a browser sending more would produce an error nobody can act on.
const POINT_DEPTH = 6;
const POINT_FIELD = 120;

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
/// keeps the feedback honest, a guide is only ever drawn for a snap that
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
/// Returns `{ value, guide }`, `guide` is null when the grid caught it, since
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

/// Snap the one edge a resize is dragging.
///
/// A resize pins the opposite edge, so there are only two kinds of offer worth
/// making, and both are expressed as a position for the moving edge so they
/// compete on the same scale: land where a neighbour's edge already is, or land
/// where this pane ends up exactly as wide (or as tall) as that neighbour.
/// Matching two panes by eye is the thing nobody can do, so it gets an offer
/// from whichever edge is in hand rather than only from the bottom-right.
///
/// `pinned` is the edge that is not moving; `dir` is +1 when the trailing edge
/// is in hand and -1 when it is the leading one.
function snapEdge(wanted, others, axis, pinned, dir) {
  const dimension = axis === "x" ? "w" : "h";
  let best = null;
  const offer = (at, guide) => {
    const gap = Math.abs(at - wanted);
    if (gap > SNAP || (best && gap >= best.gap)) return;
    best = { gap, value: at, guide };
  };
  for (const spot of others) {
    const low = axis === "x" ? spot.x : spot.y;
    const high = low + spot[dimension];
    offer(low, low);
    offer(high, high);
    offer(high + GUTTER, high);
    offer(low - GUTTER, low);
    // Same size as that one. No guide: there is no line on screen that would
    // explain "these are now equally wide", and a line that means nothing is
    // worse than none.
    offer(pinned + dir * spot[dimension], null);
  }
  if (best) return { value: best.value, guide: best.guide };
  return { value: Math.round(wanted / GRID) * GRID, guide: null };
}

/// Which edges each handle is holding: +1 trailing, -1 leading, 0 pinned.
const HANDLES = [
  { edge: "n", x: 0, y: -1, cursor: "ns-resize" },
  { edge: "s", x: 0, y: 1, cursor: "ns-resize" },
  { edge: "w", x: -1, y: 0, cursor: "ew-resize" },
  { edge: "e", x: 1, y: 0, cursor: "ew-resize" },
  { edge: "nw", x: -1, y: -1, cursor: "nwse-resize" },
  { edge: "ne", x: 1, y: -1, cursor: "nesw-resize" },
  { edge: "sw", x: -1, y: 1, cursor: "nesw-resize" },
  { edge: "se", x: 1, y: 1, cursor: "nwse-resize" },
];

/// Resolve one axis of a resize into `{ pos, size, guide }`.
///
/// The minimum is applied to the *edge* before the rectangle is derived, so a
/// pane dragged past its floor stops dead instead of turning inside out and
/// walking away from the pointer.
function resizeAxis(dir, startPos, startSize, delta, minimum, others, axis, free) {
  if (dir === 0) return { pos: startPos, size: startSize, guide: null };
  const pinned = dir > 0 ? startPos : startPos + startSize;
  const wanted = (dir > 0 ? startPos + startSize : startPos) + delta;
  const snapped = free
    ? { value: Math.max(0, wanted), guide: null }
    : snapEdge(Math.max(0, wanted), others, axis, pinned, dir);
  let value = Math.max(0, snapped.value);
  if (dir > 0) {
    value = Math.max(value, pinned + minimum);
    return { pos: pinned, size: value - pinned, guide: snapped.guide };
  }
  value = Math.min(value, pinned - minimum);
  return { pos: value, size: pinned - value, guide: snapped.guide };
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
/// the reader's own position, like scroll, it never enters the document.
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
      // click inside a sandbox becomes a pane note, which bumps the revision,
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
      // The host already laid this out, see `view.rs::place`. Everything here
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
          detail: { text: `About "${box.label}" in the diagram. Tell me more about that one.` },
        }));
        group.addEventListener("click", ask);
        group.addEventListener("keydown", (event) => {
          if (event.key === "Enter" || event.key === " ") { event.preventDefault(); ask(); }
        });
        const title = svgEl("title");
        title.textContent = `${box.label}. Ask about this`;
        group.append(title);
        svg.append(group);
      }

      // Edge labels last. A label sits at the midpoint of the gap between two
      // boxes, and it is routinely wider than that gap, drawn with the wires
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
      const conflict = `Port ${option.port} is also used by ${option.contested_by.join(", ")}. Only one can run at a time.`;
      const resolution = option.port_env ? ` Set ${option.port_env} to another port before starting this one.` : "";
      row.append(el("p", "option-note", conflict + resolution));
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
  /// Who this browser is, as the host minted it.
  ///
  /// Fetched before the first render because "you" is not a property of a mark.
  /// It is a comparison between the mark's author and whoever is looking, and
  /// the room broadcasts one state payload to every viewer, so the server
  /// cannot make it for us, it has no idea which of us is reading.
  // Shared with the shell rather than fetched here: both load at once and
  // neither holds a cookie on a first visit, so two fetches admitted this
  // browser as two different people. `whoAmI` resolves once for the page.
  //
  // It resolves to null when the surface has no identity to give, in which case
  // the room still shows every byline and simply shows this person their own
  // name instead of "you", strictly better than calling everybody "you".
  const me = ctx.whoAmI ? await ctx.whoAmI() : null;
  /// Which pane, if any, is filling the page. Local to this browser and never
  /// written back: two people in the room can be looking at different things
  /// without fighting over the document.
  let expanded = null;
  /// Live pane elements by id, with the key they were built from.
  const panes = new Map();

  /// What a pane's DOM is actually built from. Marks, notes, titles, bylines,
  /// pins and sizes are all refreshed in place by `__update`, so none of them
  /// belong here, only the view, whose shape decides the body's structure.
  function paneKey(pane) {
    return JSON.stringify(pane.view);
  }

  /// What to call an author who never announced a name. Mirrors the words the
  /// server uses in the agent's read-back so the two descriptions of one room
  /// do not disagree about what happened in it.
  const CATEGORY = { human: "someone", agent: "agent", companion: "companion" };

  /// The byline as *this* viewer should read it.
  ///
  /// The only place "you" is decided. The document records which participant
  /// wrote a pane; only the browser knows which participant is reading, and
  /// this is where those two facts meet. Comparing ids and never names is the
  /// point: two people who both chose "Sam" would otherwise each read the
  /// other's marks as their own, which is the exact confusion a byline exists
  /// to prevent.
  function bylineOf(pane) {
    // Built on `whoIs` rather than repeating it, because the two resolving a
    // participant differently is precisely how one person ends up reading as
    // "you" on a pane and by name on the note inside it.
    const { mine, name, hue } = whoIs(pane.by_id, pane.by_name);
    return {
      mine,
      text: mine ? "you" : name || CATEGORY[pane.author] || pane.author,
      // The tooltip always states the category the host stands behind, even
      // when the visible label is a chosen name. A label a participant picked
      // never gets to stand in for the claim the host is making, and people
      // pick their own names here exactly like agents do, so a person calling
      // themselves "Codex" still reads as a person on inspection.
      title: name ? `${name}, ${pane.author}` : pane.author,
      named: Boolean(name) && !mine,
      hue,
    };
  }

  /// Who a participant id belongs to, from this viewer's point of view.
  ///
  /// The shared half of every byline in the room: pane authors, note writers
  /// and mark setters all resolve the same way, so one person cannot read as
  /// "you" in one corner of a pane and by name in another.
  function whoIs(id, recorded) {
    const roster = state?.participants || {};
    const known = id ? roster[id] : null;
    const name = known?.name || recorded || null;
    const mine = Boolean(me?.id && id && id === me.id);
    return {
      mine,
      name,
      text: mine ? "you" : name || "someone",
      hue: Number.isFinite(known?.hue) ? known.hue : null,
    };
  }

  /// Paint the mark row, including whose mark it is.
  ///
  /// A mark is the one thing in this room an agent cannot make, so it is the
  /// one most worth attributing, and a disagreement whose author you cannot
  /// see is an argument with nobody. The glyph carries the marker's colour and
  /// says their name on hover; it stays uncoloured when the mark is your own,
  /// because everyone sees their own marks as theirs.
  function applyMarks(article, marks, pane) {
    const who = whoIs(pane.mark_by, pane.mark_by_name);
    for (const mark of MARKS) {
      const button = marks.querySelector(`[data-mark="${mark.id}"]`);
      const on = pane.mark === mark.id;
      article.classList.toggle(`marked-${mark.id}`, on);
      if (!button) continue;
      button.classList.toggle("on", on);
      button.title = on ? `${mark.title}, ${who.text}` : mark.title;
      if (on && !who.mine && who.hue !== null) {
        button.style.setProperty("--by-hue", String(who.hue));
      } else {
        button.style.removeProperty("--by-hue");
      }
    }
  }

  /// Paint a byline. Shared by the first build and every in-place refresh, so
  /// the two can never drift into describing the same author differently.
  function applyByline(by, article, pane) {
    const credit = bylineOf(pane);
    by.className = `pane-by by-${pane.author}${credit.named ? " named" : ""}${credit.mine ? " mine" : ""}`;
    by.textContent = credit.text;
    by.title = credit.title;
    // The hue comes from the host, which assigned it from the participant's
    // principal. Deriving it here from a name would give two same-named people
    // the same colour, undoing the one thing the colour is for.
    if (credit.hue === null) by.style.removeProperty("--by-hue");
    else by.style.setProperty("--by-hue", String(credit.hue));
    article.dataset.author = pane.author;
    article.dataset.mine = credit.mine ? "true" : "false";
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

  /// Which pane is in front, from the document rather than from the order the
  /// panes happen to have been built in.
  ///
  /// Document order used to decide this, which is why some panes could never be
  /// brought forward however many times you clicked them: their place in the
  /// list was fixed and touching one did not change it.
  ///
  /// The z-index is the pane's *rank* among the room's stack numbers, not the
  /// number itself. A rank is bounded by the number of panes, so a long session
  /// of clicking cannot climb into the bands reserved for the pane in hand or
  /// the one filling the page.
  function applyStack(article, id) {
    const order = (state?.panes || [])
      .map((pane) => ({ id: pane.id, stack: Number.isFinite(pane.stack) ? pane.stack : 0 }))
      .sort((a, b) => a.stack - b.stack || (a.id < b.id ? -1 : 1));
    const rank = order.findIndex((pane) => pane.id === id);
    article.style.zIndex = String(1 + Math.max(0, rank));
  }

  /// Bring a pane in front of the ones it overlaps, if it is not already there.
  ///
  /// Skipped when nothing overlaps it, and skipped when it is already in front:
  /// raising is a write, and a write nobody can see is a revision the room did
  /// not need. It is recorded silently, see `mutate` in `room.rs`, so this
  /// never wakes an agent parked on the room.
  function raise(id) {
    const panesNow = state?.panes || [];
    const me = panesNow.find((pane) => pane.id === id);
    if (!me) return;
    const mine = spotOf(me);
    const stackOf = (pane) => (Number.isFinite(pane.stack) ? pane.stack : 0);
    const overlapping = panesNow.filter((pane) => {
      if (pane.id === id) return false;
      const other = spotOf(pane);
      return mine.x < other.x + other.w && other.x < mine.x + mine.w
        && mine.y < other.y + other.h && other.y < mine.y + mine.h;
    });
    if (!overlapping.length) return;
    if (overlapping.every((pane) => stackOf(pane) < stackOf(me))) return;
    queued(() => ["room_arrange", { panes: [{ id, raise: true }] }]);
  }

  /// Everyone else's rectangle, what a drag snaps against.
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
  // cannot pull is a canvas you keep losing your place on, and the gesture has
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
  // clicked in the same tick would make the second one stale, nudge a slider
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
  // it happened, and the label is recorded against that pane.
  //
  // It goes to `pointed`, not to `note`. Both are the person speaking and both
  // are human-only, but a note is a sentence they chose and a point is a
  // gesture the page caught. Sharing one slot meant opening the note box
  // presented them with the last thing they clicked, to be deleted before they
  // could write anything.
  window.addEventListener("message", (event) => {
    // No truncation here. This used to `.slice(0, 560)`, which silently cut a
    // gesture down before it could reach the room, three freehand strokes
    // arrived as exactly 560 characters and looked like they had fit. The
    // host owns the limit (MAX_POINTED) and rejects an over-length point out
    // loud, which is now visible, so a report is either delivered whole or
    // refused where the person can see it.
    const label = event.data && typeof event.data.aguiPoint === "string"
      ? event.data.aguiPoint.trim()
      : "";
    if (!label) return;
    const frame = [...document.querySelectorAll("iframe.html-frame")]
      .find((candidate) => candidate.contentWindow === event.source);
    const paneId = frame?.dataset.paneId;
    if (!paneId) return;
    // An unchanged point is not re-written. Without this, a script that
    // reports on load re-fires on every re-render, each write bumps the
    // revision, the new snapshot re-renders, the frame refetches, and the
    // room spins revisions forever.
    const current = state?.panes?.find((pane) => pane.id === paneId)?.pointed;
    if (current === label) return;
    if (!admitGesture(paneId)) return;
    // No corroboration flag travels with this. A field saying "a person did
    // this" would be one more claim to trust, and the room already has too
    // many. The gate refuses at the boundary instead, so a stored point is
    // one that passed rather than one that says it did.
    const path = pointPath(event.data);
    queued(() => ["room_annotate_pane", {
      id: paneId,
      pointed: label,
      ...(path.length ? { pointed_path: path } : {}),
    }]);
  });

  /// What the point was sitting inside, innermost first.
  ///
  /// Sanitised here rather than trusted, even though the bridge that builds
  /// it is ours: the agent's own script can post `aguiPath` alongside its
  /// `aguiPoint`, and this is the boundary where the difference stops
  /// mattering. The host refuses a malformed one outright, so anything that
  /// would be rejected there is dropped here instead of costing a round trip
  /// and a visible error the person cannot act on.
  function pointPath(data) {
    if (!Array.isArray(data?.aguiPath)) return [];
    return data.aguiPath
      .slice(0, POINT_DEPTH)
      .map((level) => ({
        role: text(level?.role),
        name: text(level?.name),
        point: text(level?.point),
      }))
      .filter((level) => level.role || level.name || level.point)
      .map((level) => {
        // `point` is omitted rather than sent empty, matching the host's
        // shape where its absence means the author never labelled that
        // level.
        if (!level.point) delete level.point;
        return level;
      });
  }

  function text(value) {
    return typeof value === "string" ? value.replace(/\s+/g, " ").trim().slice(0, POINT_FIELD) : "";
  }

  /// The admission gate on a gesture out of a sandbox.
  ///
  /// Every other person-attributed write in this room is made with the
  /// room's own controls, which only a human can work. A point is the
  /// exception: it arrives by `postMessage` from a frame whose script the
  /// agent wrote, so the agent can post one with nobody in the room. That was
  /// reproducible, a pane whose only script was a timer, a browser that
  /// touched nothing, and the room recorded a person pointing at what the
  /// pane wanted it to.
  ///
  /// `navigator.userActivation.isActive` is the one signal that survives the
  /// trip. Genuine activation propagates out of a sandboxed child through
  /// every ancestor unfiltered by origin, and the frame's own script cannot
  /// set it. `isTrusted` cannot be used instead: it is unforgeable, but it
  /// lives on the identity of the Event object and does not survive being
  /// serialized across a message.
  ///
  /// Transient (`isActive`) and never sticky (`hasBeenActive`). Sticky turns
  /// true the first time anybody touches the page and then stays true for the
  /// life of the document, which would admit every forgery after the first
  /// real click and read like a working gate.
  ///
  /// What this does NOT prove, and the read-back must not imply: which
  /// element, which action, or that this point is the one they touched. An
  /// agent that waits for a real click can ride it. That is a smaller hole
  /// than needing nobody present at all, and it is the whole of what a
  /// same-origin capture layer above the frame would have closed, at the cost
  /// of the frame no longer being touchable.
  function admitGesture(paneId) {
    const activation = navigator.userActivation;
    if (!activation) {
      // Refused rather than admitted. Letting it through here would be a
      // fallback that quietly restores the hole on whichever browser lacks
      // the API, and a gate with a browser-shaped bypass is not a gate.
      refuseGesture(paneId, "this browser cannot confirm a person is here (no navigator.userActivation), so a point out of a sandbox cannot be trusted and was not recorded");
      return false;
    }
    if (!activation.isActive) {
      refuseGesture(paneId, "a point arrived from inside a pane with no recent input behind it, so it was not recorded as yours, an agent's script can send one on its own");
      return false;
    }
    return true;
  }

  /// A refused gesture is shown, not swallowed. The person needs to know when
  /// something claimed to be theirs, and swallowing it would leave a forging
  /// agent invisible. Throttled per pane, because the script doing this is in
  /// a loop by construction and an unthrottled alert would bury the room.
  const lastRefusal = new Map();
  function refuseGesture(paneId, message) {
    const now = Date.now();
    if (now - (lastRefusal.get(paneId) ?? -Infinity) < REFUSAL_QUIET_MS) return;
    lastRefusal.set(paneId, now);
    document.dispatchEvent(new CustomEvent("room:error", { detail: { message } }));
  }

  /// Everything the person has said about a pane, each in its own voice.
  ///
  /// `note` is a sentence they typed. `pointed` is the last thing they clicked
  /// inside a sandbox, which the room recorded for them. Both are theirs and
  /// neither is the agent's, but only one of them is writing, so only one of
  /// them belongs in the box they type into.
  function showSaid(into, pane) {
    const rows = [];
    if (pane.note) {
      const row = el("p", "pane-note");
      // Signed with whoever actually wrote it. This said "you" unconditionally,
      // which was true for the only reader a single-occupancy room had and
      // became a lie about the other person the moment there were two.
      const who = whoIs(pane.note_by, pane.note_by_name);
      const tag = el("span", "pane-note-mark", who.text);
      if (who.hue !== null) tag.style.setProperty("--by-hue", String(who.hue));
      if (who.mine) tag.classList.add("mine");
      row.append(tag, document.createTextNode(pane.note));
      rows.push(row);
    }
    if (pane.pointed) {
      const row = el("p", "pane-note pane-pointed");
      // Signed the same way, for the same reason: with two people in the room,
      // "somebody clicked Run" is not something either of them can act on.
      // "pointed" alone when nobody was identified, which is the pre-identity
      // case and still what a caller who never joined gets.
      const who = whoIs(pane.pointed_by, pane.pointed_by_name);
      const signed = who.mine || who.name;
      const tag = el("span", "pane-note-mark", signed ? `${who.text} pointed` : "pointed");
      if (who.hue !== null) tag.style.setProperty("--by-hue", String(who.hue));
      if (who.mine) tag.classList.add("mine");
      row.append(tag, document.createTextNode(pane.pointed));
      rows.push(row);
    }
    into.replaceChildren(...rows);
  }

  /// Hold the room still while a pane is being dragged or resized.
  ///
  /// A press that becomes a drag has already started a text selection by then:
  /// `preventDefault` on `pointerdown` does not stop Chrome firing `mousedown`,
  /// and the pane's own `user-select: none` only covers the pane, not the
  /// siblings the pointer then sweeps across. So a drag used to paint the whole
  /// room blue and leave the selection behind after the drop. Clear what has
  /// been selected already, and mark the canvas so nothing new can be.
  function holdStill(on) {
    canvas.classList.toggle("dragging", on);
    if (on) window.getSelection()?.removeAllRanges();
  }

  /// Move a pane with the pointer.
  ///
  /// `threshold` is how far the pointer has to travel before this counts as a
  /// move at all. The header commits immediately, that strip exists to be
  /// dragged. A press on the body has to wait, because the same press is also
  /// how you click the button under it, and only the travel tells them apart.
  ///
  /// Returns true once it has committed, so the caller can swallow the click
  /// that a completed drag would otherwise deliver to whatever was underneath.
  function startMove(article, paneId, event, threshold) {
    const start = spotOf(livePane(paneId) || {});
    const origin = { x: event.clientX, y: event.clientY };
    const others = otherSpots(paneId);
    const source = event.currentTarget;
    let placed = start;
    let moving = threshold === 0;
    if (moving) {
      article.classList.add("moving");
      holdStill(true);
    }
    source.setPointerCapture(event.pointerId);

    const onMove = (move) => {
      if (!moving) {
        const travel = Math.hypot(move.clientX - origin.x, move.clientY - origin.y);
        if (travel < threshold) return;
        moving = true;
        article.classList.add("moving");
        // Only now, once the press has turned out to be a drag. Clearing on
        // press would wipe a selection they made deliberately and then clicked
        // beside.
        holdStill(true);
      }
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
      source.removeEventListener("pointermove", onMove);
      source.removeEventListener("pointerup", finish);
      source.removeEventListener("pointercancel", finish);
      article.classList.remove("moving");
      holdStill(false);
      clearGuides();
      if (!moving) return;
      // A drag that travelled is not also a click on whatever it started over.
      // Armed only here, and disarmed on the next turn of the loop, so a drag
      // released somewhere that never delivers a click cannot leave a trap for
      // an unrelated click later.
      const swallow = (click) => {
        click.preventDefault();
        click.stopPropagation();
      };
      window.addEventListener("click", swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener("click", swallow, true), 0);
      if (placed.x === start.x && placed.y === start.y) return;
      resizeCanvas();
      queued(() => ["room_arrange", { panes: [{ id: paneId, spot: placed }] }]);
    };

    source.addEventListener("pointermove", onMove);
    source.addEventListener("pointerup", finish);
    source.addEventListener("pointercancel", finish);
  }

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
      startMove(article, pane.id, event, 0);
    });

    const title = el("h3", "pane-title", pane.title);
    // Print the author the host recorded rather than collapsing everything
    // that is not the person into "agent", "companion" is a distinct claim
    // (an assistant reaching in from outside the room) and flattening it here
    // would throw away the byline the server went to the trouble of keeping.
    // The category stays in the class either way, so "agent" and "companion"
    // keep looking different at a glance no matter what anyone decided to call
    // themselves.
    const by = el("span", "pane-by");
    applyByline(by, article, pane);
    head.append(title, by);

    const marks = el("div", "pane-marks");
    for (const mark of MARKS) {
      const button = el("button", "mark", mark.glyph);
      button.type = "button";
      button.title = mark.title;
      button.dataset.mark = mark.id;
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
    // Once all four exist, so the pass that colours the active one can find it.
    applyMarks(article, marks, pane);
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
    // Tree-order index for every html node, including deck faces that are
    // not mounted, so the renderer and the pane-html route agree on which
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
    // Two lines, never one. What the person wrote is signed "you"; what they
    // clicked is signed "pointed" and reads as the record it is. Collapsing
    // them was what put a click label in the note box.
    const lines = el("div", "pane-said");
    article.append(lines);
    showSaid(lines, pane);
    article.append(body);

    // Every edge and every corner resizes, because the edge you want is the one
    // nearest whatever you are trying to line the pane up with, and a pane you
    // can only grow down and to the right is one you have to move first and
    // resize second. Any size at all, down to the point where the header stops
    // being readable: a pane nobody can identify is a pane nobody can talk
    // about, which is the only reason there is a floor here.
    let grip = null;
    for (const handle of HANDLES) {
      const node = el("div", `pane-handle handle-${handle.edge}`);
      node.title = "Drag to resize · shift to ignore the guides";
      // The bottom-right one carries the visible chevron and the double-click,
      // so the affordance stays exactly where people already look for it.
      if (handle.edge === "se") {
        grip = node;
        node.classList.add("pane-grip");
        node.title = "Drag to resize · shift to ignore the guides · double-click for the whole page";
      }

      node.addEventListener("pointerdown", (event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.stopPropagation();
        const start = spotOf(livePane(pane.id) || pane);
        const origin = { x: event.clientX, y: event.clientY };
        const others = otherSpots(pane.id);
        let placed = start;
        article.classList.add("resizing");
        holdStill(true);
        node.setPointerCapture(event.pointerId);

        const onMove = (move) => {
          const free = move.shiftKey;
          const x = resizeAxis(
            handle.x, start.x, start.w, move.clientX - origin.x, MIN_W, others, "x", free,
          );
          const y = resizeAxis(
            handle.y, start.y, start.h, move.clientY - origin.y, MIN_H, others, "y", free,
          );
          placed = { x: x.pos, y: y.pos, w: x.size, h: y.size };
          applySpot(article, placed);
          showGuides([
            x.guide === null ? null : { axis: "x", at: x.guide, ...guideSpan("x", x.guide, placed, others) },
            y.guide === null ? null : { axis: "y", at: y.guide, ...guideSpan("y", y.guide, placed, others) },
          ]);
        };

        const finish = () => {
          node.removeEventListener("pointermove", onMove);
          node.removeEventListener("pointerup", finish);
          node.removeEventListener("pointercancel", finish);
          article.classList.remove("resizing");
          holdStill(false);
          clearGuides();
          if (placed.x === start.x && placed.y === start.y
            && placed.w === start.w && placed.h === start.h) return;
          resizeCanvas();
          queued(() => ["room_arrange", { panes: [{ id: pane.id, spot: placed }] }]);
        };

        node.addEventListener("pointermove", onMove);
        node.addEventListener("pointerup", finish);
        node.addEventListener("pointercancel", finish);
      });

      article.append(node);
    }

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
    if (expanded === pane.id) article.classList.add("expanded");

    // Everything a mark or a note changes, refreshed without touching the body.
    // The body holds the html sandboxes, and rebuilding one throws away
    // whatever the person had going inside it, which is exactly what pointing
    // at it used to do.
    article.__update = (next) => {
      // Position and size are refreshed here rather than keyed, so moving or
      // resizing a pane does not reload what is inside it either. This is what
      // lets a running html sandbox survive being dragged across the canvas.
      if (!article.classList.contains("moving") && !article.classList.contains("resizing")) {
        applySpot(article, spotOf(next));
      }
      applyStack(article, next.id);
      title.textContent = next.title;
      applyByline(by, article, next);

      applyMarks(article, marks, next);
      pin.textContent = next.pinned ? "pinned" : "pin";
      close.disabled = Boolean(next.pinned);

      // Never overwrite what they are in the middle of typing, and never seed
      // it with anything but their own note, which is the whole point of
      // keeping `pointed` in its own field.
      if (document.activeElement !== noteInput) noteInput.value = next.note || "";
      showSaid(lines, next);

      article.classList.toggle("focused", focused === next.id);
      article.classList.toggle("expanded", expanded === next.id);
      wider.title = expanded === next.id ? "Back to the room (esc)" : "Fill the page";
    };

    // An unselected pane moves from anywhere on it, the way an unfocused window
    // does: you should not have to find the title bar to shove something out of
    // the way. Once it is selected the body is content again and a press there
    // means what the content says it means.
    //
    // `pointerdown` runs before `mousedown`, so this still sees the selection
    // as it was *before* this very press selects the pane, which is the whole
    // distinction being drawn.
    article.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || expanded === pane.id) return;
      if (focused === pane.id) return;
      if (event.target.closest(".pane-head, .pane-handle, input, textarea, select")) return;
      // Not preventDefault: the press has not committed to anything yet, and
      // swallowing it here would break every control it might still turn out
      // to be a click on.
      startMove(article, pane.id, event, 4);
    });

    // Clicking a pane is the deixis gesture: it makes "this" mean this pane.
    // It also brings it forward, because the thing you just reached for being
    // the thing you can see is not a preference, it is what every window on
    // the person's desktop already does.
    article.addEventListener("mousedown", () => {
      focused = pane.id;
      canvas.querySelectorAll(".pane.focused").forEach((node) => node.classList.remove("focused"));
      article.classList.add("focused");
      raise(pane.id);
      ctx.semantic("room.pane.focused", { id: pane.id }).catch(() => {});
      document.dispatchEvent(new CustomEvent("room:focus", {
        detail: { id: pane.id, label: pane.title },
      }));
    });

    return article;
  }

  /// The pane as the room currently has it, which is what a drag must start
  /// from, the closure's `pane` is a snapshot from whenever this element was
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
    // Document order no longer decides anything visual, every pane is placed
    // by its own rectangle, so panes are only ever appended or removed, never
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
    // After every pane exists, so a rank is computed against the whole room
    // rather than against however much of it had been built at the time.
    for (const pane of value.panes) applyStack(panes.get(pane.id).article, pane.id);
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
