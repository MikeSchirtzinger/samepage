// `<agui-conversation>`, the conversation chrome, owned by the runtime.
//
// Every app built on this crate had rewritten the same panel: a transcript, a
// streaming assistant message, the `surface.ask`/`narrate`/`history` broadcasts,
// a composer with barge-in, and a connection light. Five copies of one protocol
// client, drifting apart, the template's copy had already fallen behind the
// `<agui-provider-settings>` element this crate ships.
//
// So it moves here, next to that element. An app writes one tag and gets the
// conversation; what it keeps is the part that is actually its own, what the
// surface *is*, and what "this" points at.
//
//     <agui-conversation hint="Try: “map this repository”"></agui-conversation>
//
//     const conversation = document.querySelector("agui-conversation");
//     conversation.client = client;                    // optional; see below
//     conversation.setAnchor({ label: "the parser node" });
//     conversation.addEventListener("anchor-clear", () => …);
//     conversation.addEventListener("runtime-state", (e) => e.detail.state);
//
// Set `.client` before the app calls `connect()` to share one transport with
// the rest of the page (extensions, custom events). Leave it unset and the
// element builds its own client and connects itself, which is the whole app for
// something that is only a chat.
//
// Theming follows `<agui-provider-settings>`: `:host` declares `--ac-*` custom
// properties an app overrides from outside the shadow root, and the pieces a
// page may want to reach are exposed as `part`s. Encapsulated, not sealed.
//
// An overlay-style host may leave the default roomy presentation in a side
// pane. A normal-flow panel with a hard row/rail allocation opts into the
// bounded contract instead:
//
//     <agui-conversation layout="docked"></agui-conversation>
//
// Docked mode contains the component's paint to its allocated box and compacts
// the composer so the transcript, the app panel beside it, and the controls all
// keep usable space. The host still chooses the allocation.

import { AgUiClient } from "/_agui/client.js";

const template = document.createElement("template");
template.innerHTML = `
  <link rel="stylesheet" href="/_agui/conversation.css">
  <div class="transcript" part="transcript" role="log" aria-live="polite" aria-label="Conversation">
    <p class="hint" part="hint"></p>
  </div>
  <form class="composer" part="composer">
    <div class="anchor" part="anchor" hidden>
      <span>pointing at <strong class="anchor-label"></strong></span>
      <button class="anchor-clear" type="button" aria-label="Stop pointing">×</button>
    </div>
    <label class="sr-only" for="input">Message the agent</label>
    <textarea id="input" part="input" rows="3" maxlength="12000"></textarea>
    <div class="footer">
      <span class="status" part="status" role="status" aria-live="polite"></span>
      <button class="send" part="send" type="submit">Send</button>
    </div>
  </form>`;

/// The words a person reads for each runtime state. `surface.tutor` is the
/// authoritative lifecycle; these are only its rendering.
const STATE_LABELS = {
  connecting: "connecting",
  warming: "agent starting",
  ready: "ready",
  done: "ready",
  thinking: "agent thinking",
  awaiting: "waiting for you",
  failed: "agent failed",
  attached: "agent attached",
  unavailable: "No agent is connected. Attach from a terminal at /mcp.",
};

/// States in which sending is meaningful for the in-page provider. `warming`
/// and `failed` are not: it has no session to receive the turn. An agent
/// attached over /mcp is a separate lane and is checked separately, because a
/// provider that cannot take a turn is not the same fact as nobody being here.
const READY_STATES = ["ready", "done", "thinking", "awaiting"];
const NO_AGENT_STATUS = "No agent is connected. Attach from a terminal at /mcp.";

/// What to say once a message has gone to the attached agents instead of to an
/// in-page provider.
///
/// Routed is not delivered. This used to say `sent to agentseat over /mcp`
/// whatever the far end was doing, which is a true statement about routing
/// that a person reads as one about delivery; measured, somebody read it for
/// nine minutes while the seat it named had closed. The host now says which
/// agents are actually listening, so the difference can be said out loud.
function relayedStatus(attached) {
  if (!attached || attached.length === 0) return "waiting for an answer";
  const live = attached.filter((agent) => !agent.quiet);
  const name = attached.length === 1
    ? (attached[0].label || "the attached agent")
    : `the ${attached.length} agents attached`;
  if (live.length === 0) return `sent to ${name}, nobody is reading /mcp right now`;
  return `sent to ${name} over /mcp`;
}

