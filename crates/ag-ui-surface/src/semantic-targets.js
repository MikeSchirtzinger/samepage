// Host-owned browser projection for namespaced semantic targets.
//
// Extensions register stable semantic ids with actual Element handles. Agent
// messages contain only those ids; this host derives geometry locally. There
// are no global DOM lookups and no application-specific custom events.

const registrations = new Map();
const namespaceListeners = new Map();
let attached = null;
let latest = null;
let overlay = null;
let overlayLabel = null;
let presenceTray = null;
let revealSignature = "";
let activeTimer = 0;

function validId(value) {
  return typeof value === "string"
    && value.length > 0
    && value.length <= 160
    && /^[A-Za-z0-9._:-]+$/.test(value);
}

function keyOf(target) {
  return `${target.extensionId}:${target.targetId}`;
}

function ensureChrome() {
  if (overlay) return;

  const stylesheet = document.createElement("link");
  stylesheet.rel = "stylesheet";
  stylesheet.href = "/_agui/semantic-targets.css";
  document.head.append(stylesheet);

  overlay = document.createElement("div");
  overlay.className = "agui-semantic-overlay";
  overlay.hidden = true;
  overlay.setAttribute("aria-hidden", "true");
  overlayLabel = document.createElement("span");
  overlayLabel.className = "agui-semantic-overlay-label";
  overlay.append(overlayLabel);

  presenceTray = document.createElement("div");
  presenceTray.className = "agui-semantic-presence";
  presenceTray.setAttribute("role", "status");
  presenceTray.setAttribute("aria-live", "polite");
  document.body.append(overlay, presenceTray);
}

function listenersFor(namespace) {
  if (!namespaceListeners.has(namespace)) namespaceListeners.set(namespace, new Set());
  return namespaceListeners.get(namespace);
}

/// A gesture may cover a set. The echo carries the whole set, in order.
///
/// Handing a listener only the first member would make every marquee collapse
/// back to one selection the moment the host echoed it, which is a shrinking
/// selection nobody asked for. `targetIds` is the ordered set; `targetId` and
/// the spread `extensionId`/`targetId` pair stay exactly what they were, so a
/// listener that only understands one target still reads the first member.
function selectionFromSnapshot(snapshot) {
  if (!attached) return null;
  const mine = (snapshot.participants || []).find((entry) =>
    entry.participant?.kind === "human"
      && entry.participant?.id === attached.participant.id
      && entry.active
  );
  const attention = mine?.attention;
  const primary = attention?.target?.target || null;
  if (!primary) return null;
  const all = [attention.target, ...(attention.additionalTargets || [])];
  return {
    ...primary,
    targetIds: all
      .filter((entry) => entry?.target?.extensionId === primary.extensionId)
      .map((entry) => entry.target.targetId),
    labels: all
      .filter((entry) => entry?.target?.extensionId === primary.extensionId)
      .map((entry) => entry.label),
    count: all.length,
  };
}

function notifySelections(snapshot) {
  if (!attached) return;
  const selected = selectionFromSnapshot(snapshot);
  for (const [namespace, listeners] of namespaceListeners) {
    const value = selected?.extensionId === namespace ? selected : null;
    for (const listener of listeners) listener(value);
  }
}

/// What to call a participant on the strip.
///
/// Every browser attaches with the label "You", which reads correctly on your
/// own screen and is a lie on everyone else's, two windows both announced
/// themselves as "You" and the strip showed one person twice with no way to
/// tell which chip was yours. "You" is therefore decided *here*, from whether
/// the id matches the participant this browser attached as, and a second
/// human keeps a neutral name: the host knows there are two clients, and
/// genuinely does not know whether they are two people.
function presenceLabel(entry) {
  const participant = entry.participant || {};
  const mine = attached && participant.id === attached.participant.id;
  if (mine) return { text: "You", mine: true };
  if (participant.kind === "human" && (!participant.label || participant.label === "You")) {
    return { text: "Another view", mine: false };
  }
  return { text: participant.label || "Participant", mine: false };
}

function renderPresence(snapshot) {
  presenceTray.replaceChildren();
  for (const entry of snapshot.participants || []) {
    const { text: name, mine } = presenceLabel(entry);
    const chip = document.createElement("span");
    chip.className = `agui-semantic-participant ${entry.active ? "active" : "present"}${mine ? " me" : ""}`;
    const dot = document.createElement("span");
    dot.className = "agui-semantic-participant-dot";
    dot.setAttribute("aria-hidden", "true");
    const text = document.createElement("span");
    text.textContent = `${name} · ${entry.status}`;
    chip.append(dot, text);
    presenceTray.append(chip);
  }
  presenceTray.hidden = presenceTray.childElementCount === 0;
}

