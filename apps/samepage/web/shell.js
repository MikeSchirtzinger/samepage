import { createLegend } from "/legend.mjs";
import { AgUiClient, loadExtensions, rejoin, rememberMe, whoAmI } from "/_agui/client.js";
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

// The red bar is for losing the shared document only: connection loss, a
// failed sync, a save that failed or is pending. Everything else is a problem
// owned by one capability and shown inside that capability's tab or above the
// dock, keyed by action so the action's next success clears it.
function showSyncLoss(message) {
  const text = document.createElement("p"); text.textContent = message;
  const dismiss = document.createElement("button"); dismiss.type = "button"; dismiss.className = "quiet";
  dismiss.textContent = "Dismiss"; dismiss.addEventListener("click", () => { errorNode.hidden = true; });
  errorNode.replaceChildren(text, dismiss);
  errorNode.dataset.message = message;
  errorNode.hidden = false;
}
function clearSyncLoss() { errorNode.hidden = true; errorNode.replaceChildren(); delete errorNode.dataset.message; }

const problems = new Map();
const problemKey = (capability, action) => `${capability}\u0000${action}`;
function renderProblems() {
  const slots = new Map([...document.querySelectorAll(".problems[data-capability]")].map(node => [node.dataset.capability, node]));
  const lists = new Map([...slots.keys()].map(id => [id, []]));
  const dock = [];
  for (const entry of problems.values()) (lists.get(entry.capability) || dock).push(entry);
  const item = (entry) => {
    const box = document.createElement("div"); box.className = "problem"; box.setAttribute("role", "alert");
    box.dataset.capability = entry.capability; box.dataset.action = entry.action;
    const text = document.createElement("p"); text.textContent = entry.message;
    const close = document.createElement("button"); close.type = "button"; close.textContent = "×";
    close.setAttribute("aria-label", "Dismiss"); close.title = "Dismiss";
    close.addEventListener("click", () => resolveProblem(entry.capability, entry.action));
    box.append(text, close); return box;
  };
  for (const [id, node] of slots) node.replaceChildren(...lists.get(id).map(item));
  document.getElementById("dock-problems")?.replaceChildren(...dock.slice(-3).map(item));
  for (const tab of document.querySelectorAll(".tab[data-tab]")) tab.dataset.problem = String((lists.get(tab.dataset.tab) || []).length > 0);
}
function reportProblem(capability, action, error) {
  const message = String(error?.message ?? error ?? "failed");
  problems.set(problemKey(capability, action), {capability, action, message, at: Date.now()});
  renderProblems();
}
function resolveProblem(capability, action) {
  let changed = false;
  for (const [key, entry] of problems) {
    if (entry.capability === capability && (action === undefined || entry.action === action)) { problems.delete(key); changed = true; }
  }
  if (changed) renderProblems();
}
function resolveAction(action) {
  let changed = false;
  for (const [key, entry] of problems) if (entry.action === action) { problems.delete(key); changed = true; }
  if (changed) renderProblems();
}
let toastTimer = 0;
function toast(message, tone = "info") {
  const node = document.getElementById("toast");
  if (!node) return;
  node.textContent = String(message || "").split("\n")[0].slice(0, 200);
  node.dataset.tone = tone;
  node.title = String(message || "");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { if (node.dataset.tone !== "error") node.textContent = ""; }, 12000);
}

function setStatus(message) { statusNode.textContent = message; }

function envelope(raw, operation) {
  const value = JSON.parse(raw);
  if (value?.abi !== "samepage.capability.v2") throw new Error(`${operation}: capability ABI mismatch`);
  if (value.error) throw new Error(`${operation}: ${value.error.code}: ${value.error.message}`);
  if (!("ok" in value)) throw new Error(`${operation}: no result`);
  return value.ok;
}

