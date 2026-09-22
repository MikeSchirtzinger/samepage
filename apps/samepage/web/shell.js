import { AgUiClient, loadExtensions, rememberMe, whoAmI } from "/_agui/client.js";
import {
  beginPersistenceHandshake,
  finishPersistenceHandshake,
  persistenceFailed,
  persistencePending,
  persistencePresentation,
  persistenceResolved,
} from "/persistence.mjs";

const statusNode = document.getElementById("connection-status");
const errorNode = document.getElementById("error-banner");
const client = new AgUiClient();
let kernel;

function showError(message) {
  errorNode.textContent = message;
  errorNode.hidden = false;
}

function setStatus(message) { statusNode.textContent = message; }

function envelope(raw, operation) {
  const value = JSON.parse(raw);
  if (value?.abi !== "samepage.capability.v1") throw new Error(`${operation}: capability ABI mismatch`);
  if (value.error) throw new Error(`${operation}: ${value.error.code}: ${value.error.message}`);
  if (!("ok" in value)) throw new Error(`${operation}: no result`);
  return value.ok;
}

function createKernel({ documentModule, manifest }) {
  if (!documentModule) throw new Error("the shared document was not verified");
  const descriptors = manifest.extensions.filter((entry) => entry.artifact || entry.nativeOnly)
    .map((entry) => entry.artifact?.descriptor || entry.nativeOnly);
  const modules = new Map();
  const renderers = new Map();
  const subscribers = new Set();
  let replica = new documentModule.Replica();
  replica.set_composition(JSON.stringify(descriptors));
  let socket = null;
  let runtime = null;
  let sync = "connecting";
  let refusal = null;
  let persistenceState = Object.freeze({failure:null, pending:null, generation:0});
  let connectGeneration = 0;
  let renderQueued = false;
  let seenHello = 0;

  const state = () => Object.freeze({ sync, refusal, persistence: persistenceState.failure, savePending: persistenceState.pending, revision: replica.revision(), capabilities: runtime?.loaded.map(({id,status}) => ({id,status})) || [] });
  const notify = () => {
    const persistence = persistencePresentation(persistenceState);
    setStatus(persistence.status || (sync === "ready" ? `Synced · ${replica.revision().slice(0, 9)}` : sync));
    for (const listener of subscribers) listener(state());
  };
  const view = (kinds = [], ids) => JSON.parse(replica.view(JSON.stringify(kinds), ids === undefined ? undefined : JSON.stringify(ids)));
  const render = async () => {
    renderQueued = false;
    const snapshot = Object.freeze({ view: view(), revision: replica.revision(), status: state() });
    for (const [id, renderer] of renderers) {
      try { await renderer.render(snapshot); }
      catch (error) { showError(`${id} renderer failed: ${error.message}`); }
    }
  };
  const scheduleRender = () => {
    if (!renderQueued) { renderQueued = true; requestAnimationFrame(() => void render()); }
  };
  const sendFrames = (frames) => {
    if (socket?.readyState !== WebSocket.OPEN) return;
    for (const frame of frames) socket.send(frame);
  };
  const flush = () => sendFrames(replica.take_outbound());
  const connect = () => {
    const generation = ++connectGeneration;
    const persistenceAtStart = beginPersistenceHandshake(persistenceState);
    sync = "connecting";
    seenHello = 0;
    notify();
    const ws = new WebSocket(`${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/ws`);
    socket = ws;
    ws.binaryType = "arraybuffer";
    ws.onopen = () => { if (generation === connectGeneration) sendFrames([replica.hello()]); };
    ws.onmessage = async (event) => {
      if (generation !== connectGeneration) return;
      try {
        sendFrames(replica.receive(new Uint8Array(event.data)));
        flush();
        if (replica.take_dirty()) { scheduleRender(); notify(); }
        if (++seenHello >= 2 && sync !== "ready") {
          sync = "ready";
          // Clear only a fault carried into this reconnect. An SSE fault that
          // arrived between the two hello frames belongs to the new host.
          persistenceState = finishPersistenceHandshake(persistenceState, persistenceAtStart);
          if (refusal) {
            showError(`Edit refused: ${refusal}${refusal.endsWith("…") ? "" : "."} Unsent edits from that document were discarded. This document is now synced.`);
          } else if (persistencePresentation(persistenceState).banner) {
            showError(persistencePresentation(persistenceState).banner);
          } else {
            errorNode.hidden = true;
          }
          notify();
          for (const kind of JSON.parse(replica.kinds())) await runtime?.activateForKind(kind);
          scheduleRender();
        }
      } catch (error) {
        showError(`Document sync failed: ${error.message}`);
        ws.close();
      }
    };
    ws.onerror = () => { if (generation === connectGeneration) setStatus("Connection error"); };
    ws.onclose = (event) => {
      if (generation !== connectGeneration) return;
      const rejected = event.code === 4409;
      const wireReason = event.reason || "host refused the edit";
      const reason = wireReason.trimEnd() + (new TextEncoder().encode(wireReason).byteLength >= 120 ? "…" : "");
      if (rejected) {
        refusal = reason;
        replica.mark_refused(reason);
        showError(`Edit refused: ${reason}${reason.endsWith("…") ? "" : "."} Unsent edits from this document were discarded. Reconnecting with a fresh document.`);
      } else {
        showError("Connection lost. Unconfirmed edits may be lost while a fresh document reconnects.");
      }
      sync = rejected ? "refused; reconnecting" : "disconnected; reconnecting";
      notify();
      replica.free();
      replica = new documentModule.Replica();
      replica.set_composition(JSON.stringify(descriptors));
      setTimeout(async () => {
        if (generation !== connectGeneration) return;
        try {
          // The host's joined-person registry is process-local. A host restart
          // invalidates the old cookie, so rejoin before attempting another WS.
          const response = await fetch("/surface/me", {method:"POST", credentials:"same-origin"});
          const person = response.ok ? await response.json() : null;
          if (!person?.joined) throw new Error("host identity join failed");
          rememberMe(person);
          if (generation === connectGeneration) connect();
        } catch (error) {
          showError(`Document reconnect is waiting for identity: ${error.message}`);
          if (generation === connectGeneration) setTimeout(() => connect(), 1500);
        }
      }, 900);
    };
  };
  const requireReady = () => {
    if (sync !== "ready" || socket?.readyState !== WebSocket.OPEN) throw new Error("the shared document is not synced");
    if (persistenceState.failure) throw new Error(`persistence failed: ${persistenceState.failure}`);
    if (persistenceState.pending) throw new Error(`persistence is pending: ${persistenceState.pending}`);
  };
  const moduleFor = (id) => {
    const module = modules.get(id);
    if (!module) throw new Error(`${id} capability is unavailable`);
    return module;
  };
  const api = Object.freeze({
    view, revision: () => replica.revision(), ready: () => sync === "ready", status: state,
    registerCapability(id, module) {
      if (modules.has(id)) throw new Error(`${id} capability registered twice`);
      if (!manifest.extensions.some((entry) => entry.id === id && entry.artifact)) throw new Error(`undeclared capability ${id}`);
      modules.set(id, module);
    },
    registerRenderer(id, renderer) {
      if (!modules.has(id) || renderers.has(id) || typeof renderer?.render !== "function") throw new Error(`invalid renderer registration for ${id}`);
      renderers.set(id, renderer);
      scheduleRender();
      return () => { renderer.dispose?.(); renderers.delete(id); };
    },
    subscribe(listener) { subscribers.add(listener); listener(state()); return () => subscribers.delete(listener); },
    async project(id, selectedView = view()) {
      await runtime?.activate(id);
      return envelope(moduleFor(id).project(JSON.stringify(selectedView)), `${id}.project`);
    },
    async plan(id, action, args, selectedView = view()) {
      await runtime?.activate(id);
      return envelope(moduleFor(id).plan(JSON.stringify({ action, args }), JSON.stringify(selectedView)), `${id}.plan`);
    },
    apply(id, plan) {
      requireReady();
      const result = JSON.parse(replica.apply(id, JSON.stringify(plan)));
      flush(); scheduleRender(); notify();
      return result;
    },
    applyBatch(batch) {
      requireReady();
      const result = JSON.parse(replica.apply_batch(JSON.stringify(batch)));
      flush(); scheduleRender(); notify();
      return result;
    },
    activate: (id) => runtime?.activate(id),
    activateForKind: (kind) => runtime?.activateForKind(kind),
    activateForTool: (tool) => runtime?.activateForTool(tool),
    activateForRoute: (route) => runtime?.activateForRoute(route),
    dispatch: (name, args) => client.action(name, args),
    attachRuntime(value) { if (runtime) throw new Error("kernel runtime already attached"); runtime = value; notify(); },
    connect,
    setPersistenceFailure(message) { persistenceState = persistenceFailed(persistenceState, message); showError(persistencePresentation(persistenceState).banner); notify(); },
    setPersistencePending(message) { persistenceState = persistencePending(persistenceState, message); showError(persistencePresentation(persistenceState).banner); notify(); },
    setPersistenceResolved() { persistenceState = persistenceResolved(persistenceState); if (!persistenceState.failure && !refusal) errorNode.hidden = true; notify(); },
    // Read-only diagnostics for real cross-peer verification.
    diagnostics: () => ({ clientId: replica.client_id(), revision: replica.revision(), status: state(), view: view() }),
  });
  return api;
}

try {
  const person = await whoAmI();
  if (!person) throw new Error("host identity join failed; document edits require a joined person");
  const runtime = await loadExtensions(client, { createApplicationContext: (parts) => {
    kernel = createKernel(parts);
    return kernel;
  } });
  kernel.attachRuntime(runtime);
  client.on("samepage.persistence-error", ({value}) => kernel.setPersistenceFailure(value?.error || "save failed"));
  client.on("samepage.persistence-wait", ({value}) => kernel.setPersistencePending(value?.message || "save is still pending"));
  client.on("samepage.persistence-resolved", () => kernel.setPersistenceResolved());
  await client.connect();
  kernel.connect();
  document.getElementById("add-card").addEventListener("submit", async (event) => {
    event.preventDefault();
    const input = document.getElementById("new-card-label");
    try { await kernel.dispatch("card_place", {label:input.value.trim()}); input.value = ""; }
    catch (error) { showError(`Card was not added: ${error.message}`); }
  });
  // Exposes observations without a second document or mutation channel.
  globalThis.samepageReadback = () => kernel.diagnostics();
} catch (error) {
  setStatus("Unavailable");
  showError(`SamePage could not start: ${error.message}`);
  console.error(error);
}