/// How long something has been waiting, in words rather than milliseconds.
function waitedFor(ms) {
  const minutes = Math.floor(ms / 60_000);
  if (minutes >= 1) return `${minutes}m`;
  return `${Math.max(1, Math.floor(ms / 1000))}s`;
}

/// The standing truth about a message nobody has collected.
///
/// The host has always computed this age; it rendered it only into the text
/// the collecting agent eventually read, so the one person who could not find
/// out that nobody had picked their message up was the person waiting for the
/// answer.
function unansweredStatus(attached, unanswered, oldestMs) {
  if (!unanswered) return null;
  const live = (attached || []).filter((agent) => !agent.quiet);
  if (live.length > 0) return null;
  const age = Number.isFinite(oldestMs) ? ` (${waitedFor(oldestMs)})` : "";
  return `sent, not picked up${age}`;
}

/// What to say when a send failed. `NO_AGENT_STATUS` used to be pasted over
/// every failure, which is a claim about /mcp derived from a dead TCP socket.
/// The failure's own words go first, and the attach hint is added only when
/// nobody is in fact attached.
function failureStatus(detail, attached) {
  if (attached && attached.length > 0) return detail;
  return `${detail} ${NO_AGENT_STATUS}`;
}

class AgUiConversation extends HTMLElement {
  static observedAttributes = ["hint", "placeholder"];

  constructor() {
    super();
    this.attachShadow({ mode: "open" }).append(template.content.cloneNode(true));
    const $ = (selector) => this.shadowRoot.querySelector(selector);
    this.ui = {
      transcript: $(".transcript"),
      hint: $(".hint"),
      form: $(".composer"),
      input: $("#input"),
      send: $(".send"),
      status: $(".status"),
      anchor: $(".anchor"),
      anchorLabel: $(".anchor-label"),
      anchorClear: $(".anchor-clear"),
    };
    // A per-tab token, so this page can skip the live echo of its OWN question:
    // it renders that the instant you hit Send, while every other connected
    // client renders it from the server's attributed `surface.ask` broadcast.
    this.origin =
      globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
    this.streaming = new Map();
    this.state = "connecting";
    /// Agents attached over /mcp, as `{ id, label }`. Seeded from `/provider`
    /// and kept current by the `surface.agents` event.
    this.attachedAgents = [];
    /// Chat nobody has collected, and how old the oldest of it is. Both come
    /// from the host with `surface.agents`, which the liveness tick republishes
    /// while nothing else is happening, so this stays current on its own.
    this.unanswered = 0;
    this.oldestUnansweredMs = null;
    this.ownsClient = false;
    this._client = null;
  }

  /// The transport. Assigning one adopts it; leaving it unset makes the element
  /// build and connect its own on `connectedCallback`.
  set client(client) {
    if (this._client === client) return;
    this._client = client;
    this.ownsClient = false;
    if (this.isConnected) this.subscribe();
  }

  get client() {
    return this._client;
  }

  connectedCallback() {
    if (this.started) return;
    this.started = true;

    this.ui.form.addEventListener("submit", (event) => {
      event.preventDefault();
      this.send();
    });
    this.ui.input.addEventListener("keydown", (event) => {
      // Enter sends, Shift+Enter is a newline. `isComposing` guards an IME
      // candidate window, where Enter means "accept this word", not "send".
      if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
        event.preventDefault();
        this.ui.form.requestSubmit();
      }
    });
    this.ui.anchorClear.addEventListener("click", () => {
      this.clearAnchor();
      this.dispatchEvent(new CustomEvent("anchor-clear", { bubbles: true }));
    });

