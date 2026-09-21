// Application chrome: what is left once the runtime owns the conversation.
//
// This file used to carry a transcript renderer, a streaming-message map, five
// `surface.*` subscriptions, a composer, and barge-in. The same code every
// other app in this repository had its own copy of. That is now
// `<agui-conversation>`, served by the crate.
//
// What remains is genuinely this app's: the header's connection chip, and the
// bridge that tells the composer what "this" currently points at. The shared
// artifact is not here either. It is in the atlas extension's CRDT replica.

import { AgUiClient, loadExtensions } from "/_agui/client.js";
import { attachSemanticTargetHost } from "/_agui/semantic-targets.js";
import { installPaneLayout } from "/panes.js";
import { installWorkspaceUI } from "/workspace-ui.js";
import "/_agui/provider-settings.js";
import "/_agui/conversation.js";

const client = new AgUiClient();
const byId = (id) => document.getElementById(id);
const requestedMode = new URLSearchParams(globalThis.location.search).get("atlas-mode");
const atlasMode = requestedMode === "learn" ? "learn" : "author";
document.body.dataset.atlasMode = atlasMode;
const runtimeLabel = byId("runtime-state");
const connectionDot = byId("connection-dot");
const conversation = byId("conversation");

if (atlasMode === "learn") {
  byId("project-label").textContent = "Temporary refresher";
  document.querySelector(".canvas-heading strong").textContent = "Refresher";
  document.querySelector(".canvas-heading span").textContent = "Learn what you need, then return to work";
}

// One transport for the whole page: the conversation, the extensions, and the
// atlas replica all watch the same stream. Assigned before `connect()`, so no
// event can arrive before the panel is subscribed.
conversation.client = client;

// Deliberately outside `start()`: the layout is the human's, and a provider
// that fails to warm up must not leave them stuck with columns they cannot
// move. This is chrome, not runtime state.
if (atlasMode === "author") installPaneLayout(byId("page-grid"));
if (atlasMode === "author") installWorkspaceUI(client);

const LABELS = {
  connecting: "connecting",
  warming: "agent starting",
  ready: "ready",
  done: "ready",
  thinking: "agent thinking",
  awaiting: "waiting for you",
  failed: "agent failed",
  attached: "agent attached",
  unavailable: "no agent connected",
};

// What the header currently knows. It is repainted from this, never written
// to directly, because the old code wrote a one-off failure DETAIL string into
// the label and nothing ever wrote over it: the header spent a whole session
// reading "No agent is connected" while an agent was attached and working.
const header = { state: "connecting", agents: [], unanswered: 0, providerCanServe: false };

/// How long an agent has been quiet, in words a person reads at a glance.
function quietFor(ms) {
  const minutes = Math.floor(ms / 60_000);
  if (minutes >= 1) return `${minutes}m`;
  return `${Math.max(1, Math.floor(ms / 1000))}s`;
}

/// Who is attached, and whether any of them is actually there.
///
/// `attached` alone was a true statement about the past being read as one
/// about the present: the host's registry was insert-only, so an agent that
/// had ever said hello was believed present for the life of the process, and
/// this header read `agentseat attached · ready` for nine minutes while the
/// seat it named had closed and nobody was reading a thing.
function attachedPhrase(agents) {
  const quiet = agents.filter((agent) => agent.quiet);
  const name = agents.length === 1
    ? (agents[0].label || "1 agent")
    : `${agents.length} agents`;
  if (quiet.length === agents.length) {
    const since = Math.min(...quiet.map((agent) => agent.quietForMs ?? 0));
    return `${name} attached, quiet for ${quietFor(since)}`;
  }
  if (quiet.length) return `${name} attached, ${quiet.length} quiet`;
  return `${name} attached`;
}

