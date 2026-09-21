// Application chrome: intent line, appearance sheet, transcript, composer.
//
// The shell owns no room state. It reads `room:state` to keep its controls in
// step and dispatches `room:command` when the person uses one, because the
// extension is what owns the room's actions, the loader would refuse this file
// if it tried to call them directly.

import { AgUiClient, loadExtensions, whoAmI, rememberMe } from "/_agui/client.js";
import "/_agui/provider-settings.js";

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
  };
  runtimeLabel.textContent = detail || labels[state] || state;
  connectionDot.classList.toggle("failed", state === "failed");
  ready = ["ready", "done", "thinking", "awaiting"].includes(state);
}

function showError(error) {
  const text = error instanceof Error ? error.message : String(error);
  composerStatus.textContent = text;
  message("agent", text, { author: "surface", error: true });
}

async function send(question) {
  if (!question) return;
  if (!ready) {
    composerStatus.textContent = "Choose or start an agent first.";
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
    await client.ask(question, clientId);
    composerStatus.textContent = "Shift + Enter for a new line";
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

// A pane's button is a question the agent wrote for the person to ask back.
document.addEventListener("room:ask", (event) => {
  const text = event.detail?.text?.trim();
  if (text) send(text);
});

const roomAlert = byId("room-alert");
let roomAlertTimer = null;

// Say it where it can actually be read. The two lines that used to be the
// whole handler write inside `.side-pane`, which `body.terminal-first` hides,
// and the body is always terminal-first. So every refusal the room produced,
// revision conflicts, over-length gestures, malformed views, went to a hidden
// element and the person saw their action simply not happen.
document.addEventListener("room:error", (event) => {
  const text = event.detail?.message || "the room rejected that";
  composerStatus.textContent = text;
  message("agent", text, { author: "room", error: true });
  if (!roomAlert) return;
  roomAlert.textContent = text;
  roomAlert.hidden = false;
  if (roomAlertTimer) clearTimeout(roomAlertTimer);
  // Long enough to read a sentence, then out of the way. It is a report, not
  // a dialog, and it must never sit on top of the room waiting to be dismissed.
  roomAlertTimer = setTimeout(() => {
    roomAlert.hidden = true;
  }, 9000);
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
    if (provider.ready) setRuntime("ready");
    else if (provider.error) setRuntime("failed", provider.error);
    else if (provider.warming) setRuntime("warming");
    await loadExtensions(client);
    client.connect();
  } catch (error) {
    setRuntime("failed", "surface failed to load");
    showError(error);
  }
}

start();
