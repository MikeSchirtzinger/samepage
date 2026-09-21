// Application chrome: intent line, appearance sheet, transcript, composer.
//
// The shell owns no room state. It reads `room:state` to keep its controls in
// step and dispatches `room:command` when the person uses one, because the
// extension is what owns the room's actions, the loader would refuse this file
// if it tried to call them directly.

import { AgUiClient, loadExtensions, whoAmI, rememberMe } from "/_agui/client.js";
import "/_agui/provider-settings.js";
import {
  relayedStatus,
  unansweredStatus,
  failureStatus,
  waitedFor,
  NO_AGENT_STATUS,
} from "/_agui/conversation.js";

const client = new AgUiClient();
// A handle for tests and for poking at the page from the console: the
// transport, and through it the typed protocol client's counters.
window.__agui = { client };
const byId = (id) => document.getElementById(id);
const transcript = byId("transcript");
const emptyState = byId("conversation-empty");
const runtimeLabel = byId("runtime-state");
const connectionDot = byId("connection-dot");
const composerStatus = byId("composer-status");
const sendButton = byId("send-button");
const input = byId("ask-input");
const intent = byId("intent-input");
const anchorBar = byId("anchor-bar");
const anchorLabel = byId("anchor-label");
const streaming = new Map();
const clientId = crypto.randomUUID();

let ready = false;
let room = null;
/// Agents attached over /mcp, as `{ id, label, listening, quiet }`. Seeded
/// from `/provider` and kept current by the `surface.agents` event.
let attachedAgents = [];
/// Chat nobody has collected, and how old the oldest of it is. Both come
/// from the host with `surface.agents`, which the liveness tick republishes
/// while nothing else is happening, so this stays current on its own.
let unanswered = 0;
let oldestUnansweredMs = null;

function message(role, text, options = {}) {
  if (!text && !options.allowEmpty) return null;
  if (emptyState?.isConnected) emptyState.remove();
  const wrapper = document.createElement("article");
  wrapper.className = `message ${role}${options.error ? " error" : ""}`;
  const meta = document.createElement("div");
  meta.className = "message-meta";
  meta.textContent = options.author || (role === "user" ? "you" : "agent");
  const body = document.createElement("div");
  body.className = "message-body";
  body.textContent = text || "";
  wrapper.append(meta, body);
  transcript.append(wrapper);
  transcript.scrollTop = transcript.scrollHeight;
  return body;
}

function beginStream(event) {
  if (!event.messageId || streaming.has(event.messageId)) return;
  const body = message(event.role === "user" ? "user" : "agent", "", { allowEmpty: true });
  if (body) streaming.set(event.messageId, { body, text: "" });
}

function appendStream(event) {
  if (!event.messageId || !event.delta) return;
  if (!streaming.has(event.messageId)) beginStream(event);
  const stream = streaming.get(event.messageId);
  if (!stream) return;
  stream.text += event.delta;
  stream.body.textContent = stream.text;
  transcript.scrollTop = transcript.scrollHeight;
}

function setRuntime(state, detail = "") {
  const labels = {
    connecting: "connecting",
    warming: "agent starting",
    ready: "ready",
    done: "ready",
    thinking: "agent thinking",
    awaiting: "waiting for you",
    failed: "agent failed",
    attached: "agent attached",
    unavailable: NO_AGENT_STATUS,
  };
  runtimeLabel.textContent = detail || labels[state] || state;
  connectionDot.classList.toggle("failed", state === "failed");
  // Either the in-page provider is live, or somebody is attached over /mcp
  // and the runtime relays to them: a provider that cannot take a turn is
  // not the same fact as nobody being here.
  ready = ["ready", "done", "thinking", "awaiting"].includes(state) || attachedAgents.length > 0;
}

function showError(error) {
  const text = error instanceof Error ? error.message : String(error);
  const status = failureStatus(text, attachedAgents);
  composerStatus.textContent = status;
  message("agent", status, { author: "surface", error: true });
}