function createKernel({ documentModule, manifest }) {
  if (!documentModule) throw new Error("the shared document was not verified");
  const descriptors = manifest.extensions.filter((entry) => entry.artifact || entry.nativeOnly)
    .map((entry) => entry.artifact?.descriptor || entry.nativeOnly);
  const modules = new Map();
  const legend = createLegend({mount:document.getElementById('legend'), decode:envelope,
    read:()=>({modules, states:runtime?.loaded || [], view:view(),
      failed:renderErrors.filter(e=>e.code==='renderer_failure').map(e=>e.message.split(':')[0])})});
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
  let rendering = false;
  let renderedRevision = null;
  let seenHello = 0;
  const observationStream = crypto.randomUUID();
  let observationSequence = 0;
  let lastObservation = '';
  let renderErrors = [];
  let lastObservedAt = 0;
  let observing = false;
  let observationError = null;
  let identityExpired = false;
  // Close code this page uses when it drops its own socket to pick up a
  // renewed identity. Client-chosen codes live in 4000-4999.
  const IDENTITY_RENEWED = 4001;
  // Whether the host still recognises this browser's resume cookie. One GET
  // and never a join, so nothing here makes an idle page rejoin by itself. A
  // host that cannot be reached counts as still joined: an outage is not an
  // identity problem.
  const stillJoined = () => fetch("/surface/me", {credentials: "same-origin"})
    .then(response => response.ok ? response.json() : null)
    .then(me => me ? me.joined === true : true)
    .catch(() => true);
  const whenSynced = (ms) => new Promise((resolve, reject) => {
    const deadline = Date.now() + ms;
    const check = () => {
      if (sync === "ready" && socket?.readyState === WebSocket.OPEN) return resolve();
      if (Date.now() > deadline) return reject(new Error("the shared document did not resync after rejoining"));
      setTimeout(check, 100);
    };
    check();
  });
  // Join again and carry the new identity onto the document socket, which
  // still holds the one captured when it connected. The socket's close path
  // rebuilds the replica from a fresh copy, so nothing is applied twice.
  const rejoinAsNewPerson = async () => {
    const me = await rejoin();
    if (!me) throw new Error("rejoining this page failed");
    Object.assign(globalThis.samepagePerson ??= {}, me);
    socket?.close(IDENTITY_RENEWED, "identity renewed");
    await whenSynced(10000);
  };
  const observeLayout = async () => {
    if (identityExpired) return;
    if (sync !== 'ready' || rendering || observing || renderedRevision !== replica.revision()) return;
    const stage = document.querySelector('.map-stage');
    if (!stage) return;
    // Stage units, so the map camera's zoom never reaches the host.
    const x = Number(stage.dataset.originX), y = Number(stage.dataset.originY);
    const bounds = Object.fromEntries([...stage.querySelectorAll('.map-card')].map(card =>
      [card.dataset.id, [card.offsetLeft-x, card.offsetTop-y, card.offsetLeft-x+card.offsetWidth, card.offsetTop-y+card.offsetHeight]]));
    const unavailable = (runtime?.errors || []).filter(entry => entry.id !== "trace").map(entry => ({code:'capability_unavailable', message:`${entry.id}: ${entry.error?.message || entry.status}`}));
    const report = {stream:observationStream, revision:renderedRevision, bounds, errors:[...renderErrors, ...unavailable].slice(-32)};
    const signature = JSON.stringify(report);
    if (signature === lastObservation && Date.now() - lastObservedAt < 5000) return;
    lastObservation = signature;
    lastObservedAt = Date.now();
    observing = true;
    const observationGeneration = connectGeneration;
    try {
      await client.action('page_observe', {...report, sequence:++observationSequence});
      if (observationGeneration === connectGeneration && observationError) {
        resolveProblem("map", "validate");
        observationError = null;
      }
    }
    catch (error) {
      // A newer CRDT revision may arrive while the observation is in flight.
      // Retry its actual geometry on the next render; never mark it passed.
      if (observationGeneration !== connectGeneration || sync !== 'ready') return;
      lastObservation = '';
      if (!identityExpired && !(await stillJoined())) {
        // The host refused the measurement because this browser's resume
        // cookie expired. Measuring is not a person's action, so it does not
        // rejoin; the next action this person takes does. Stop re-measuring
        // until then.
        identityExpired = true;
        observationError = "Page validation is paused: your identity on this page expired. Your next action rejoins.";
        reportProblem("map", "validate", observationError);
        return;
      }
      if (!String(error.message).includes('stale revision')) {
        observationError = `Page validation unavailable: ${error.message}`;
        reportProblem("map", "validate", observationError);
      }
    } finally { observing = false; }
  };
  // Re-measure a healthy unchanged page before host evidence expires.
  setInterval(() => void observeLayout(), 5000);
  const sizeObserver = typeof ResizeObserver === 'function' ? new ResizeObserver(() => void observeLayout()) : null;

  const state = () => Object.freeze({ sync, refusal, persistence: persistenceState.failure, savePending: persistenceState.pending, revision: replica.revision(), capabilities: runtime?.loaded.map(({id,status}) => ({id,status})) || [] });
  const notify = () => {
    const persistence = persistencePresentation(persistenceState);
    setStatus(persistence.status || (sync === "ready" ? `Synced · ${replica.revision().slice(0, 9)}` : sync));
    for (const listener of subscribers) listener(state());
  };
  const view = (kinds = [], ids) => JSON.parse(replica.view(JSON.stringify(kinds), ids === undefined ? undefined : JSON.stringify(ids)));
  const render = async () => {
    if (rendering) { renderQueued = true; return; }
    rendering = true;
    renderQueued = false;
    renderedRevision = null;
    const snapshot = Object.freeze({ view: view(), revision: replica.revision(), status: state() });
    const errors = [];
    try {
      for (const [id, renderer] of renderers) {
        if (snapshot.revision !== replica.revision()) break;
        try { await renderer.render(snapshot); resolveProblem(id, "render"); }
        catch (error) { errors.push({code:"renderer_failure",message:`${id}: ${error.message}`}); reportProblem(id, "render", `${id} could not draw: ${error.message}`); }
      }
      if (snapshot.revision === replica.revision()) {
        renderedRevision = snapshot.revision;
        renderErrors = errors;
        legend.refresh();
        sizeObserver?.disconnect();
        document.querySelectorAll('.map-card').forEach(card => sizeObserver?.observe(card));
      }
    } finally {
      rendering = false;
      void observeLayout();
      if (renderQueued || snapshot.revision !== replica.revision()) {
        renderQueued = false;
        scheduleRender();
      }
    }
  };
  const scheduleRender = () => {
    if (!renderQueued) { renderQueued = true; requestAnimationFrame(() => void render()); }
  };
  const activateForCurrentView = async () => {
    try { await runtime?.activateForView(view()); resolveProblem("page", "load"); }
    catch (error) { reportProblem("page", "load", `Capability unavailable: ${error.message}`); console.error("[samepage] capability activation failed:", error); }
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
    lastObservation = "";
    lastObservedAt = 0;
    identityExpired = false;
    renderedRevision = null;
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
        if (replica.take_dirty()) { await activateForCurrentView(); scheduleRender(); notify(); }
        if (++seenHello >= 2 && sync !== "ready") {
          sync = "ready";
          // Clear only a fault carried into this reconnect. An SSE fault that
          // arrived between the two hello frames belongs to the new host.
          persistenceState = finishPersistenceHandshake(persistenceState, persistenceAtStart);
          // A refused edit stays with the capability that refused it; the
          // document itself is synced again, so the red bar goes.
          refusal = null;
          if (persistencePresentation(persistenceState).banner) showSyncLoss(persistencePresentation(persistenceState).banner);
          else clearSyncLoss();
          document.body.dataset.sync = "ready";
          notify();
          await activateForCurrentView();
          scheduleRender();
        }
      } catch (error) {
        showSyncLoss(`Document sync failed: ${error.message}. Reconnecting.`);
        ws.close();
      }
    };
    ws.onerror = () => { if (generation === connectGeneration) setStatus("Connection error"); };
    ws.onclose = (event) => {
      if (generation !== connectGeneration) return;
      const rejected = event.code === 4409;
      const wireReason = event.reason || "host refused the edit";
      const reason = wireReason.trimEnd() + (new TextEncoder().encode(wireReason).byteLength >= 120 ? "…" : "");
      document.body.dataset.sync = "reconnecting";
      if (rejected) {
        refusal = reason;
        replica.mark_refused(reason);
        const owner = /capability "([a-z0-9_-]+)"/i.exec(reason)?.[1] || "page";
        const refused = `Edit refused: ${reason}${reason.endsWith("…") ? "" : "."} Your unsent edits were discarded.`;
        reportProblem(owner, "edit", refused);
        toast(refused, "error");
        showSyncLoss("Reconnecting with a fresh copy of the page after a refused edit…");
      } else if (event.code === IDENTITY_RENEWED) {
        showSyncLoss("Rejoining this page: your identity here had expired. Earlier edits keep the name they were signed with.");
      } else {
        showSyncLoss("Connection lost. Unconfirmed edits may be lost while a fresh document reconnects.");
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
          // Bylines compare against this object live, so a person admitted
          // afresh after a restart reads as "you" without a reload.
          Object.assign(globalThis.samepagePerson ??= {}, person);
          if (generation === connectGeneration) connect();
        } catch (error) {
          showSyncLoss(`Document reconnect is waiting for identity: ${error.message}`);
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
  // The native commit runs every capability's cascade for deleted records in
  // the same batch. The browser replica does not, so the shell asks each
  // loaded capability for its cascade before applying. Without it a deleted
  // pane leaves its layout and annotation records behind and the host
  // refuses the whole edit as an orphan.
  const deletedIn = (entries) => entries.flatMap(entry => (entry.plan?.ops || [])
    .filter(op => op.op === "delete" && typeof op.target?.id === "string").map(op => op.target.id));
  const withCascades = (batch) => {
    const base = JSON.stringify(view());
    const announced = new Set();
    const extra = [];
    let deleted = [...new Set(deletedIn(batch.entries))];
    for (let round = 0; round < 8 && deleted.length; round++) {
      deleted.forEach(id => announced.add(id));
      const next = [];
      for (const [id, module] of modules) {
        if (typeof module.cascade !== "function") continue;
        const plan = envelope(module.cascade(JSON.stringify(deleted), base), `${id}.cascade`);
        const ops = (plan.ops || []).filter(op => !(op.op === "delete" && announced.has(op.target?.id)));
        if (!ops.length) continue;
        extra.push({capability: id, plan: {...plan, ops}});
        next.push(...deletedIn([{plan: {ops}}]));
      }
      deleted = [...new Set(next)].filter(id => !announced.has(id));
    }
    return extra.length ? {...batch, entries: [...batch.entries, ...extra]} : batch;
  };
  const commit = (batch) => {
    requireReady();
    const full = withCascades(batch);
    const result = JSON.parse(replica.apply_batch(JSON.stringify(full)));
    flush(); void activateForCurrentView(); scheduleRender(); notify();
    for (const entry of full.entries) resolveProblem(entry.capability, "edit");
    return result;
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
    apply(id, plan) { return commit({base_revision: replica.revision(), entries: [{capability: id, plan}]}); },
    applyBatch(batch) { return commit(batch); },
    hasCapability: (id) => descriptors.some(d => d.id === id),
    activate: (id) => runtime?.activate(id),
    activateForKind: (kind) => runtime?.activateForKind(kind),
    activateForView: (selectedView = view()) => runtime?.activateForView(selectedView),
    activateForTool: (tool) => runtime?.activateForTool(tool),
    activateForRoute: (route) => runtime?.activateForRoute(route),
    // An action's next success clears the problem it left behind.
    // `background: true` marks a call a timer made rather than a person: it
    // never rejoins, so an idle page keeps whatever identity it has.
    dispatch: async (name, args, {background = false} = {}) => {
      let result;
      try { result = await client.action(name, args); }
      catch (error) {
        // The resume cookie outlives the last request by a day. A tab that
        // slept longer wakes with its socket open and its identity gone, and
        // the host refuses the action before doing anything with it. Rejoin
        // once, on this person's own action and never from a timer, then
        // retry the same call.
        if (background || await stillJoined()) throw error;
        await rejoinAsNewPerson();
        result = await client.action(name, args);
      }
      resolveAction(name); return result;
    },
    problem: (capability, action, error) => reportProblem(capability, action, error),
    resolved: (capability, action) => resolveProblem(capability, action),
    toast: (message, tone) => toast(message, tone),
    openPanel: (tab) => document.dispatchEvent(new CustomEvent("samepage:panel-open", {detail: {tab}})),
    attachRuntime(value) { if (runtime) throw new Error("kernel runtime already attached"); runtime = value; notify(); legend.refresh(); },
    connect,
    setPersistenceFailure(message) { persistenceState = persistenceFailed(persistenceState, message); showSyncLoss(persistencePresentation(persistenceState).banner); notify(); },
    setPersistencePending(message) { persistenceState = persistencePending(persistenceState, message); showSyncLoss(persistencePresentation(persistenceState).banner); notify(); },
    setPersistenceResolved() { persistenceState = persistenceResolved(persistenceState); if (!persistenceState.failure && sync === "ready") clearSyncLoss(); notify(); },
    // Read-only diagnostics for real cross-peer verification.
    diagnostics: () => ({ renderedRevision, observationStream, clientId: replica.client_id(), revision: replica.revision(), status: state(), view: view() }),
  });
  return api;
}