    this.render();
    if (!this._client) {
      this._client = new AgUiClient();
      this.ownsClient = true;
    }
    this.subscribe();
    this.refreshAttachedAgents();
    if (this.ownsClient) this._client.connect();
  }

  /// Seed the attached-agent list. The live `surface.agents` event only fires
  /// on a change, so a page that loads after an agent attached would otherwise
  /// believe the room was empty until the next attach.
  async refreshAttachedAgents() {
    try {
      const provider = await this._client.getJson("/provider");
      this.attachedAgents = provider.agents || [];
      const liveness = provider.agent_liveness || {};
      this.unanswered = liveness.unanswered || 0;
      this.oldestUnansweredMs = liveness.oldestUnansweredMs ?? null;
      if (provider.provider_can_serve === false) {
        this.state = this.attachedAgents.length === 0 ? "unavailable" : "attached";
      }
      this.setState(this.state);
    } catch {
      /* the header reports transport trouble; the composer does not guess */
    }
  }

  disconnectedCallback() {
    for (const off of this.subscriptions || []) off();
    this.subscriptions = [];
  }

  attributeChangedCallback() {
    if (this.started) this.render();
  }

  render() {
    this.ui.hint.textContent = this.getAttribute("hint") || "";
    this.ui.hint.hidden = !this.ui.hint.textContent;
    this.ui.input.placeholder =
      this.getAttribute("placeholder") || "Message the agent…";
    this.setState(this.state);
  }

  subscribe() {
    for (const off of this.subscriptions || []) off();
    const client = this._client;
    if (!client) return;
    const on = (topic, handler) => client.on(topic, handler);
    this.subscriptions = [
      on("connection:open", () => this.setLive(true)),
      on("connection:error", () => {
        this.setLive(false);
        this.setState("connecting");
      }),
      on("RUN_STARTED", () => this.setLive(true)),
      on("TEXT_MESSAGE_START", (event) => this.beginStream(event)),
      on("TEXT_MESSAGE_CONTENT", (event) =>
        this.appendStream(event.messageId, event.delta)
      ),
      on("TEXT_MESSAGE_END", (event) => this.streaming.delete(event.messageId)),
      on("surface.tutor", (event) => {
        const value = event.value || {};
        this.setState(
          value.state || "ready",
          value.state === "failed" ? value.question || "" : ""
        );
      }),
      on("surface.ask", (event) => {
        const value = event.value || {};
        if (value.origin === this.origin) return;
        this.message(value.by === "you" ? "you" : "other", value.text || "", {
          author: value.by || "participant",
        });
      }),
      on("surface.narrate", (event) => {
        const value = event.value || {};
        this.message("agent", value.text || "", { author: value.by || "agent" });
      }),
      on("surface.agents", (event) => {
        const snapshot = event.value || {};
        this.attachedAgents = snapshot.attached || [];
        this.unanswered = snapshot.unanswered || 0;
        this.oldestUnansweredMs = snapshot.oldestUnansweredMs ?? null;
        if (snapshot.providerCanServe === false) {
          this.state = this.attachedAgents.length === 0
            ? (snapshot.providerState || "unavailable")
            : "attached";
        }
        // The composer may have been refusing to send while the provider was
        // down; an attach changes that answer, so repaint the status line.
        if (!this.ui.send.disabled) this.setState(this.state);
      }),
      on("surface.history", (event) => {
        // A replayed transcript replaces what is on screen rather than
        // appending to it; the server is the authority on what was said.
        this.streaming.clear();
        this.ui.transcript.replaceChildren(this.ui.hint);
        for (const entry of (event.value || {}).entries || []) {
          this.message(entry.by === "you" ? "you" : "agent", entry.text || "", {
            author: entry.by,
          });
        }
      }),
    ];
  }

  // ── transcript ──────────────────────────────────────────────────────────

  message(role, text, options = {}) {
    if (!text && !options.allowEmpty) return null;
    this.ui.hint.hidden = true;
    const wrapper = document.createElement("article");
    wrapper.className = `message ${role}${options.error ? " error" : ""}`;
    wrapper.setAttribute("part", "message");
    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = options.author || (role === "you" ? "you" : "agent");
    const body = document.createElement("div");
    body.className = "body";
    body.textContent = text || "";
    wrapper.append(meta, body);
    this.ui.transcript.append(wrapper);
    this.ui.transcript.scrollTop = this.ui.transcript.scrollHeight;
    return body;
  }

  beginStream(event) {
    if (!event.messageId || this.streaming.has(event.messageId)) return;
    const body = this.message(event.role === "user" ? "you" : "agent", "", {
      allowEmpty: true,
    });
    if (body) this.streaming.set(event.messageId, body);
  }

  appendStream(id, delta) {
    if (!id || !delta) return;
    if (!this.streaming.has(id)) this.beginStream({ messageId: id });
    const body = this.streaming.get(id);
    if (!body) return;
    body.textContent += delta;
    this.ui.transcript.scrollTop = this.ui.transcript.scrollHeight;
  }

  /// Everything currently in the transcript, for tests and for a host page that
  /// wants to save or export the conversation.
  messages() {
    return [...this.ui.transcript.querySelectorAll(".message")].map((node) => ({
      by: node.querySelector(".meta")?.textContent || "",
      text: node.querySelector(".body")?.textContent || "",
    }));
  }

  // ── state ───────────────────────────────────────────────────────────────

  setState(state, detail = "") {
    this.state = state;
    // A message nobody has collected outranks the lifecycle word. `ready` is
    // a claim that a turn will be served, and while every attached agent is
    // quiet with a question waiting, it is the wrong thing to show a person
    // who is watching this line to find out whether they were heard.
    const unserved = unansweredStatus(
      this.attachedAgents,
      this.unanswered,
      this.oldestUnansweredMs,
    );
    const attachedStatus = !READY_STATES.includes(state) && this.attachedAgents.length
      ? this.attachedAgents.some((agent) => !agent.quiet)
        ? "Agent attached. Messages go to the shared conversation."
        : "Agent attached but quiet. Messages will wait for pickup."
      : null;
    this.ui.status.textContent =
      unserved || attachedStatus || detail || STATE_LABELS[state] || "Shift + Enter for a new line";
    this.classList.toggle("failed", state === "failed");
    this.classList.toggle("unserved", Boolean(unserved));
    this.dispatchEvent(
      new CustomEvent("runtime-state", { detail: { state, detail }, bubbles: true })
    );
  }

  setLive(live) {
    this.classList.toggle("live", live);
  }

  /// Can this composer hand a message to anybody? Either the in-page provider
  /// is live, or somebody is attached over /mcp and the runtime relays to them.
  get ready() {
    return READY_STATES.includes(this.state) || this.attachedAgents.length > 0;
  }

  // ── the anchor: what "this" currently means ─────────────────────────────

  /// Show what a following question will bind to. The referent itself lives in
  /// the runtime (the surface posted `/semantic`); this is only the reminder,
  /// so a person can see what they pointed at before they say "this".
  setAnchor(anchor) {
    if (!anchor || !anchor.label) return this.clearAnchor();
    this.ui.anchorLabel.textContent = anchor.label;
    this.ui.anchor.hidden = false;
  }

  clearAnchor() {
    this.ui.anchor.hidden = true;
    this.ui.anchorLabel.textContent = "";
  }

  // ── asking ──────────────────────────────────────────────────────────────

  async send() {
    const question = this.ui.input.value.trim();
    if (!question || this.ui.send.disabled) return;
    if (!this.ready) {
      // Reachable only when nothing is attached: an attached agent makes the
      // composer ready regardless of the in-page provider's lifecycle.
      this.ui.status.textContent = NO_AGENT_STATUS;
      return;
    }
    this.ui.send.disabled = true;
    this.ui.status.textContent = "Sending…";
    try {
      // Barge-in first: a question asked while the agent is mid-turn replaces
      // that turn rather than queueing behind it. An idle provider has nothing
      // to interrupt, so its refusal is not an error.
      try {
        await this._client.interrupt();
      } catch {
        /* nothing in flight */
      }
      this.message("you", question);
      this.ui.input.value = "";
      const accepted = await this._client.ask(question, this.origin);
      // The runtime says when it handed the message to attached agents rather
      // than to an in-page provider. Say so, instead of leaving a person to
      // wonder which of the two is thinking.
      this.ui.status.textContent =
        accepted && accepted.relayed === "mcp"
          ? relayedStatus(this.attachedAgents)
          : STATE_LABELS[this.state] || "";
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      const status = failureStatus(detail, this.attachedAgents);
      this.ui.status.textContent = status;
      this.message("agent", status, { author: "surface", error: true });
    } finally {
      this.ui.send.disabled = false;
    }
  }
}

if (!customElements.get("agui-conversation")) {
  customElements.define("agui-conversation", AgUiConversation);
}

export { AgUiConversation };