/// The composer status line when nothing is actively in flight. A message
/// nobody has collected outranks the lifecycle word: it is the fact a person
/// watching this line is most likely waiting to find out.
function idleComposerStatus() {
  return unansweredStatus(attachedAgents, unanswered, oldestUnansweredMs)
    || "Shift + Enter for a new line";
}

/// Repaint the idle status. Skipped while a send is in flight (the button is
/// disabled), so an arriving surface.agents event cannot stomp on "Sending…".
function repaintComposerStatus() {
  if (sendButton.disabled) return;
  composerStatus.textContent = idleComposerStatus();
}

async function send(question) {
  if (!question) return;
  if (!ready) {
    composerStatus.textContent = NO_AGENT_STATUS;
    return;
  }
  sendButton.disabled = true;
  composerStatus.textContent = "Sending…";
  try {
    try {
      await client.interrupt();
    } catch {
      // An idle provider has nothing to interrupt.
    }
    message("user", question);
    const accepted = await client.ask(question, clientId);
    // The runtime says when it handed the message to attached agents rather
    // than to an in-page provider. Say so, instead of leaving a person to
    // wonder which of the two is thinking.
    composerStatus.textContent = accepted && accepted.relayed === "mcp"
      ? relayedStatus(attachedAgents)
      : idleComposerStatus();
  } catch (error) {
    showError(error);
  } finally {
    sendButton.disabled = false;
  }
}

function command(detail) {
  document.dispatchEvent(new CustomEvent("room:command", { detail }));
}

// ── event stream ──────────────────────────────────────────────────────────

client.on("connection:open", () => connectionDot.classList.add("live"));
client.on("connection:error", () => {
  connectionDot.classList.remove("live");
  setRuntime("connecting");
});
client.on("RUN_STARTED", () => connectionDot.classList.add("live"));
client.on("protocol:unavailable", ({ error }) => {
  setRuntime("failed", `Typed protocol client unavailable: ${error}`);
});
// Assembled by the typed protocol client; the shell only paints it.
client.on("transcript:text-started", beginStream);
client.on("transcript:text-delta", appendStream);
client.on("transcript:text-finished", (update) => {
  const stream = streaming.get(update.messageId);
  streaming.delete(update.messageId);
  if (stream) stream.body.textContent = update.text;
});
client.on("surface.tutor", (event) => {
  const value = event.value || {};
  setRuntime(value.state || "ready", value.state === "failed" ? value.question || "" : "");
});
client.on("surface.ask", (event) => {
  const value = event.value || {};
  if (value.origin === clientId) return;
  // Whose bubble this is, by participant id rather than by the word "you".
  // Every browser sends "you", so matching on it put the other person's
  // question on this screen as though this person had asked it.
  const mine = value.by_id ? value.by_id === participantId : value.by === "you";
  message(mine ? "user" : "agent", value.text || "", { author: value.by || "participant" });
});
client.on("surface.narrate", (event) => {
  const value = event.value || {};
  message("agent", value.text || "", { author: value.by || "agent" });
});
client.on("surface.agents", (event) => {
  const snapshot = event.value || {};
  attachedAgents = snapshot.attached || [];
  unanswered = snapshot.unanswered || 0;
  oldestUnansweredMs = snapshot.oldestUnansweredMs ?? null;
  if (snapshot.providerCanServe === false) {
    setRuntime(attachedAgents.length === 0 ? (snapshot.providerState || "unavailable") : "attached");
  }
  repaintComposerStatus();
  renderAgentChips();
});
client.on("surface.history", (event) => {
  streaming.clear();
  transcript.replaceChildren();
  for (const entry of (event.value || {}).entries || []) {
    message(entry.by === "you" ? "user" : "agent", entry.text || "");
  }
});

// ── from the room ─────────────────────────────────────────────────────────