// ---- Shell controller: panel tabs, dock drawer, header, rail. Capabilities
// render inside these mounts; only the person's own action opens a panel.
const body = document.body;
const remember = (key, value) => { try { if (value === undefined) return localStorage.getItem(key); if (value === null) localStorage.removeItem(key); else localStorage.setItem(key, value); } catch { return null; } };
const tabActivation = {
  review: () => kernel.activate("review"),
  board: () => kernel.activate("board"),
  sources: () => kernel.activateForRoute("sources"),
  panes: () => kernel.activateForRoute("panes"),
  cement: () => kernel.activateForRoute("cement"),
};
function tabButton(tab) { return document.querySelector(`.tab[data-tab="${tab}"]`); }
async function openTab(tab, {activate = true} = {}) {
  if (!tabButton(tab) || tabButton(tab).hidden) return;
  body.dataset.panel = "open"; body.dataset.tab = tab;
  for (const button of document.querySelectorAll(".tab[data-tab]")) button.setAttribute("aria-selected", String(button.dataset.tab === tab));
  for (const section of document.querySelectorAll(".tab-body[data-tab]")) section.hidden = section.dataset.tab !== tab;
  tabButton(tab).dataset.unread = "false";
  if (!activate) return;
  const section = document.querySelector(`.tab-body[data-tab="${tab}"]`);
  const pending = setTimeout(() => { tabButton(tab).dataset.loading = "true"; section.dataset.loading = "true"; }, 120);
  try { await tabActivation[tab]?.(); resolveProblem(tab, "load"); }
  catch (error) { reportProblem(tab, "load", `${tab} is unavailable: ${error.message}`); }
  finally { clearTimeout(pending); delete tabButton(tab).dataset.loading; delete section.dataset.loading; }
  if (tab === "sources") document.dispatchEvent(new CustomEvent("samepage:sources-open", {detail: {}}));
  if (tab === "cement") document.dispatchEvent(new CustomEvent("samepage:cement-open"));
}
function closePanel() {
  body.dataset.panel = "closed"; delete body.dataset.tab;
  for (const button of document.querySelectorAll(".tab[data-tab]")) button.setAttribute("aria-selected", "false");
  for (const section of document.querySelectorAll(".tab-body[data-tab]")) section.hidden = true;
}
function markUnread(tab) { const button = tabButton(tab); if (button && body.dataset.tab !== tab) button.dataset.unread = "true"; }

