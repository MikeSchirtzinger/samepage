// Minimal Map entry: projection and plans come from the verified Map WASM.
// The shell owns the single document, authenticated socket and lifecycle.
export function activate({ mount, wasm, application }) {
  if (!application || !wasm) throw new Error("Map needs the verified capability and shared document context");
  application.registerCapability("map", wasm);
  const count = document.getElementById("map-count");
  let disposed = false;
  let previousRevision = "";

  // The map is one scroll owner: a camera (scale and translate) on the stage.
  // Wheel pans, ctrl or cmd with the wheel zooms at the pointer, dragging the
  // empty background or holding Space pans, Fit frames everything. Card
  // geometry is always read in stage units, never through the camera.
  let camera = {scale: 1, x: 0, y: 0};
  let fitted = false, spaceHeld = false, selectedId = null;
  const zoomLabel = document.getElementById("zoom-level");
  const clampScale = (value) => Math.min(2.5, Math.max(0.2, value));
  const typing = (target) => Boolean(target?.closest?.("input, textarea, select, [contenteditable=''], [contenteditable='true']"));
  function applyCamera() {
    const stage = mount.querySelector(".map-stage");
    if (stage) stage.style.transform = `translate(${camera.x}px, ${camera.y}px) scale(${camera.scale})`;
    mount.dataset.scale = String(camera.scale);
    if (zoomLabel) zoomLabel.textContent = `${Math.round(camera.scale * 100)}%`;
  }
  function zoomAt(factor, cx = mount.clientWidth / 2, cy = mount.clientHeight / 2) {
    const scale = clampScale(camera.scale * factor);
    const k = scale / camera.scale;
    camera = {scale, x: cx - (cx - camera.x) * k, y: cy - (cy - camera.y) * k};
    applyCamera();
  }
  function contentBox() {
    const stage = mount.querySelector(".map-stage");
    if (!stage) return null;
    let l = Infinity, t = Infinity, r = -Infinity, b = -Infinity;
    const take = (x1, y1, x2, y2) => { l = Math.min(l, x1); t = Math.min(t, y1); r = Math.max(r, x2); b = Math.max(b, y2); };
    for (const card of stage.querySelectorAll(".map-card")) take(card.offsetLeft, card.offsetTop, card.offsetLeft + card.offsetWidth, card.offsetTop + card.offsetHeight);
    const drawn = stage.querySelector(".sketch-layer > g");
    if (drawn?.childNodes.length) {
      const box = drawn.getBBox();
      const ox = Number(stage.dataset.originX), oy = Number(stage.dataset.originY);
      if (box.width || box.height) take(box.x + ox, box.y + oy - 28, box.x + ox + box.width, box.y + oy + box.height);
    }
    return Number.isFinite(l) ? {l, t, r, b} : null;
  }
  function fit() {
    const box = contentBox();
    if (!box || !mount.clientWidth) return false;
    const pad = 32, width = mount.clientWidth, height = mount.clientHeight;
    const w = Math.max(1, box.r - box.l), h = Math.max(1, box.b - box.t);
    const scale = clampScale(Math.min(1, (width - 2 * pad) / w, (height - 2 * pad - 40) / h));
    camera = {scale, x: (width - w * scale) / 2 - box.l * scale, y: Math.max(pad, (height - 40 - h * scale) / 2) - box.t * scale};
    applyCamera();
    return true;
  }
  // Bring one card into view without changing the zoom.
  function reveal(id) {
    const card = [...mount.querySelectorAll(".map-card")].find(node => node.dataset.id === id);
    if (!card) return false;
    const cx = (card.offsetLeft + card.offsetWidth / 2) * camera.scale + camera.x;
    const cy = (card.offsetTop + card.offsetHeight / 2) * camera.scale + camera.y;
    if (cx < 40 || cy < 40 || cx > mount.clientWidth - 40 || cy > mount.clientHeight - 40) {
      camera = {...camera, x: camera.x + mount.clientWidth / 2 - cx, y: camera.y + mount.clientHeight / 2 - cy};
      applyCamera();
    }
    return true;
  }
  function select(id) {
    selectedId = id;
    mount.querySelectorAll(".map-card").forEach(node => node.classList.toggle("map-selected", node.dataset.id === id));
    const card = id ? [...mount.querySelectorAll(".map-card")].find(node => node.dataset.id === id) : null;
    const label = card?.querySelector("h2")?.textContent || "";
    const detail = id ? {id, label, count: 1, summary: label, kind: "node"} : null;
    window.samepageSelection = detail;
    document.dispatchEvent(new CustomEvent("map-selection-changed", {detail}));
  }
  const onWheel = (event) => {
    if (event.target.closest?.(".map-constraints, .diagram-legend, .sketch-editor")) return;
    event.preventDefault();
    const box = mount.getBoundingClientRect();
    if (event.ctrlKey || event.metaKey) zoomAt(Math.exp(-event.deltaY * 0.0025), event.clientX - box.left, event.clientY - box.top);
    else { camera = {...camera, x: camera.x - event.deltaX, y: camera.y - event.deltaY}; applyCamera(); }
  };
  const onPanStart = (event) => {
    const background = !event.target.closest?.(".map-card, .sketch-shape, .sketch-editor, .sketch-grip, button, input, textarea, select, a, .map-constraints, .diagram-legend, .map-link-hit");
    const tool = mount.dataset.sketchTool || "select";
    if (!(event.button === 1 || (event.button === 0 && spaceHeld) || (event.button === 0 && background && tool === "select"))) return;
    event.preventDefault(); event.stopImmediatePropagation();
    const start = {x: event.clientX, y: event.clientY, cx: camera.x, cy: camera.y};
    let moved = false;
    mount.dataset.panning = "true";
    const move = (e) => {
      if (Math.abs(e.clientX - start.x) + Math.abs(e.clientY - start.y) > 3) moved = true;
      camera = {...camera, x: start.cx + e.clientX - start.x, y: start.cy + e.clientY - start.y}; applyCamera();
    };
    const stop = () => {
      document.removeEventListener("pointermove", move, true); document.removeEventListener("pointerup", stop, true); document.removeEventListener("pointercancel", stop, true);
      delete mount.dataset.panning;
      if (!moved && background && event.button === 0 && !spaceHeld) select(null);
    };
    document.addEventListener("pointermove", move, true); document.addEventListener("pointerup", stop, true); document.addEventListener("pointercancel", stop, true);
  };
  const onKey = (event) => {
    if (event.code === "Space" && !typing(event.target)) {
      if (event.type === "keydown") { spaceHeld = true; mount.dataset.space = "true"; event.preventDefault(); }
      else { spaceHeld = false; delete mount.dataset.space; }
      return;
    }
    if (event.type !== "keydown" || event.metaKey || event.ctrlKey || event.altKey || typing(event.target)) return;
    if (event.key === "f" || event.key === "F") { fit(); event.preventDefault(); }
    else if (event.key === "+" || event.key === "=") { zoomAt(1.2); event.preventDefault(); }
    else if (event.key === "-" || event.key === "_") { zoomAt(1 / 1.2); event.preventDefault(); }
    else if (event.key === "0") { zoomAt(1 / camera.scale); event.preventDefault(); }
    else if (event.key === "Escape") { select(null); document.dispatchEvent(new CustomEvent("samepage:sketch-tool", {detail: {tool: "select"}})); }
  };
  const onFocus = (event) => { if (event.detail?.id) reveal(event.detail.id); };
  mount.addEventListener("wheel", onWheel, {passive: false});
  mount.addEventListener("pointerdown", onPanStart, true);
  document.addEventListener("keydown", onKey);
  document.addEventListener("keyup", onKey);
  document.addEventListener("samepage:map-focus", onFocus);
  const controls = [["zoom-in", () => zoomAt(1.2)], ["zoom-out", () => zoomAt(1 / 1.2)], ["zoom-fit", () => fit()]];
  for (const [id, action] of controls) document.getElementById(id)?.addEventListener("click", action);
  globalThis.samepageMapCamera = () => ({...camera});

  // Interface §7: height = max(h_pref, h_min, measurement or estimate). The
  // projection already supplies the server's own max(h_pref, h_min, estimate)
  // as `card.height`. A browser may know better than that text-based
  // estimate, because it can render the card and read its own layout back.
  // This substitutes the real local measurement for the estimate, in the
  // same maximum, in this browser only: nothing measured here is ever written
  // to the shared document, and native `measured_known` stays false.
  function measureLocalHeights(cards) {
    const local = new Map();
    // Falls back to the server's own estimate (via `heightOf`'s `??`) when
    // this context cannot lay out real DOM nodes, e.g. a non-browser harness.
    if (typeof document === "undefined" || !document.body || typeof document.createElement !== "function") {
      return local;
    }
    const probe = document.createElement("div");
    probe.className = "map-measure-probe";
    const nodes = new Map();
    for (const card of cards) {
      const node = document.createElement("article");
      node.className = "map-card";
      node.style.setProperty("--card-width", `${Number(card.width) || 232}px`);
      node.style.minHeight = "0"; // unconstrained: measure content, not a floor
      const title = document.createElement("h2");
      title.textContent = card.label || "Untitled card";
      node.appendChild(title);
      if (card.note) {
        const note = document.createElement("p");
        note.textContent = card.note;
        node.appendChild(note);
      }
      const byline = document.createElement("div");
      byline.className = "byline";
      byline.textContent = `Created by ${card.created_by || "unknown"} · touched by ${card.touched_by || "unknown"}`;
      node.appendChild(byline);
      probe.appendChild(node);
      nodes.set(card.id, node);
    }
    try {
      document.body.appendChild(probe);
      for (const card of cards) {
        const node = nodes.get(card.id);
        if (typeof node.getBoundingClientRect !== "function") continue;
        const measured = node.getBoundingClientRect().height;
        if (!Number.isFinite(measured) || measured <= 0) continue;
        const hPref = Number(card.h_pref) || 0;
        const hMin = Number(card.h_min) || 0;
        const authored = Math.max(hPref, hMin);
        const height = Math.max(authored, measured);
        // Same rule the native projection uses to choose `preferred` over
        // `estimated`, with this browser's real measurement standing in for
        // the estimate: the authored number only governs when it already
        // covers what the content needs.
        const source = authored > 0 && authored >= measured ? "preferred" : "local_measured";
        local.set(card.id, { height, source, measured });
      }
    } finally {
      probe.remove();
    }
    return local;
  }

  async function render({view, revision}) {
    if (disposed) return;
    const began = performance.now();
    const projection = await application.project("map", view);
    const cards = projection.cards || [];
    const links = projection.links || [];
    const constraints = projection.constraints || [];
    const byId = new Map(cards.map((card) => [card.id, card]));
    const local = measureLocalHeights(cards);
    const heightOf = (card) => local.get(card.id)?.height ?? Number(card.height || 100);
    const originX = 42 - Math.min(0, ...cards.map((card) => Number(card.x) || 0));
    const originY = 42 - Math.min(0, ...cards.map((card) => Number(card.y) || 0));
    const width = Math.max(mount.clientWidth, ...cards.map((card) => originX + Number(card.x || 0) + Number(card.width || 232) + 42));
    const height = Math.max(mount.clientHeight, ...cards.map((card) => originY + Number(card.y || 0) + heightOf(card) + 42));
    const stage = document.createElement("div");
    stage.className = "map-stage";
    stage.style.transform = `translate(${camera.x}px, ${camera.y}px) scale(${camera.scale})`;
    stage.dataset.originX = String(originX);
    stage.dataset.originY = String(originY);
    stage.style.width = `${width}px`;
    stage.style.height = `${height}px`;
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.classList.add("map-links");
    svg.setAttribute("width", String(width));
    svg.setAttribute("height", String(height));
    svg.setAttribute("aria-hidden", "true");
    for (const link of links) {
      if (link.transition) continue;
      const from = byId.get(link.from);
      const to = byId.get(link.to);
      if (!from || !to) continue;
      const x1 = originX + Number(from.x || 0) + Number(from.width || 232) / 2;
      const y1 = originY + Number(from.y || 0) + heightOf(from) / 2;
      const x2 = originX + Number(to.x || 0) + Number(to.width || 232) / 2;
      const y2 = originY + Number(to.y || 0) + heightOf(to) / 2;
      const line = document.createElementNS(svg.namespaceURI, "line");
      line.classList.add("map-link");
      line.dataset.id = link.id;
      for (const [key, value] of Object.entries({x1,y1,x2,y2})) line.setAttribute(key, String(value));
      svg.appendChild(line);
      if (link.label) {
        const label = document.createElementNS(svg.namespaceURI, "text");
        label.classList.add("map-link-label");
        label.setAttribute("x", String((x1+x2)/2));
        label.setAttribute("y", String((y1+y2)/2 - 7));
        label.textContent = link.label;
        svg.appendChild(label);
      }
    }
    stage.appendChild(svg);
    const fragment = document.createDocumentFragment();
    for (const card of cards) {
      const node = document.createElement("article");
      node.className = "map-card";
      node.dataset.id = card.id;
      if (cards.some(child => child.parent === card.id)) node.classList.add("map-container");
      node.dataset.status = card.status || "open";
      const geometry = local.get(card.id);
      node.dataset.heightSource = geometry?.source || card.height_source || "estimated";
      node.style.left = `${originX + Number(card.x || 0)}px`;
      node.style.top = `${originY + Number(card.y || 0)}px`;
      node.style.setProperty("--card-width", `${Number(card.width || 232)}px`);
      node.style.setProperty("--card-height", `${heightOf(card)}px`);
      const title = document.createElement("h2");
      title.textContent = card.label || "Untitled card";
      node.appendChild(title);
      if (card.note) {
        const note = document.createElement("p");
        note.textContent = card.note;
        node.appendChild(note);
      }
      const byline = document.createElement("div");
      byline.className = "byline";
      byline.textContent = `Created by ${card.created_by || "unknown"} · touched by ${card.touched_by || "unknown"}`;
      node.appendChild(byline);
      // Local-only geometry readout. Never written back to the document:
      // native `measured_known` stays false, and only this browser's own
      // render informs `height_source` here.
      const meta = document.createElement("div");
      meta.className = "map-card-meta";
      const sourceLabel = geometry?.source || card.height_source || "estimated";
      meta.textContent = `height ${Math.round(heightOf(card))}px · ${sourceLabel}`;
      if (card.legacy_h !== null && card.legacy_h !== undefined) {
        meta.textContent += ` · legacy h ${card.legacy_h}`;
      }
      node.appendChild(meta);
      if (application.hasCapability?.("review")) {
        const question = document.createElement("button");
        question.type = "button";
        question.className = "review-open";
        question.textContent = "?";
        question.setAttribute("aria-label", `Question ${card.label || "card"}`);
        question.addEventListener("pointerdown", event => event.stopPropagation());
        question.addEventListener("click", async () => {
          try {
            await application.activateForTool("mark_add");
            document.dispatchEvent(new CustomEvent("samepage:review-open", {detail: {id:card.id}}));
          } catch (error) {
            application.problem?.("review", "load", `Review is unavailable: ${error.message}`);
          }
        });
        node.appendChild(question);
      }
      node.classList.toggle("map-selected", card.id === selectedId);
      node.addEventListener("pointerdown", (event) => {
        if (event.button !== 0 || !application.ready() || spaceHeld) return;
        event.preventDefault();
        const startX = event.clientX;
        const startY = event.clientY;
        const scale = camera.scale;
        node.setPointerCapture(event.pointerId);
        const moving = (move) => {
          node.style.transform = `translate(${(move.clientX - startX) / scale}px, ${(move.clientY - startY) / scale}px)`;
        };
        const finish = async (up) => {
          node.removeEventListener("pointermove", moving);
          node.removeEventListener("pointerup", finish);
          node.removeEventListener("pointercancel", cancel);
          node.style.transform = "";
          const dx = (up.clientX - startX) / scale;
          const dy = (up.clientY - startY) / scale;
          if (Math.abs(dx) + Math.abs(dy) < 3) { select(card.id); return; }
          try {
            const planned = await application.plan("map", "card_move", {
              id: card.id, x: Number(card.x || 0) + dx, y: Number(card.y || 0) + dy,
            });
            await application.applyBatch(planned);
            application.resolved?.("map", "card_move");
          } catch (error) {
            application.problem?.("map", "card_move", `Card move refused: ${error.message}`);
          }
        };
        const cancel = () => {
          node.removeEventListener("pointermove", moving);
          node.removeEventListener("pointerup", finish);
          node.removeEventListener("pointercancel", cancel);
          node.style.transform = "";
        };
        node.addEventListener("pointermove", moving);
        node.addEventListener("pointerup", finish);
        node.addEventListener("pointercancel", cancel);
      });
      fragment.appendChild(node);
    }
    stage.appendChild(fragment);
    if (cards.length === 0) {
      const empty = document.createElement("p");
      empty.textContent = "No cards yet. Add one here or ask an agent to place a card.";
      empty.style.padding = "52px";
      stage.appendChild(empty);
    }
    const panel = renderConstraints(constraints, byId, projection.constraint_state);
    mount.replaceChildren(stage, ...(panel ? [panel] : []));
    mount.dataset.scale = String(camera.scale);
    if (selectedId && !byId.has(selectedId)) select(null);
    // Frame everything once, after the other capabilities have drawn.
    if (!fitted && cards.length) setTimeout(() => { if (!fitted && fit()) fitted = true; }, 350);
    const hasDiagram = links.some(link => link.transition) || cards.some(card => (card.other_keys || []).some(key => ['diagram_kind','state_initial','state_terminal'].includes(key)));
    const diagramState = application.status?.().capabilities?.find(capability => capability.id === 'diagrams');
    if (hasDiagram && diagramState?.status !== 'ready') {
      const notice = document.createElement('p');
      notice.className = 'diagram-unavailable';
      notice.textContent = `Diagrams unavailable${diagramState ? ` (${diagramState.status})` : ': capability not selected'}. Stored diagram cards remain here; state roles and transitions cannot be rendered.`;
      notice.style.cssText = 'position:sticky;top:0;margin:0;padding:12px;background:#621f26;color:#fff;z-index:5';
      mount.prepend(notice);
    }
    count.textContent = `${cards.length} cards · ${links.length} links`;
    previousRevision = revision;
    globalThis.samepageMapMetrics = {
      revision, cards: cards.length, links: links.length,
      renderMs: performance.now() - began,
      projectionBytes: new TextEncoder().encode(JSON.stringify(projection)).byteLength,
    };
  }

  // Every field here (members, per-member resolved/unresolved, constraint
  // status) comes straight from the Map projection's own `constraints` array.
  // No arity, resolution or status rule is recomputed here.
  function renderConstraints(constraints, byId, state) {
    if (!constraints.length) return null;
    const panel = document.createElement("aside");
    panel.className = "map-constraints";
    const heading = document.createElement("h3");
    heading.textContent = `Constraints · ${constraints.length}`;
    panel.appendChild(heading);
    if (state?.status === "unsat") {
      const diagnostic = document.createElement("p");
      diagnostic.textContent = `UNSAT sequence cycle: ${state.core.join(", ")}`;
      diagnostic.dataset.status = "unsat";
      diagnostic.className = "map-constraint-cycle";
      panel.appendChild(diagnostic);
    }
    const list = document.createElement("ul");
    for (const constraint of constraints) {
      const item = document.createElement("li");
      item.className = "map-constraint";
      item.dataset.status = constraint.status;
      const head = document.createElement("div");
      head.className = "map-constraint-head";
      const legacyTag = constraint.membership === "legacy" ? " · legacy scalar" : "";
      const textTag = constraint.text ? ` "${constraint.text}"` : "";
      head.textContent = `${constraint.op}${textTag} · ${constraint.status}${legacyTag}`;
      item.appendChild(head);
      const members = document.createElement("ul");
      members.className = "map-constraint-members";
      for (const member of constraint.members) {
        const li = document.createElement("li");
        li.dataset.state = member.state;
        const target = byId.get(member.target);
        li.textContent = target ? (target.label || target.id) : member.target;
        members.appendChild(li);
      }
      item.appendChild(members);
      list.appendChild(item);
    }
    panel.appendChild(list);
    return panel;
  }

  const unregister = application.registerRenderer("map", { render });
  return { get revision() { return previousRevision; }, dispose() {
    disposed = true; unregister();
    mount.removeEventListener("wheel", onWheel); mount.removeEventListener("pointerdown", onPanStart, true);
    document.removeEventListener("keydown", onKey); document.removeEventListener("keyup", onKey); document.removeEventListener("samepage:map-focus", onFocus);
  } };
}