/// The box a target is actually visible inside: every clipping ancestor
/// intersected together.
///
/// Without this the ring is drawn at `getBoundingClientRect()` and nothing
/// stops it from landing on a *neighbouring* column, because the overlay is
/// `position: fixed` and the ancestor that clipped the target does not clip
/// it. An agent pointing at an off-screen card then highlights whatever
/// happens to sit at those coordinates, a confident ring around the wrong
/// artifact, which is worse than no ring at all.
function clipBoundsFor(element) {
  let bounds = null;
  for (let node = element.parentElement; node && node !== document.body; node = node.parentElement) {
    const style = getComputedStyle(node);
    const clips = style.overflow !== "visible"
      || style.overflowX !== "visible"
      || style.overflowY !== "visible";
    if (!clips) continue;
    const box = node.getBoundingClientRect();
    bounds = bounds ? intersectRects(bounds, box) : box;
    if (!bounds) return null;
  }
  return bounds;
}

function intersectRects(a, b) {
  const left = Math.max(a.left, b.left);
  const top = Math.max(a.top, b.top);
  const right = Math.min(a.right, b.right);
  const bottom = Math.min(a.bottom, b.bottom);
  if (right <= left || bottom <= top) return null;
  return { left, top, right, bottom, width: right - left, height: bottom - top };
}

function renderAgentAttention(snapshot) {
  const active = (snapshot.participants || [])
    .filter((entry) => entry.participant?.kind === "agent" && entry.active && entry.attention)
    .at(0);
  if (!active) {
    overlay.hidden = true;
    return;
  }
  const target = active.attention.target?.target;
  const registration = target ? registrations.get(keyOf(target)) : null;
  if (!registration?.element?.isConnected) {
    overlay.hidden = true;
    return;
  }
  if (registration.project?.()?.kind === "nowhere") {
    // The surface says there is no honest place for this: not a rectangle to
    // ring and not an edge to point from. Drawing anything here would be a
    // confident mark on the wrong thing.
    overlay.hidden = true;
    return;
  }

  // `reveal` asks to bring the target into view. `scrollIntoView` is only the
  // right answer for a pane that scrolls; a surface moved by a CSS-transform
  // camera (the atlas) has to move its own camera, so a registration may hand
  // us the way in. Without this hook, reveal is a silent no-op there.
  if (active.attention.mode === "reveal") {
    const signature = `${keyOf(target)}:${active.attention.message || ""}`;
    if (signature !== revealSignature) {
      revealSignature = signature;
      // Who is asking decides what a surface is allowed to do about it. A
      // surface may hold the camera against an agent and still honour the
      // same call when the human clicks the offer below, so the initiator
      // travels with the request rather than being guessed at.
      const moved = registration.reveal?.({ initiator: "agent" });
      if (!moved) {
        registration.element.scrollIntoView({ behavior: "smooth", block: "center", inline: "center" });
      } else {
        // A camera-backed surface may animate the move. Recompute the ring
        // after that animation so the highlight lands on the revealed target,
        // not where it was when the movement began.
        setTimeout(() => {
          if (latest === snapshot) renderAgentAttention(snapshot);
        }, 320);
      }
    }
  }

  // A surface that owns a camera answers where its own target is. The DOM
  // read below is right for a pane that scrolls and wrong for one moved by a
  // CSS transform: `getBoundingClientRect` is correct at the instant it is
  // called and nothing recomputes it when the camera moves, which is why the
  // ring used to keep the pixels it was handed while the board slid away.
  // `project` is that surface's own answer, recomputed from the camera every
  // time this runs.
  const projected = registration.project?.() ?? null;
  const rect = projected?.kind === "visible"
    ? { left: projected.left, top: projected.top, width: projected.width, height: projected.height,
      right: projected.left + projected.width, bottom: projected.top + projected.height }
    : registration.element.getBoundingClientRect();
  const bounds = projected ? null : clipBoundsFor(registration.element);
  const visible = projected
    ? (projected.kind === "visible" ? rect : null)
    : (bounds ? intersectRects(rect, bounds) : rect);

  overlayLabel.textContent = active.attention.message
    || `Agent: ${active.attention.anchor?.label || registration.label}`;

  if (visible) {
    // Ring what can actually be seen. A partly-scrolled card gets a ring on
    // the part that is on screen rather than one that spills over its pane.
    overlay.classList.remove("offscreen");
    overlay.style.left = `${visible.left}px`;
    overlay.style.top = `${visible.top}px`;
    overlay.style.width = `${visible.width}px`;
    overlay.style.height = `${visible.height}px`;
  } else {
    // Nothing to ring. Say so at the edge of the pane the target lives in,
    // pointing the way, instead of drawing a ring somewhere it would lie.
    //
    // Pointing and guiding are different promises. A point preserves the
    // current camera and offers a clickable edge label. A deliberate reveal
    // uses the registered camera hook above and may pan or zoom as part of a
    // walkthrough.
    // Both halves of this come from the surface when it owns a camera: the
    // point on the pane edge and the direction to point. The DOM branch below
    // it is the fallback for a surface that scrolls.
    const edgeLeft = projected ? projected.x
      : clamp(rect.left + rect.width / 2, bounds.left + 12, bounds.right - 12);
    const edgeTop = projected ? projected.y
      : clamp(rect.top + rect.height / 2, bounds.top + 12, bounds.bottom - 12);
    const arrow = projected ? ARROWS[projected.direction]
      : rect.right < bounds.left ? "←"
        : rect.left > bounds.right ? "→"
          : rect.bottom < bounds.top ? "↑" : "↓";
    overlay.classList.add("offscreen");
    overlay.style.left = `${edgeLeft}px`;
    overlay.style.top = `${edgeTop}px`;
    overlay.style.width = "0px";
    overlay.style.height = "0px";
    // The label opens back into the pane it is pinned to. Anchored at the
    // right edge it has to grow leftwards, or it lies across the *next*
    // column, the same mistake as the ring, made by the caption instead.
    overlay.classList.toggle("from-right", arrow === "→");
    overlay.classList.toggle("from-left", arrow === "←");
    const canGo = typeof registration.reveal === "function";
    overlayLabel.textContent =
      `${arrow} ${overlayLabel.textContent}${canGo ? ", off screen, click to go there" : " (off screen)"}`;
    overlayLabel.classList.toggle("actionable", canGo);
    overlayLabel.onclick = canGo
      ? () => {
        registration.reveal({ initiator: "human" });
        overlay.hidden = true;
      }
      : null;
  }
  if (!overlay.classList.contains("offscreen")) {
    overlayLabel.classList.remove("actionable");
    overlayLabel.onclick = null;
  }
  // The label hangs above the ring; near the top of the window that puts it
  // out of the viewport entirely, so it flips below instead.
  overlay.classList.toggle("label-below", (visible ? visible.top : rect.top) < 74);
  overlay.hidden = false;

  clearTimeout(activeTimer);
  activeTimer = setTimeout(() => {
    overlay.hidden = true;
  }, Math.max(1, active.activeForMs || 1));
}