const drawerTitles = {lanes: "Found in source", explain: "Walkthrough"};
function openDrawer(name) {
  const drawer = document.getElementById("dock-drawer");
  drawer.hidden = false; drawer.dataset.drawer = name;
  document.getElementById("dock-drawer-title").textContent = drawerTitles[name] || name;
  for (const section of drawer.querySelectorAll(".drawer-body")) section.hidden = section.dataset.drawer !== name;
  for (const segment of document.querySelectorAll(".dock-segment[data-drawer]")) segment.setAttribute("aria-expanded", String(segment.dataset.drawer === name));
}
function closeDrawer() {
  document.getElementById("dock-drawer").hidden = true;
  for (const segment of document.querySelectorAll(".dock-segment[data-drawer]")) segment.setAttribute("aria-expanded", "false");
}
function toggleDrawer(name) {
  const drawer = document.getElementById("dock-drawer");
  if (!drawer.hidden && drawer.dataset.drawer === name) closeDrawer(); else openDrawer(name);
}

function wireChrome() {
  for (const button of document.querySelectorAll(".tab[data-tab]")) {
    button.hidden = !kernel.hasCapability(button.dataset.tab);
    button.addEventListener("click", () => {
      if (body.dataset.panel === "open" && body.dataset.tab === button.dataset.tab) closePanel();
      else void openTab(button.dataset.tab);
    });
  }
  document.getElementById("panel-close").addEventListener("click", closePanel);
  document.addEventListener("samepage:panel-open", event => void openTab(event.detail?.tab));
  document.addEventListener("samepage:review-open", () => void openTab("review", {activate: false}));
  document.addEventListener("samepage:toast", event => toast(event.detail?.text, event.detail?.tone));
  document.getElementById("dock-drawer-close").addEventListener("click", closeDrawer);

  // Overflow menu, legend popover, add-card popover: open on click, close on
  // Escape or a click elsewhere.
  const popovers = [
    ["overflow-toggle", "overflow-menu"],
    ["legend-toggle", "legend"],
    ["add-card-open", "add-card"],
  ];
  const setPopover = (toggleId, panelId, open) => {
    const panel = document.getElementById(panelId), toggle = document.getElementById(toggleId);
    if (!panel || !toggle) return;
    panel.hidden = !open; toggle.setAttribute("aria-expanded", String(open));
    if (open && panelId === "add-card") document.getElementById("new-card-label").focus();
    if (open && panelId === "legend") document.dispatchEvent(new CustomEvent("samepage:legend-open"));
  };
  for (const [toggleId, panelId] of popovers) {
    document.getElementById(toggleId)?.addEventListener("click", event => {
      event.stopPropagation();
      setPopover(toggleId, panelId, document.getElementById(panelId).hidden);
    });
  }
  document.getElementById("legend-close").addEventListener("click", () => setPopover("legend-toggle", "legend", false));
  document.addEventListener("pointerdown", event => {
    for (const [toggleId, panelId] of popovers) {
      const panel = document.getElementById(panelId);
      if (panel.hidden || panel.contains(event.target) || document.getElementById(toggleId).contains(event.target)) continue;
      if (panelId === "legend") continue; // the legend stays until closed; it is read beside the map
      setPopover(toggleId, panelId, false);
    }
    const inbox = document.getElementById("inbox");
    if (!inbox.hidden && !inbox.contains(event.target) && !document.getElementById("inbox-toggle").contains(event.target)) {
      inbox.hidden = true; document.getElementById("inbox-toggle").setAttribute("aria-expanded", "false");
    }
  });
  document.addEventListener("keydown", event => {
    if (event.key !== "Escape") return;
    for (const [toggleId, panelId] of popovers) if (!document.getElementById(panelId).hidden) setPopover(toggleId, panelId, false);
    document.getElementById("inbox").hidden = true;
  });
  document.getElementById("overflow-menu").addEventListener("click", event => {
    if (event.target.closest("button:not(#drawing-io button)")) setPopover("overflow-toggle", "overflow-menu", false);
  });
  const debug = document.getElementById("debug-toggle");
  debug.checked = remember("samepage.debug") === "1";
  body.dataset.debug = String(debug.checked);
  debug.addEventListener("change", () => { body.dataset.debug = String(debug.checked); remember("samepage.debug", debug.checked ? "1" : null); });
}

