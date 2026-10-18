// Minimal Map entry: projection and plans come from the verified Map WASM.
// The shell owns the single document, authenticated socket and lifecycle.
export async function activate({ mount, wasm, application }) {
  if (!application || !wasm) throw new Error("Map needs the verified capability and shared document context");
  const { speechIcon } = await import(new URL("/vocabulary.mjs", location.origin).href);
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
  // The latest projection, so a key press can name the card it acts on.
  let latestCards = new Map(), latestRecords = new Map();
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
    if (id !== null || selectedLink) { selectedLink = null; mount.querySelectorAll(".map-link-end").forEach((node) => node.classList.remove("map-link-end")); mount.querySelectorAll(".map-link-selected").forEach((node) => node.classList.remove("map-link-selected")); }
    selectedId = id;
    mount.querySelectorAll(".map-card").forEach(node => node.classList.toggle("map-selected", node.dataset.id === id));
    const card = id ? [...mount.querySelectorAll(".map-card")].find(node => node.dataset.id === id) : null;
    const label = card?.querySelector("h2")?.textContent || "";
    const detail = id ? {id, label, count: 1, summary: label, kind: "node"} : null;
    window.samepageSelection = detail;
    document.dispatchEvent(new CustomEvent("map-selection-changed", {detail}));
  }
  const who = (name, participant) => participant && participant === globalThis.samepagePerson?.id ? "you" : (String(participant || "").startsWith("agent-") ? String(name || "agent").replace(/-\d+$/, "") : (name || "someone"));
  let routed = [], selectedLink = null;
  // Clip the centre-to-centre segment to each card's border.
  function edgePoint(box, toward) {
    const cx = box.x + box.w / 2, cy = box.y + box.h / 2;
    const dx = toward.x - cx, dy = toward.y - cy;
    if (!dx && !dy) return {x: cx, y: cy};
    const k = Math.min(dx ? (box.w / 2) / Math.abs(dx) : Infinity, dy ? (box.h / 2) / Math.abs(dy) : Infinity);
    return {x: cx + dx * k, y: cy + dy * k};
  }
  function routeLinks(moving) {
    const stage = mount.querySelector(".map-stage");
    if (!stage) return;
    const nodes = new Map([...stage.querySelectorAll(".map-card")].map((node) => [node.dataset.id, node]));
    const boxOf = (id) => {
      const node = nodes.get(id);
      if (!node) return null;
      const shift = moving?.id === id ? moving : {dx: 0, dy: 0};
      return {x: node.offsetLeft + shift.dx, y: node.offsetTop + shift.dy, w: node.offsetWidth, h: node.offsetHeight};
    };
    for (const {link, line, hit, label} of routed) {
      const a = boxOf(link.from), b = boxOf(link.to);
      if (!a || !b) continue;
      const start = edgePoint(a, {x: b.x + b.w / 2, y: b.y + b.h / 2});
      const end = edgePoint(b, {x: a.x + a.w / 2, y: a.y + a.h / 2});
      for (const element of [line, hit]) {
        element.setAttribute("x1", String(start.x)); element.setAttribute("y1", String(start.y));
        element.setAttribute("x2", String(end.x)); element.setAttribute("y2", String(end.y));
      }
      if (label) { label.setAttribute("x", String((start.x + end.x) / 2)); label.setAttribute("y", String((start.y + end.y) / 2 - 7)); }
      line.classList.toggle("map-link-selected", selectedLink === link.id);
    }
  }
  const resizeWatch = typeof ResizeObserver === "function" ? new ResizeObserver(() => requestAnimationFrame(() => routeLinks())) : null;
  // Clicking a link shows its two ends and says which way it points.
  function selectLink(link) {
    select(null);
    selectedLink = link?.id || null;
    mount.querySelectorAll(".map-card").forEach((node) => node.classList.toggle("map-link-end", Boolean(link) && (node.dataset.id === link.from || node.dataset.id === link.to)));
    routeLinks();
    if (link) {
      const name = (id) => [...mount.querySelectorAll(".map-card")].find((node) => node.dataset.id === id)?.querySelector("h2")?.textContent || id;
      application.toast?.(`${name(link.from)} ${link.label || "links to"} ${name(link.to)}.`);
    }
  }
  function emptyState() {
    const box = document.createElement("section");
    box.className = "map-empty"; box.setAttribute("aria-label", "Getting started");
    const line = (tag, text) => { const node = document.createElement(tag); node.textContent = text; return node; };
    const copyRow = (text, label) => {
      const row = document.createElement("div"); row.className = "map-empty-copy";
      const code = line("code", text);
      const copy = document.createElement("button"); copy.type = "button"; copy.textContent = "Copy"; copy.setAttribute("aria-label", `Copy ${label}`);
      copy.addEventListener("click", async () => {
        try { await navigator.clipboard.writeText(text); copy.textContent = "Copied"; application.toast?.(`Copied ${label}.`); }
        catch { const range = document.createRange(); range.selectNodeContents(code); getSelection().removeAllRanges(); getSelection().addRange(range); application.toast?.(`Select and copy the ${label}.`); }
      });
      row.append(code, copy); return row;
    };
    const address = `${location.origin}/mcp`;
    const prompt = "Read this project and place a card for each part that handles an order, linked in the order a request flows. Bind each card to its source file.";
    const add = document.createElement("button"); add.type = "button"; add.textContent = "Add a card yourself";
    add.addEventListener("click", () => document.getElementById("add-card-open")?.click());
    box.append(
      line("h2", "A shared map for you and your coding agent"),
      line("p", "Cards, links and drawings here are shared live with every person and agent on this page. Nothing is on it yet."),
      line("h3", "1. Connect your agent to this page over MCP"), copyRow(address, "MCP address"),
      line("p", "If the host was started with an MCP token, give the agent that token as its bearer token."),
      line("h3", "2. Ask it to map something"), copyRow(prompt, "starter prompt"),
      line("h3", "3. Or start yourself"), add);
    return box;
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
    else if ((event.key === "Delete" || event.key === "Backspace") && selectedId && latestCards.has(selectedId)) {
      // The key that removes a drawing removes a selected card too. A person
      // is not asked to confirm: every edit is attributed, and the toast names
      // whose card it was. Agents are the ones that must ask first.
      const id = selectedId, card = latestCards.get(id), record = latestRecords.get(id);
      event.preventDefault();
      const by = who(card.created_by, record?.props?.created_by_participant);
      void (async () => {
        try {
          const planned = await application.plan("map", "object_remove", {id});
          await application.applyBatch(planned);
          select(null);
          application.resolved?.("map", "object_remove");
          application.toast?.(`Removed “${card.label || "card"}”, made by ${by}.`);
        } catch (error) {
          application.problem?.("map", "object_remove", `Card removal refused: ${error.message}`);
        }
      })();
    }
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
      // The byline is an overlay shown on hover or selection; it takes no room.
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
    const recordById = new Map(view.records.filter((record) => record.kind === "node").map((record) => [record.id, record]));
    latestCards = byId; latestRecords = recordById;
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
    // Links are routed from the rendered card boxes, border to border, with an
    // arrowhead at the target. They follow a card while it is dragged.
    const defs = document.createElementNS(svg.namespaceURI, "defs");
    const marker = document.createElementNS(svg.namespaceURI, "marker");
    for (const [key, value] of Object.entries({id: "map-arrow", viewBox: "0 0 10 10", refX: "9", refY: "5", markerWidth: "8", markerHeight: "8", orient: "auto-start-reverse", markerUnits: "userSpaceOnUse"})) marker.setAttribute(key, value);
    const head = document.createElementNS(svg.namespaceURI, "path");
    head.setAttribute("d", "M 0 0 L 10 5 L 0 10 z"); head.setAttribute("class", "map-link-head");
    marker.append(head); defs.append(marker); svg.append(defs);
    routed = [];
    for (const link of links) {
      if (link.transition) continue;
      if (!byId.get(link.from) || !byId.get(link.to)) continue;
      const line = document.createElementNS(svg.namespaceURI, "line");
      line.classList.add("map-link");
      line.dataset.id = link.id;
      line.setAttribute("marker-end", "url(#map-arrow)");
      const hit = document.createElementNS(svg.namespaceURI, "line");
      hit.classList.add("map-link-hit");
      hit.dataset.id = link.id;
      hit.addEventListener("click", (event) => { event.stopPropagation(); selectLink(link); });
      svg.append(line, hit);
      let label = null;
      if (link.label) {
        label = document.createElementNS(svg.namespaceURI, "text");
        label.classList.add("map-link-label");
        label.textContent = link.label;
        svg.appendChild(label);
      }
      routed.push({link, line, hit, label});
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
      const record = recordById.get(card.id)?.props || {};
      const byline = document.createElement("div");
      byline.className = "byline";
      byline.textContent = `Created by ${who(card.created_by, record.created_by_participant)} · last by ${who(card.touched_by, record.touched_by_participant)}`;
      node.appendChild(byline);
      if ((card.other_keys || []).includes("state_order")) node.dataset.stateCard = "true";
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
        question.append(speechIcon());
        question.title = "Discuss this card in Review";
        question.setAttribute("aria-label", `Discuss ${card.label || "card"} in Review`);
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
          const dx = (move.clientX - startX) / scale, dy = (move.clientY - startY) / scale;
          node.style.transform = `translate(${dx}px, ${dy}px)`;
          routeLinks({id: card.id, dx, dy});
        };
        const finish = async (up) => {
          node.removeEventListener("pointermove", moving);
          node.removeEventListener("pointerup", finish);
          node.removeEventListener("pointercancel", cancel);
          node.style.transform = "";
          routeLinks();
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
          routeLinks();
        };
        node.addEventListener("pointermove", moving);
        node.addEventListener("pointerup", finish);
        node.addEventListener("pointercancel", cancel);
      });
      fragment.appendChild(node);
    }
    stage.appendChild(fragment);
    // First run: say what this is and how to bring an agent in. Shown only
    // when the page holds nothing at all, so it never sits on a drawing.
    const empty = cards.length === 0 && !view.records.some((record) => record.kind === "shape");
    if (empty) mount.dataset.empty = "true"; else delete mount.dataset.empty;
    const panel = renderConstraints(constraints, byId, projection.constraint_state);
    mount.replaceChildren(stage, ...(panel ? [panel] : []), ...(empty ? [emptyState()] : []));
    mount.dataset.scale = String(camera.scale);
    routeLinks();
    resizeWatch?.disconnect();
    stage.querySelectorAll(".map-card").forEach((card) => resizeWatch?.observe(card));
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