document.addEventListener("room:state", (event) => {
  room = event.detail;
  if (document.activeElement !== intent) intent.value = room.intent || "";
  byId("theme-surface").value = room.theme?.surface || "ink";
  byId("theme-density").value = room.theme?.density || "cozy";
  byId("theme-accent").value = room.theme?.accent || "#6ea8ff";
  byId("theme-radius").value = String(room.theme?.radius ?? 12);
  byId("theme-scale").value = String(room.theme?.scale ?? 1);
  byId("radius-value").textContent = `${room.theme?.radius ?? 12}px`;
  byId("scale-value").textContent = `${Number(room.theme?.scale ?? 1).toFixed(2)}×`;
});

const roomAlert = byId("room-alert");
let roomAlertTimer = null;

// Say it where it can actually be read. `.side-pane`, which the composer
// status lives in, is hidden by `body.terminal-first`, and the body is always
// terminal-first. So every refusal the room produced, revision conflicts,
// over-length gestures, malformed views, went to a hidden element and the
// person saw their action simply not happen. `.room-alert` is visible in
// every mode for exactly this reason.
function showRoomAlert(text) {
  if (!roomAlert || !text) return;
  roomAlert.textContent = text;
  roomAlert.hidden = false;
  if (roomAlertTimer) clearTimeout(roomAlertTimer);
  // Long enough to read a sentence, then out of the way. It is a report, not
  // a dialog, and it must never sit on top of the room waiting to be dismissed.
  roomAlertTimer = setTimeout(() => {
    roomAlert.hidden = true;
  }, 9000);
}

/// What to tell the person BEFORE their pane-button ask goes anywhere, when
/// nobody is actually going to read it: `null` while at least one attached
/// agent is not quiet, so a reading agent gets no banner at all.
function noAgentReadingAlert() {
  if (attachedAgents.length === 0) return NO_AGENT_STATUS;
  if (attachedAgents.some((agent) => !agent.quiet)) return null;
  return relayedStatus(attachedAgents);
}

// A pane's button is a question the agent wrote for the person to ask back.
// Unlike the composer, this ask has no status line of its own next to it, so
// the one thing the person can lose track of, whether anyone is even reading
// it, has to be said up front rather than discovered from silence.
document.addEventListener("room:ask", (event) => {
  const text = event.detail?.text?.trim();
  if (!text) return;
  showRoomAlert(noAgentReadingAlert());
  send(text);
});

document.addEventListener("room:error", (event) => {
  const text = event.detail?.message || "the room rejected that";
  composerStatus.textContent = text;
  message("agent", text, { author: "room", error: true });
  showRoomAlert(text);
});

document.addEventListener("room:focus", (event) => {
  const detail = event.detail;
  anchorBar.hidden = !detail;
  anchorLabel.textContent = detail ? detail.label : "";
});

byId("clear-anchor").addEventListener("click", () => {
  anchorBar.hidden = true;
});

// ── header controls ───────────────────────────────────────────────────────

intent.addEventListener("keydown", (event) => {
  if (event.key !== "Enter") return;
  event.preventDefault();
  intent.blur();
  const next = intent.value.trim();
  if (next && next !== room?.intent) command({ type: "configure", args: { intent: next } });
});
intent.addEventListener("blur", () => {
  if (room && intent.value.trim() !== room.intent) intent.value = room.intent || "";
});

const appearanceSheet = byId("appearance-sheet");
byId("appearance-button").addEventListener("click", () => appearanceSheet.showModal());
byId("close-appearance").addEventListener("click", () => appearanceSheet.close());

function theme(patch) {
  byId("appearance-error").textContent = "";
  command({ type: "configure", args: { theme: patch } });
}

byId("theme-surface").addEventListener("change", (event) => theme({ surface: event.target.value }));
byId("theme-density").addEventListener("change", (event) => theme({ density: event.target.value }));
byId("theme-accent").addEventListener("change", (event) => theme({ accent: event.target.value }));
byId("theme-radius").addEventListener("change", (event) => theme({ radius: Number(event.target.value) }));
byId("theme-scale").addEventListener("change", (event) => theme({ scale: Number(event.target.value) }));
byId("theme-radius").addEventListener("input", (event) => {
  byId("radius-value").textContent = `${event.target.value}px`;
});
byId("theme-scale").addEventListener("input", (event) => {
  byId("scale-value").textContent = `${Number(event.target.value).toFixed(2)}×`;
});