// The shared mode lives in the cement capability's record. The switch shows
// it; only the person's own click opens the Cement tab or the walkthrough.
// The view mode (explain, map, cement) is the record's "view"; its "value" is
// the cement posture. A record written before the split held the view mode in
// "value", so that is read when it names a view mode.
const VIEW_MODES = ["explain", "map", "cement"];
function currentMode() {
  const props = kernel.view(["cement-mode"]).records.find(r => r.kind === "cement-mode")?.props || {};
  if (VIEW_MODES.includes(props.view)) return props.view;
  if (VIEW_MODES.includes(props.value)) return props.value;
  return "map";
}
const viewMode = (mode) => mode === "explain" ? "explain" : mode === "cement" ? "cement" : "map";
function wireModes() {
  if (!kernel.hasCapability("cement")) return;
  const host = document.getElementById("mode-switch");
  const modes = [["explain", "Explain"], ["map", "Map"], ["cement", "Cement"]].filter(([id]) => id !== "explain" || kernel.hasCapability("explain"));
  for (const [id, label] of modes) {
    const button = document.createElement("button"); button.type = "button"; button.dataset.mode = id; button.textContent = label;
    button.addEventListener("click", async () => {
      const shared = currentMode();
      try {
        if (viewMode(shared) !== id) {
          button.disabled = true;
          await kernel.dispatch("mode_set", {mode: id});
        }
        if (id === "cement") { await openTab("cement"); openLanes(); }
        if (id === "explain" && !document.getElementById("explain-open").hidden) openDrawer("explain");
        if (id === "map") closeDrawer();
      } catch (error) { reportProblem("cement", "mode_set", `Mode not changed: ${error.message}`); }
      finally { button.disabled = false; }
    });
    host.append(button);
  }
  host.hidden = false;
  const reflect = () => {
    const mode = viewMode(currentMode());
    body.dataset.mode = mode;
    for (const button of host.querySelectorAll("button")) button.setAttribute("aria-pressed", String(button.dataset.mode === mode));
    const summary = document.getElementById("cement-summary");
    summary.hidden = mode !== "cement";
  };
  kernel.subscribe(reflect);
  document.getElementById("cement-summary").addEventListener("click", () => void openTab("cement"));
}

async function openLanes() {
  try { await kernel.activateForRoute("lanes"); openDrawer("lanes"); document.dispatchEvent(new CustomEvent("samepage:lanes-open")); resolveProblem("lanes", "load"); }
  catch (error) { reportProblem("lanes", "load", `Found in source is unavailable: ${error.message}`); }
}

function wireDock() {
  const lanes = document.getElementById("lanes-open");
  if (kernel.hasCapability("lanes")) {
    lanes.hidden = false;
    lanes.addEventListener("click", () => {
      const drawer = document.getElementById("dock-drawer");
      if (!drawer.hidden && drawer.dataset.drawer === "lanes") closeDrawer(); else void openLanes();
    });
  }
  const explain = document.getElementById("explain");
  const explainOpen = document.getElementById("explain-open");
  explainOpen.addEventListener("click", () => toggleDrawer("explain"));
  const follow = () => {
    explainOpen.hidden = explain.hidden;
    if (!explain.hidden) {
      const title = explain.querySelector("#explain-flow")?.selectedOptions?.[0]?.textContent;
      const status = explain.dataset.status;
      explainOpen.textContent = `Walkthrough${title ? `: ${title}` : ""}${status ? ` · ${status}` : ""}`;
      explainOpen.dataset.attention = String(status === "active" && document.getElementById("dock-drawer").hidden);
    } else if (document.getElementById("dock-drawer").dataset.drawer === "explain") closeDrawer();
  };
  new MutationObserver(follow).observe(explain, {attributes: true, attributeFilter: ["hidden", "data-status"]});
  follow();
}