const clamp = (value, low, high) => Math.min(Math.max(value, low), Math.max(low, high));
const ARROWS = { left: "←", right: "→", up: "↑", down: "↓" };

function acceptSnapshot(snapshot) {
  if (!snapshot || snapshot.schemaVersion !== 1) return;
  latest = snapshot;
  ensureChrome();
  renderPresence(snapshot);
  notifySelections(snapshot);
  renderAgentAttention(snapshot);

  if (!attached) return;
  const mine = (snapshot.participants || []).find((entry) =>
    entry.participant?.kind === "human"
      && entry.participant?.id === attached.participant.id
      && entry.active
      && entry.attention
  );
  if (mine) attached.composer?.setAnchor(mine.attention.anchor);
  else attached.composer?.clearAnchor();
}

async function post(path, body) {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const value = await response.json().catch(() => ({}));
  if (!response.ok || value.ok === false) {
    throw new Error(value.error || `${path} failed (${response.status})`);
  }
  if (value.snapshot) acceptSnapshot(value.snapshot);
  return value;
}

function humanBody(extra = {}) {
  if (!attached) throw new Error("semantic target host is not attached");
  return {
    participantId: attached.participant.id,
    participantLabel: attached.participant.label,
    ...extra,
  };
}

/// Attach one browser participant and the shared composer to the host service.
export async function attachSemanticTargetHost({ client, composer, participant }) {
  if (!client || !composer) throw new Error("semantic target host needs client and composer");
  if (!validId(participant?.id) || !participant?.label) {
    throw new Error("semantic target host needs a stable participant id and label");
  }
  attached = { client, composer, participant };
  ensureChrome();
  client.on("surface.semantic-targets", (event) => {
    acceptSnapshot(event.value || event.detail || {});
  });
  composer.addEventListener("anchor-clear", () => {
    post("/semantic-targets/clear", humanBody()).catch((error) => {
      console.error("could not clear semantic focus", error);
    });
  });
  await post("/semantic-targets/presence", humanBody());
  setInterval(() => {
    post("/semantic-targets/presence", humanBody()).catch(() => {});
  }, 20_000);
  return {
    get snapshot() {
      return latest;
    },
  };
}

