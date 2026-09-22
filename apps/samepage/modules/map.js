// Minimal Map entry: projection and plans come from the verified Map WASM.
// The shell owns the single document, authenticated socket and lifecycle.
export function activate({ mount, wasm, application }) {
  if (!application || !wasm) throw new Error("Map needs the verified capability and shared document context");
  application.registerCapability("map", wasm);
  const count = document.getElementById("map-count");
  let disposed = false;
  let previousRevision = "";

  async function render({view, revision}) {
    if (disposed) return;
    const began = performance.now();
    const projection = await application.project("map", view);
    const cards = projection.cards || [];
    const links = projection.links || [];
    const byId = new Map(cards.map((card) => [card.id, card]));
    const originX = 42 - Math.min(0, ...cards.map((card) => Number(card.x) || 0));
    const originY = 42 - Math.min(0, ...cards.map((card) => Number(card.y) || 0));
    const width = Math.max(mount.clientWidth, ...cards.map((card) => originX + Number(card.x || 0) + Number(card.width || 232) + 42));
    const height = Math.max(mount.clientHeight, ...cards.map((card) => originY + Number(card.y || 0) + Number(card.height || 100) + 42));
    const stage = document.createElement("div");
    stage.className = "map-stage";
    stage.style.width = `${width}px`;
    stage.style.height = `${height}px`;
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.classList.add("map-links");
    svg.setAttribute("width", String(width));
    svg.setAttribute("height", String(height));
    svg.setAttribute("aria-hidden", "true");
    for (const link of links) {
      const from = byId.get(link.from);
      const to = byId.get(link.to);
      if (!from || !to) continue;
      const x1 = originX + Number(from.x || 0) + Number(from.width || 232) / 2;
      const y1 = originY + Number(from.y || 0) + Number(from.height || 100) / 2;
      const x2 = originX + Number(to.x || 0) + Number(to.width || 232) / 2;
      const y2 = originY + Number(to.y || 0) + Number(to.height || 100) / 2;
      const line = document.createElementNS(svg.namespaceURI, "line");
      line.classList.add("map-link");
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
      node.dataset.status = card.status || "open";
      node.style.left = `${originX + Number(card.x || 0)}px`;
      node.style.top = `${originY + Number(card.y || 0)}px`;
      node.style.setProperty("--card-width", `${Number(card.width || 232)}px`);
      node.style.setProperty("--card-height", `${Number(card.height || 100)}px`);
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
      node.addEventListener("pointerdown", (event) => {
        if (event.button !== 0 || !application.ready()) return;
        event.preventDefault();
        const startX = event.clientX;
        const startY = event.clientY;
        node.setPointerCapture(event.pointerId);
        const moving = (move) => {
          node.style.transform = `translate(${move.clientX - startX}px, ${move.clientY - startY}px)`;
        };
        const finish = async (up) => {
          node.removeEventListener("pointermove", moving);
          node.removeEventListener("pointerup", finish);
          node.removeEventListener("pointercancel", cancel);
          node.style.transform = "";
          const dx = up.clientX - startX;
          const dy = up.clientY - startY;
          if (Math.abs(dx) + Math.abs(dy) < 3) return;
          try {
            const planned = await application.plan("map", "card_move", {
              id: card.id, x: Number(card.x || 0) + dx, y: Number(card.y || 0) + dy,
            });
            application.apply("map", planned);
          } catch (error) {
            const alert = document.getElementById("error-banner");
            alert.textContent = `Card move refused: ${error.message}`;
            alert.hidden = false;
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
    mount.replaceChildren(stage);
    count.textContent = `${cards.length} cards · ${links.length} links`;
    previousRevision = revision;
    globalThis.samepageMapMetrics = {
      revision, cards: cards.length, links: links.length,
      renderMs: performance.now() - began,
      projectionBytes: new TextEncoder().encode(JSON.stringify(projection)).byteLength,
    };
  }

  const unregister = application.registerRenderer("map", { render });
  return { get revision() { return previousRevision; }, dispose() { disposed = true; unregister(); } };
}