// Presence in the header: "you", other people by name, and one chip per
// agent client however many sessions it opened.
function wirePresence(person) {
  const tray = document.getElementById("presence");
  let latest = null;
  const base = (label) => String(label || "").replace(/-\d+$/, "");
  const render = () => {
    const entries = latest?.participants || [];
    const chips = [];
    const me = {kind: "human", label: "you", active: true, me: true, count: 1};
    chips.push(me);
    const groups = new Map();
    for (const entry of entries) {
      const participant = entry.participant || {};
      if (participant.id === person.id) continue;
      const kind = participant.kind === "agent" ? "agent" : "human";
      const label = kind === "agent" ? base(participant.label) || "agent" : participant.label || "someone";
      const key = `${kind}:${label}`;
      const group = groups.get(key) || {kind, label, active: false, count: 0};
      group.active ||= Boolean(entry.active); group.count += 1; groups.set(key, group);
    }
    chips.push(...[...groups.values()].sort((a, b) => a.kind.localeCompare(b.kind) || a.label.localeCompare(b.label)));
    tray.replaceChildren(...chips.map(chip => {
      const node = document.createElement("span"); node.className = "presence-chip";
      node.dataset.kind = chip.kind; node.dataset.active = String(chip.active); node.dataset.me = String(Boolean(chip.me));
      // Agents have a dashed chip; the tooltip says how many connections it holds.
      node.textContent = chip.label;
      node.title = chip.me ? `You are ${person.name || "someone"}` : chip.kind === "agent" ? `Agent ${chip.label}: ${chip.count} connection${chip.count === 1 ? "" : "s"}` : `${chip.label} is here`;
      return node;
    }));
  };
  client.on("surface.semantic-targets", (event) => { latest = event.value || event.detail || latest; render(); });
  const announce = () => fetch("/semantic-targets/presence", {method: "POST", headers: {"content-type": "application/json"}, body: JSON.stringify({participantId: person.id, participantLabel: person.name || "someone"})})
    .then(response => response.ok ? response.json() : null).then(value => { if (value?.snapshot) { latest = value.snapshot; render(); } }).catch(() => {});
  render();
  void fetch("/semantic-targets").then(response => response.ok ? response.json() : null).then(value => { if (value?.snapshot) { latest = value.snapshot; render(); } }).catch(() => {});
  void announce();
  setInterval(announce, 20000);
  return {rename(name) { person.name = name; void announce(); render(); }};
}

// A first visit asks for a name inline. Saving never reloads the page.
function wireNamePrompt(person, presence) {
  const form = document.getElementById("name-prompt");
  const input = document.getElementById("name-prompt-input");
  const taken = document.getElementById("name-prompt-error");
  input.addEventListener("input", () => { taken.hidden = true; input.removeAttribute("aria-invalid"); });
  const defaultName = /^someone(-\d+)?$/i.test(person.name || "someone");
  const show = () => { form.hidden = false; input.value = defaultName ? "" : (person.name || ""); input.focus(); };
  if (defaultName && remember("samepage.name-skipped") !== "1") form.hidden = false;
  document.getElementById("rename-open").addEventListener("click", show);
  document.getElementById("name-prompt-skip").addEventListener("click", () => { form.hidden = true; remember("samepage.name-skipped", "1"); });
  form.addEventListener("submit", async event => {
    event.preventDefault();
    try {
      const response = await fetch("/surface/me", {method: "POST", headers: {"content-type": "application/json"}, body: JSON.stringify({name: input.value.trim()})});
      // A name someone else already uses is refused; say why beside the field and keep it open.
      if (response.status === 409) {
        const body = await response.json().catch(() => ({}));
        taken.textContent = body.message || `"${input.value.trim()}" is already used on this page; choose another name.`;
        taken.hidden = false; input.setAttribute("aria-invalid", "true"); input.focus(); input.select();
        return;
      }
      if (!response.ok) throw new Error(`the name was refused (${response.status})`);
      const updated = await response.json();
      taken.hidden = true; input.removeAttribute("aria-invalid");
      rememberMe(updated);
      Object.assign(person, updated);
      presence.rename(updated.name);
      form.hidden = true;
      toast(`Others now see you as ${updated.name}.`);
      document.dispatchEvent(new CustomEvent("samepage:identity", {detail: updated}));
    } catch (error) { reportProblem("page", "name", error); }
  });
}