/// Typed, namespace-bound API handed to one extension.
export function semanticTargetNamespace(extensionId) {
  if (!validId(extensionId)) throw new Error("invalid semantic target extension id");
  const owned = new Set();

  return Object.freeze({
    /// `reveal` is optional and only matters for surfaces that do not scroll:
    /// return truthy from it once you have brought the target into view, and
    /// the default `scrollIntoView` is skipped.
    /// `project` is optional and only matters for a surface whose content is
    /// moved by a camera rather than by scrolling. Return the client-space
    /// placement for this target, recomputed from that camera: either
    /// `{kind: "visible", left, top, width, height}`, `{kind: "edge", x, y,
    /// direction}`, or `{kind: "nowhere"}`. Supplying it means the ring is
    /// recomputed on every camera write instead of being frozen at the pixels
    /// it happened to be handed.
    register({ id, label, element, spatial = false, reveal, project }) {
      if (!validId(id)) throw new Error(`invalid semantic target id for ${extensionId}`);
      if (!label || !(element instanceof Element)) {
        throw new Error(`semantic target ${extensionId}:${id} needs label and Element`);
      }
      if (reveal !== undefined && typeof reveal !== "function") {
        throw new Error(`semantic target ${extensionId}:${id} reveal must be a function`);
      }
      if (project !== undefined && typeof project !== "function") {
        throw new Error(`semantic target ${extensionId}:${id} project must be a function`);
      }
      const target = { extensionId, targetId: id };
      const key = keyOf(target);
      registrations.set(key, { target, label, element, spatial: Boolean(spatial), reveal, project });
      owned.add(key);
      if (latest) renderAgentAttention(latest);
      return Object.freeze({ ...target });
    },

    /// Redraw every anchored overlay from the surface's current camera.
    ///
    /// This is the recompute path the ring never had. Call it from wherever
    /// the surface writes its camera and from its own size observer; do NOT
    /// call it from a frame loop. A `requestAnimationFrame` poll paints
    /// nothing in a background or headless tab, which is exactly where an
    /// agent checks its own work.
    reproject() {
      if (latest) renderAgentAttention(latest);
    },

    unregister(id) {
      const key = keyOf({ extensionId, targetId: id });
      registrations.delete(key);
      owned.delete(key);
    },

    reset() {
      for (const key of owned) registrations.delete(key);
      owned.clear();
    },

    async select(targetId, message = "") {
      return this.selectMany([targetId], message);
    },

    /// Select a whole set as ONE gesture, in the order the human built it.
    ///
    /// One request, not one per member: the host takes a single sequence
    /// number for it, so an agent parked on a wait is woken once and reads a
    /// set, instead of being woken N times and reading N selections of one.
    async selectMany(targetIds, message = "") {
      const ids = Array.from(new Set(Array.isArray(targetIds) ? targetIds : [targetIds]));
      if (ids.length === 0) throw new Error(`semantic selection for ${extensionId} is empty`);
      for (const id of ids) {
        if (!validId(id)) throw new Error(`invalid semantic target id for ${extensionId}`);
      }
      return post("/semantic-targets/focus", humanBody({
        mode: "selection",
        target: { extensionId, targetId: ids[0] },
        targets: ids.map((targetId) => ({ extensionId, targetId })),
        message,
      }));
    },

    async focus(targetId, message = "") {
      if (!validId(targetId)) throw new Error(`invalid semantic target id for ${extensionId}`);
      return post("/semantic-targets/focus", humanBody({
        mode: "focus",
        target: { extensionId, targetId },
        message,
      }));
    },

    async clear() {
      return post("/semantic-targets/clear", humanBody());
    },

    reveal(targetId) {
      const registration = registrations.get(keyOf({ extensionId, targetId }));
      if (!registration?.element?.isConnected) return false;
      registration.element.scrollIntoView({ behavior: "smooth", block: "center", inline: "center" });
      return true;
    },

    onSelection(listener) {
      const listeners = listenersFor(extensionId);
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  });
}