const noteSheet = byId("note-sheet");
byId("add-note").addEventListener("click", () => {
  byId("note-error").textContent = "";
  noteSheet.showModal();
  byId("note-title-input").focus();
});
byId("close-note").addEventListener("click", () => noteSheet.close());
byId("note-submit").addEventListener("click", () => {
  const title = byId("note-title-input").value.trim();
  const text = byId("note-body-input").value.trim();
  if (!title || !text) {
    byId("note-error").textContent = "A pane needs a title and something in it.";
    return;
  }
  command({
    type: "note",
    id: `you-${Date.now().toString(36)}`,
    title,
    text,
  });
  byId("note-title-input").value = "";
  byId("note-body-input").value = "";
  noteSheet.close();
});

byId("ask-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const question = input.value.trim();
  if (!question || sendButton.disabled) return;
  input.value = "";
  await send(question);
});

input.addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    byId("ask-form").requestSubmit();
  }
});

document.querySelector("agui-provider-settings").addEventListener("provider-state", (event) => {
  const snapshot = event.detail || {};
  if (snapshot.ready) setRuntime("ready");
  else if (snapshot.error) setRuntime("failed", snapshot.error);
  else if (snapshot.warming) setRuntime("warming");
});

// ── presence ──────────────────────────────────────────────────────────────
// Who is on this page right now: this browser, and every agent attached over
// /mcp, by the label the host disambiguated. The strip only describes, the
// snapshot's own `authority` field says this state cannot authorize anything.
const presenceStrip = byId("presence-strip");

/// Who this browser is, minted by the host and resumed from its cookie.
///
/// This replaced an id the page minted for itself and kept in `localStorage`.
/// A client that picks its own id can pick one already in use, and every
/// browser picked the label "You", which is why the strip used to have to
/// guess that a second participant calling itself "You" was "another browser".
/// It is not a guess any more: the host names everybody, and no two people get
/// the same name.
///
/// Taken from the shared `whoAmI`, never fetched here. The shell and the room
/// extension both need it and both load at once, so two independent fetches
/// admitted this browser twice, which showed up as your own writing coming
/// back signed by somebody you had never met.
let me = await whoAmI();
let participantId = me?.id || null;

function renderPresence(snapshot) {
  if (!presenceStrip || !Array.isArray(snapshot?.participants)) return;
  presenceStrip.replaceChildren();
  for (const entry of snapshot.participants) {
    // An attached agent gets its own liveness-aware chip below, built from
    // attachedAgents (surface.agents/`/provider`), not from this presence
    // snapshot. Presence has its own 45s lease renewed by any `/mcp` call;
    // the registry this reuses keeps an entry, quiet or not, until
    // AGENT_DETACH_AFTER, and the chip has to survive that whole window.
    if (entry.participant?.kind === "agent") continue;
    const you = Boolean(participantId) && entry.participant?.id === participantId;
    const label = entry.participant?.label || "someone";
    if (you) {
      // Your own chip is the name control. Everyone else in the room reads
      // this, and a room where the other person is permanently "someone" can
      // record who did what but cannot tell you who they were.
      const field = document.createElement("input");
      field.className = `presence-chip presence-me${entry.active ? " active" : ""}`;
      field.value = me?.name || label;
      field.title = "What everyone else in this room sees you as";
      field.setAttribute("aria-label", "Your name in this room");
      field.maxLength = 40;
      field.size = Math.max(6, field.value.length);
      if (Number.isFinite(me?.hue)) field.style.setProperty("--by-hue", String(me.hue));
      field.addEventListener("keydown", (event) => {
        if (event.key === "Enter") field.blur();
        if (event.key === "Escape") {
          field.value = me?.name || label;
          field.blur();
        }
      });
      field.addEventListener("change", () => rename(field.value));
      presenceStrip.append(field);
      continue;
    }
    const chip = document.createElement("span");
    chip.className = `presence-chip${entry.active ? " active" : ""}`;
    chip.textContent = label;
    chip.title = `${label} · ${entry.status}`;
    presenceStrip.append(chip);
  }
  renderAgentChips();
}