function paintRuntime() {
  const attached = header.agents.length;
  const live = header.agents.some((agent) => !agent.quiet);
  const provider = LABELS[header.state] || header.state;
  // A provider that failed is not news when somebody is attached over /mcp,
  // and it is the only thing worth saying when nobody is.
  const parts = [];
  if (attached) parts.push(attachedPhrase(header.agents));
  // `ready` is a claim that a turn will be served. With every attached agent
  // quiet and no in-page provider, nothing will serve it, so the header says
  // what is true instead of the word a person acts on.
  if (!attached) parts.push(provider);
  else if (header.providerCanServe && isProviderLive(header.state)) parts.push(provider);
  else if (!live) parts.push("nobody listening");
  if (header.unanswered > 0 && !live) {
    parts.push(`${header.unanswered} message${header.unanswered === 1 ? "" : "s"} not picked up`);
  }
  runtimeLabel.textContent = parts.join(" · ");
  runtimeLabel.title = runtimeLabel.textContent;
  connectionDot.classList.toggle("failed", header.state === "failed" && !attached);
  connectionDot.classList.toggle("quiet", attached && !live);
}

/// Is the IN-PAGE provider able to take a turn? Separate from whether an
/// agent is attached, because they are two different lanes and conflating
/// them is what let one of them speak for the other.
function isProviderLive(state) {
  return state === "ready" || state === "done" || state === "thinking" || state === "awaiting";
}

function setRuntime(state) {
  header.state = state;
  paintRuntime();
}

function setAttachedAgents(agents, unanswered = header.unanswered) {
  header.agents = Array.isArray(agents) ? agents : [];
  header.unanswered = Number.isFinite(unanswered) ? unanswered : 0;
  paintRuntime();
}

// The panel already tracks the runtime lifecycle for its own status line; the
// header mirrors it rather than subscribing to `surface.tutor` a second time.
// The failure DETAIL deliberately does not come along: it is one moment's
// error text, and the header is a current-state readout.
conversation.addEventListener("runtime-state", (event) => {
  setRuntime(event.detail.state);
});
client.on("surface.agents", (event) => {
  const snapshot = event.value || {};
  header.providerCanServe = snapshot.providerCanServe === true;
  setAttachedAgents(snapshot.attached, snapshot.unanswered);
  if (header.agents.length === 0 && snapshot.providerCanServe === false) {
    setRuntime(snapshot.providerState || "unavailable");
  }
});
client.on("connection:open", () => connectionDot.classList.add("live"));
client.on("connection:error", () => connectionDot.classList.remove("live"));
client.on("RUN_STARTED", () => connectionDot.classList.add("live"));

document.querySelector("agui-provider-settings").addEventListener("provider-state", (event) => {
  const snapshot = event.detail || {};
  if (snapshot.ready) setRuntime("ready");
  else if (snapshot.error) setRuntime("failed");
  else if (snapshot.warming) setRuntime("warming");
  else if (header.agents.length === 0) setRuntime("unavailable");
});

async function start() {
  try {
    const provider = await client.getJson("/provider");
    header.providerCanServe = provider.provider_can_serve === true;
    // Seeded before the lifecycle, so a page opened after an agent attached
    // never paints a failure it already knows is not the whole story.
    setAttachedAgents(provider.agents, (provider.agent_liveness || {}).unanswered);
    if (provider.ready) setRuntime("ready");
    else if (provider.error) setRuntime("failed");
    else if (provider.warming) setRuntime("warming");
    else setRuntime("unavailable");
    // Identity lives in `localStorage`, not `sessionStorage`, and that is the
    // whole fix for one person showing up in the presence strip twice.
    // `sessionStorage` is scoped to a *tab*, so a second tab — or a reload
    // into a new one — minted a second participant with the same hardcoded
    // "You" label, and the strip claimed two humans were on the page. One
    // browser profile is now one participant across every tab it opens.
    //
    // This still is not authentication: nothing stops someone editing the
    // value, and the host takes the browser's word for who it is. That is
    // roadmap #17, and the label below says as much rather than implying a
    // guarantee this does not make.
    const KEY = "agui.semanticParticipantId";
    let participantId = localStorage.getItem(KEY)
      || sessionStorage.getItem(KEY); // carry over an id minted before this change
    if (!participantId) participantId = `human-${crypto.randomUUID()}`;
    localStorage.setItem(KEY, participantId);
    sessionStorage.removeItem(KEY);

    await attachSemanticTargetHost({
      client,
      composer: conversation,
      participant: { id: participantId, label: "You" },
    });
    await loadExtensions(client);
    client.connect();
  } catch (error) {
    window.atlasValidation?.error("startup_error", error);
    setRuntime("failed");
    conversation.message("agent", String(error?.message || error), {
      author: "surface",
      error: true,
    });
  }
}

start();