// The inbox lists what waits for this person: answers to their questions,
// agent claims without a verdict, board activity since they last looked and
// undecided architecture proposals. Opening an item marks it seen.
function wireInbox(person) {
  const toggle = document.getElementById("inbox-toggle");
  const box = document.getElementById("inbox");
  const countNode = document.getElementById("inbox-count");
  const seen = new Set(JSON.parse(remember("samepage.inbox-seen") || "[]"));
  const markSeen = (key) => { seen.add(key); remember("samepage.inbox-seen", JSON.stringify([...seen].slice(-400))); render(); };
  let board = null;
  const refreshBoard = () => fetch("/board/state").then(r => r.ok ? r.json() : null).then(v => { board = v?.board || null; render(); }).catch(() => {});
  const items = () => {
    const records = kernel.view(["node", "mark", "claim", "architecture-proposal"]).records;
    const label = (id) => records.find(r => r.id === id)?.props?.label || "a card";
    const list = [];
    for (const mark of records.filter(r => r.kind === "mark" && r.props.answer && r.props.created_by_participant === person.id)) {
      list.push({key: `answer:${mark.id}:${mark.props.answer}`, title: `${label(mark.props.target)}: your question was answered`, detail: `${mark.props.touched_by_participant === person.id ? "you" : String(mark.props.touched_by || "someone").replace(/-\d+$/, "")}: ${mark.props.answer}`, open: () => { document.dispatchEvent(new CustomEvent("samepage:review-open", {detail: {id: mark.props.target}})); document.dispatchEvent(new CustomEvent("samepage:map-focus", {detail: {id: mark.props.target}})); }, clears: true});
    }
    for (const claim of records.filter(r => r.kind === "claim" && !r.props.withdrawn && !["accepted", "rejected"].includes(r.props.verdict))) {
      list.push({key: `claim:${claim.id}`, title: `${label(claim.props.about)}: a claim waits for your verdict`, detail: `${String(claim.props.created_by || "someone").replace(/-\d+$/, "")}: ${claim.props.text || ""}`, open: () => { void kernel.activate("review").then(() => document.dispatchEvent(new CustomEvent("samepage:review-open", {detail: {id: claim.props.about}}))); document.dispatchEvent(new CustomEvent("samepage:map-focus", {detail: {id: claim.props.about}})); }});
    }
    const proposals = records.filter(r => r.kind === "architecture-proposal");
    const superseded = new Set(proposals.map(r => { try { return JSON.parse(r.props.definition || "{}").previous; } catch { return null; } }).filter(Boolean));
    for (const proposal of proposals.filter(r => !r.props.verdict && !superseded.has(r.id))) {
      let title = "Architecture proposal"; try { title = JSON.parse(proposal.props.definition).title || title; } catch {}
      list.push({key: `proposal:${proposal.id}`, title: `Proposal waits for your decision`, detail: title, open: () => document.getElementById("architecture-open")?.click()});
    }
    const lastSeen = Number(remember("samepage.board-seen") || 0);
    if (board && board.revision > lastSeen && (board.entries?.length || board.subject)) {
      list.push({key: `board:${board.revision}`, title: "Board changed since you looked", detail: board.subject || `${board.entries.length} items`, open: () => { remember("samepage.board-seen", String(board.revision)); void openTab("board"); }, clears: true});
    }
    return list;
  };
  function render() {
    if (!kernel) return;
    const list = items();
    const unread = list.filter(item => !(item.clears && seen.has(item.key)));
    countNode.textContent = String(unread.length);
    toggle.dataset.unread = String(unread.length > 0);
    toggle.title = unread.length ? `${unread.length} waiting for you` : "Nothing waiting for you";
    if (box.hidden) return;
    const heading = document.createElement("h2"); heading.textContent = unread.length ? `Waiting for you · ${unread.length}` : "Nothing waiting for you";
    const ul = document.createElement("ul");
    for (const item of list) {
      const li = document.createElement("li"); li.dataset.seen = String(Boolean(item.clears && seen.has(item.key)));
      const button = document.createElement("button"); button.type = "button";
      const detail = document.createElement("small"); detail.textContent = item.detail;
      button.append(document.createTextNode(item.title), detail);
      button.addEventListener("click", () => { box.hidden = true; toggle.setAttribute("aria-expanded", "false"); if (item.clears) markSeen(item.key); item.open(); });
      li.append(button); ul.append(li);
    }
    box.replaceChildren(heading, ...(list.length ? [ul] : [Object.assign(document.createElement("p"), {className: "inbox-empty", textContent: "Answers to your questions, claims for your verdict and board changes appear here."})]));
  }
  toggle.addEventListener("click", (event) => { event.stopPropagation(); box.hidden = !box.hidden; toggle.setAttribute("aria-expanded", String(!box.hidden)); render(); });
  kernel.subscribe(render);
  client.on("board.changed", () => void refreshBoard());
  document.addEventListener("samepage:panel-open", (event) => { if (event.detail?.tab === "board" && board) { remember("samepage.board-seen", String(board.revision)); render(); } });
  for (const tab of document.querySelectorAll('.tab[data-tab="board"]')) tab.addEventListener("click", () => { if (board) { remember("samepage.board-seen", String(board.revision)); render(); } });
  void refreshBoard();
}

// The project this page maps, measured by the host from the project root:
// "orders · 7 files · Rust", or the name and why it could not be measured.
async function wireProject() {
  const node = document.getElementById("project");
  try {
    const response = await fetch("/surface/project");
    if (!response.ok) return;
    const project = await response.json();
    if (!project?.name) return;
    if (project.state === "unavailable" || project.files === null || project.files === undefined) {
      node.textContent = `${project.name} · ${String(project.reason || "not measured").replace(/\.$/, "")}`;
      node.title = project.reason ? `${project.name}: ${project.reason}` : `${project.name} could not be measured`;
      node.dataset.state = "unavailable";
      return;
    }
    const languages = (project.languages || []).slice(0, 2).map((language) => language.name).filter(Boolean);
    node.textContent = [project.name, `${project.files} file${project.files === 1 ? "" : "s"}`, ...languages].join(" · ");
    node.title = [project.branch && `branch ${project.branch}`, project.revision && `at ${project.revision}`, project.boundary].filter(Boolean).join(" · ");
    node.dataset.state = "measured";
  } catch { /* the header keeps "Shared map" */ }
}

const railTools = [["select", "Select", "V", "↖"], ["rectangle", "Rectangle", "R", "▭"], ["ellipse", "Ellipse", "O", "◯"], ["arrow", "Arrow", "A", "→"], ["pen", "Pen", "P", "✎"], ["text", "Text", "T", "T"]];
function wireRail() {
  const rail = document.getElementById("tool-rail");
  if (kernel.hasCapability("sketch")) {
    const separator = document.createElement("span"); separator.className = "rail-sep"; separator.setAttribute("aria-hidden", "true");
    rail.prepend(separator);
    for (const [tool, label, key, glyph] of [...railTools].reverse()) {
      const button = document.createElement("button");
      button.type = "button"; button.className = "rail-button"; button.dataset.sketchTool = tool; button.dataset.key = key;
      button.title = `${label} (${key})`; button.setAttribute("aria-label", label); button.setAttribute("aria-pressed", String(tool === "select"));
      const icon = document.createElement("span"); icon.textContent = glyph; icon.setAttribute("aria-hidden", "true");
      const hint = document.createElement("kbd"); hint.textContent = key;
      button.append(icon, hint);
      button.addEventListener("click", async () => {
        try {
          await kernel.activateForTool(`sketch-${tool}`);
          document.dispatchEvent(new CustomEvent("samepage:sketch-tool", {detail: {tool}}));
          resolveProblem("sketch", "load");
        } catch (error) { reportProblem("sketch", "load", `Drawing tools are unavailable: ${error.message}`); }
      });
      rail.prepend(button);
    }
  }
  document.addEventListener("keydown", event => {
    if (event.metaKey || event.ctrlKey || event.altKey || event.defaultPrevented) return;
    if (event.target.closest?.("input, textarea, select, [contenteditable=''], [contenteditable='true'], dialog")) return;
    const key = event.key.toUpperCase();
    const button = rail.querySelector(`[data-key="${key}"]`) || (key === "C" ? document.getElementById("add-card-open") : null);
    if (!button) return;
    event.preventDefault();
    button.click();
  });
}