/// The attached-agent half of the strip: one chip per agent in
/// `attachedAgents`, reading (normal) or quiet (dimmed, with how long), and
/// one muted line replacing nothing when no agent is attached at all. Called
/// after `renderPresence` rebuilds the human chips, and on its own whenever
/// `attachedAgents` changes (`surface.agents`), so it clears its own prior
/// output first rather than assuming a fresh strip.
function renderAgentChips() {
  if (!presenceStrip) return;
  for (const stale of presenceStrip.querySelectorAll(".presence-agent, .presence-note")) {
    stale.remove();
  }
  for (const agent of attachedAgents) {
    const label = agent.label || "agent";
    const quiet = Boolean(agent.quiet);
    const chip = document.createElement("span");
    chip.className = `presence-chip presence-agent${quiet ? " quiet" : ""}`;
    chip.append(document.createTextNode(label));
    if (quiet) {
      const age = waitedFor(agent.quietForMs ?? 0);
      const suffix = document.createElement("small");
      suffix.className = "presence-chip-suffix";
      suffix.textContent = `quiet ${age}`;
      chip.append(suffix);
      chip.title = `${label} · quiet ${age}`;
    } else {
      chip.title = `${label} · reading /mcp`;
    }
    presenceStrip.append(chip);
  }
  if (attachedAgents.length === 0) {
    const note = document.createElement("span");
    note.className = "presence-note";
    note.textContent = "No agent attached. Attach from a terminal at /mcp.";
    presenceStrip.append(note);
  }
}

/// Choose the name everyone else sees. The host disambiguates it, so the name
/// that comes back may not be the one that went out, take what it says rather
/// than what was asked for, or this page will show a name nobody else does.
async function rename(proposed) {
  const wanted = String(proposed || "").trim();
  if (!wanted || wanted === me?.name) return;
  try {
    const response = await fetch("/surface/me", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify({ name: wanted }),
    });
    if (!response.ok) return;
    me = await response.json();
    // Everyone else who asks who this browser is must get the new name too.
    rememberMe(me);
    await presenceRefresh();
  } catch {
    /* keep the old name rather than showing one the room did not accept */
  }
}

async function presenceCheckIn() {
  if (!participantId) return;
  try {
    const response = await fetch("/semantic-targets/presence", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ participantId, participantLabel: me?.name || "someone" }),
    });
    const body = await response.json();
    if (body?.snapshot) renderPresence(body.snapshot);
  } catch {
    /* the strip goes quiet rather than the room breaking */
  }
}

async function presenceRefresh() {
  try {
    const response = await fetch("/semantic-targets");
    const body = await response.json();
    if (body?.snapshot) renderPresence(body.snapshot);
  } catch {
    /* same: quiet, not broken */
  }
}

presenceCheckIn();
setInterval(presenceCheckIn, 15000);
setInterval(presenceRefresh, 5000);

async function start() {
  try {
    const provider = await client.getJson("/provider");
    // Seeded here so a page that loads after an agent attached does not
    // believe the room is empty until the next surface.agents tick.
    attachedAgents = provider.agents || [];
    const liveness = provider.agent_liveness || {};
    unanswered = liveness.unanswered || 0;
    oldestUnansweredMs = liveness.oldestUnansweredMs ?? null;
    if (provider.provider_can_serve === false) {
      setRuntime(attachedAgents.length === 0 ? "unavailable" : "attached");
    } else if (provider.ready) setRuntime("ready");
    else if (provider.error) setRuntime("failed", provider.error);
    else if (provider.warming) setRuntime("warming");
    repaintComposerStatus();
    renderAgentChips();
    await loadExtensions(client);
    client.connect();
  } catch (error) {
    setRuntime("failed", "surface failed to load");
    showError(error);
  }
}

start();