function wireActivity() {
  const health = async () => {
    const drawer = document.getElementById("activity-drawer");
    if (!drawer || drawer.hidden) return;
    const response = await fetch("/extensions");
    if (!response.ok) return;
    const activity = (await response.json()).activity;
    let line = document.getElementById("trace-health");
    if (!line) { line = document.createElement("span"); line.id = "trace-health"; line.setAttribute("role", "status"); drawer.querySelector("header")?.append(line); }
    line.textContent = `${(activity?.dropped || 0) + (globalThis.__samepageTraceDropped || 0)} events not recorded`;
  };
  setInterval(() => void health().catch(() => {}), 2000);
  document.getElementById("activity-toggle")?.addEventListener("click", async () => {
    try {
      if (!kernel.hasCapability("trace")) throw new Error("activity recording was not selected for this page");
      const loaded = document.getElementById("activity-drawer");
      const open = loaded?.hidden ?? true;
      await kernel.activateForRoute("activity");
      if (loaded) document.dispatchEvent(new CustomEvent("samepage:trace-open", {detail: {open}}));
      void health();
      resolveProblem("trace", "load");
    } catch (error) { reportProblem("trace", "load", `Activity is unavailable: ${error.message}`); }
  });
}

try {
  const person = await whoAmI();
  if (!person) throw new Error("host identity join failed; document edits require a joined person");
  const runtime = await loadExtensions(client, { createApplicationContext: (parts) => {
    kernel = createKernel(parts);
    return kernel;
  } });
  kernel.attachRuntime(runtime);
  // "you" is decided by participant id, as the host reports it for this browser.
  const confirmed = await fetch("/surface/me", {credentials: "same-origin"}).then(r => r.ok ? r.json() : null).catch(() => null);
  if (confirmed?.joined && confirmed.id) Object.assign(person, confirmed);
  globalThis.samepagePerson = person;
  client.on("samepage.capability-used",async ({value})=>{
    try {
      if (value?.tool) await kernel.activateForTool(value.tool);
      else if (kernel.hasCapability(value?.id)) await kernel.activate(value.id);
    } catch(error){ reportProblem(value?.id || "page", "load", `${value?.id || "A capability"} is unavailable: ${error.message}`); }
  });
  client.on("samepage.persistence-error", ({value}) => kernel.setPersistenceFailure(value?.error || "save failed"));
  client.on("samepage.persistence-wait", ({value}) => kernel.setPersistencePending(value?.message || "save is still pending"));
  client.on("samepage.persistence-resolved", () => kernel.setPersistenceResolved());
  // The shared question in the header: the review scope question when one is
  // set, otherwise the board's subject.
  const question = {review: null, board: null};
  const showQuestion = () => {
    const node = document.getElementById("shared-question");
    const text = question.review || question.board;
    node.hidden = !text;
    const label = document.createElement("strong"); label.textContent = "Q";
    node.replaceChildren(label, document.createTextNode(text || ""));
    node.title = text || "";
  };
  document.addEventListener("samepage:shared-question", event => { question.review = event.detail?.question || null; showQuestion(); });
  void fetch("/board/state").then(response => response.ok ? response.json() : null).then(value => { question.board = value?.board?.subject || null; showQuestion(); }).catch(() => {});
  // A board change marks its tab; it never opens the panel by itself.
  client.on("board.changed",({value})=>{
    question.board = value?.board?.subject || null; showQuestion();
    if (value?.board?.entries?.length || value?.board?.subject) markUnread("board");
  });
  wireChrome();
  wireRail();
  const presence = wirePresence(person);
  wireNamePrompt(person, presence);
  await client.connect();
  kernel.connect();
  wireModes();
  wireDock();
  wireInbox(person);
  void wireProject();
  wireActivity();
  document.getElementById("add-card").addEventListener("submit", async (event) => {
    event.preventDefault();
    const input = document.getElementById("new-card-label");
    try {
      await kernel.dispatch("card_place", {label:input.value.trim()});
      toast(`Added card "${input.value.trim()}".`);
      input.value = "";
      document.getElementById("add-card").hidden = true;
      document.getElementById("add-card-open").setAttribute("aria-expanded", "false");
    } catch (error) { reportProblem("map", "card_place", `Card was not added: ${error.message}`); }
  });
  document.getElementById("architecture-open")?.addEventListener("click", async () => {
    try {
      if (!kernel.hasCapability("architecture")) throw new Error("architecture capability is unavailable");
      await kernel.activateForTool("architecture_read");
      document.dispatchEvent(new CustomEvent("samepage:architecture-open"));
      resolveProblem("architecture", "load");
    } catch (error) { reportProblem("architecture", "load", `Architecture unavailable: ${error.message}`); }
  });
  // Exposes observations without a second document or mutation channel.
  globalThis.samepageReadback = () => kernel.diagnostics();
} catch (error) {
  setStatus("Unavailable");
  showSyncLoss(`SamePage could not start: ${error.message}`);
  console.error(error);
}
