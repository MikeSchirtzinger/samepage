import { wasmSpans, hostSpans, layoutSpans } from "/activity-model.js";
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const duration = ms => ms < 1 ? `${(ms * 1000).toFixed(0)} µs` : ms < 1000 ? `${ms.toFixed(2)} ms` : `${(ms / 1000).toFixed(2)} s`;

export function installActivityDrawer(client) {
  const toggle = document.getElementById("activity-toggle");
  const drawer = el("section", "activity-drawer");
  drawer.id = "activity-drawer";
  drawer.hidden = true;
  drawer.setAttribute("aria-label", "Page activity and tracing");
  const header = el("header", "activity-header");
  const status = el("span", "activity-status", "Connecting to host journal…");
  status.setAttribute("role", "status");
  const controls = el("div", "activity-controls");
  const pause = el("button", null, "Pause view");
  const refresh = el("button", null, "Refresh");
  const download = el("button", null, "Export trace");
  const close = el("button", null, "Close");
  for (const button of [pause, refresh, download, close]) button.type = "button";
  controls.append(pause, refresh, download, close);
  header.append(el("h2", null, "Page activity"), status, controls);
  const tabs = el("div", "activity-tabs");
  const tablist = el("div", "activity-controls");
  tablist.setAttribute("role", "tablist");
  tablist.setAttribute("aria-label", "Activity view");
  const filter = el("input", "activity-filter");
  filter.type = "search";
  filter.placeholder = "Filter actions or actors…";
  filter.setAttribute("aria-label", "Filter activity");
  const category = el("select", "activity-category");
  category.setAttribute("aria-label", "Event type");
  for (const [value, label] of [["action", "Actions"], ["all", "All events"], ["utterance", "Messages"], ["connection", "Connections"]]) {
    const option = el("option", null, label); option.value = value; category.append(option);
  }
  tabs.append(tablist, category, filter);
  const body = el("div", "activity-body");
  body.id = "activity-body";
  body.setAttribute("role", "tabpanel");
  drawer.append(header, tabs, body);
  document.body.append(drawer);
  let active = "journal", snapshot = null, wasm = null, paused = false, request = null;
  let lastSignature = "", error = null, selectedSpan = null;
  let selectedTrace = "latest", visibleEventCount = 40;
  const expandedEvents = new Set();
  const names = { journal: "Timeline", host: "Action flame graph", wasm: "WASM flame graph", coverage: "Coverage" };
  const buttons = Object.entries(names).map(([id, text]) => {
    const button = el("button", null, text);
    button.type = "button";
    button.id = `activity-tab-${id}`;
    button.setAttribute("role", "tab");
    button.setAttribute("aria-controls", "activity-body");
    button.addEventListener("click", () => { active = id; lastSignature = ""; selectedSpan = null; selectedTrace = "latest"; visibleEventCount = 40; render(); });
    tablist.append(button);
    return button;
  });
  tablist.addEventListener("keydown", event => {
    const index = buttons.indexOf(document.activeElement);
    const next = event.key === "ArrowRight" ? (index + 1) % buttons.length : event.key === "ArrowLeft" ? (index + buttons.length - 1) % buttons.length : event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : null;
    if (next === null) return;
    event.preventDefault(); buttons[next].click(); buttons[next].focus();
  });
  const readWasm = () => {
    try { wasm = globalThis.__samePageAtlasTrace?.snapshot() || null; }
    catch { wasm = null; }
  };
  const matches = text => String(text).toLocaleLowerCase().includes(filter.value.trim().toLocaleLowerCase());
  const render = (preserveInteraction = false) => {
    if (drawer.hidden) return;
    for (const [index, id] of Object.keys(names).entries()) {
      buttons[index].setAttribute("aria-selected", String(active === id));
      buttons[index].tabIndex = active === id ? 0 : -1;
    }
    body.setAttribute("aria-labelledby", `activity-tab-${active}`);
    category.hidden = active !== "journal";
    const events = snapshot?.events || [];
    status.textContent = error ? `Host journal unavailable: ${error}` : `${paused ? "View paused" : "Live"} · ${events.length} retained events${snapshot?.durability?.status === "degraded" ? " · storage degraded" : ""}`;
    const signature = JSON.stringify([active, filter.value, category.value, events.at(-1)?.sequence, snapshot?.durability, active === "wasm" || active === "coverage" ? wasm?.emitted : null, selectedTrace, visibleEventCount, error]);
    if (signature === lastSignature) return;
    // Keep controls and expanded evidence stable while the reader is using them.
    if (preserveInteraction && body.contains(document.activeElement) && document.activeElement.matches("select, button, summary")) return;
    lastSignature = signature;
    const scroll = body.scrollTop;
    body.replaceChildren();
    if (active === "journal") {
      body.append(el("p", "activity-note", "Host-observed events, newest first. Human and agent actions retain their caller, outcome, and state revisions."));
      if (!events.length) body.append(el("p", "activity-note", error ? "No host evidence is available. Refresh to retry." : "No retained events yet. An empty journal does not establish inactivity."));
      const matchingEvents = [...events].reverse().filter(e => (category.value === "all" || e.category === category.value) && matches(`${e.kind} ${e.actor.label} ${e.outcome}`));
      if (events.length && !matchingEvents.length) body.append(el("p", "activity-note", "No events match this filter."));
      for (const event of matchingEvents.slice(0, visibleEventCount)) {
        const row = el("article", "activity-event");
        row.dataset.outcome = event.outcome;
        row.dataset.sequence = event.sequence;
        const time = el("time", null, new Date(event.atMs).toLocaleTimeString([], { hour12: false }));
        time.dateTime = new Date(event.atMs).toISOString();
        row.append(time, el("span", "activity-event-actor", event.actor.label), el("span", "activity-event-name", event.kind.replace(/^action\./, "")), el("span", "activity-event-outcome", event.outcome));
        const details = el("details");
        details.open = expandedEvents.has(event.correlation.eventId);
        details.addEventListener("toggle", () => {
          if (!details.isConnected) return;
          if (details.open) expandedEvents.add(event.correlation.eventId);
          else expandedEvents.delete(event.correlation.eventId);
        });
        details.append(el("summary", null, "Recorded fields"), el("pre", null, JSON.stringify(event, null, 2)));
        if (event.timing) {
          const traceButton = el("button", "activity-open-trace", `${duration(event.timing.durationUs / 1000)} · View trace`);
          traceButton.type = "button";
          traceButton.addEventListener("click", () => {
            active = "host"; selectedTrace = event.correlation.eventId; selectedSpan = null;
            buttons[1].focus(); render();
          });
          row.append(traceButton);
        }
        row.append(details); body.append(row);
      }
      if (matchingEvents.length > visibleEventCount) {
        const more = el("button", "activity-load-more", `Show 40 more (${matchingEvents.length - visibleEventCount} remaining)`);
        more.type = "button";
        more.addEventListener("click", () => { visibleEventCount += 40; more.blur(); render(); body.querySelector(".activity-load-more")?.focus({ preventScroll: true }); });
        body.append(more);
      }
    } else if (active === "coverage") {
      body.append(el("p", "activity-note", snapshot ? `Journal storage: ${snapshot.durability.status}. ${snapshot.durability.replaysAfterRestart ? "Host records replay after restart." : "Restart persistence is not established."} Browser WASM records are local to this tab and are lost on reload.` : "Coverage unavailable until the host journal responds."));
      const grid = el("div", "activity-coverage");
      for (const coverage of snapshot?.coverage || []) {
        const section = el("section");
        section.append(el("h3", null, `${coverage.category} · ${coverage.instrumentation}`), el("p", null, `${coverage.observedEvents} retained. ${coverage.note}`));
        grid.append(section);
      }
      const browser = el("section");
      browser.append(el("h3", null, "Browser WASM · partial"), el("p", null, `${wasm?.records?.length || 0} records retained; ${wasm?.dropped || 0} dropped from the bounded ring. Measured instrumented spans only. Host and browser clocks are separate; no cross-host causal link is inferred.`));
      grid.append(browser); body.append(grid);
    } else {
      const isWasm = active === "wasm";
      const recorded = isWasm ? wasmSpans(wasm) : hostSpans(snapshot);
      const matchingTraces = new Set(recorded.filter(s => matches(`${s.name} ${s.fields?.actor || ""}`)).map(s => s.trace));
      const spans = recorded.filter(s => matchingTraces.has(s.trace));
      const roots = spans.filter(s => !s.parent);
      const defaultRoot = isWasm ? roots.findLast(s => s.name === "atlas.render") || roots.at(-1) : roots.at(-1);
      const chosen = selectedTrace === "latest" ? defaultRoot : roots.find(s => s.id === selectedTrace);
      const visible = chosen ? spans.filter(s => s.trace === chosen.trace) : [];
      if (roots.length) {
        const chooser = el("select", "activity-trace-choice");
        chooser.setAttribute("aria-label", "Choose a recorded trace");
        const latest = el("option", null, isWasm ? (defaultRoot?.name === "atlas.render" ? "Follow latest render" : "Follow latest retained trace") : "Follow latest action");
        latest.value = "latest"; chooser.append(latest);
        for (const root of [...roots].reverse()) {
          const option = el("option", null, `${root.name} · ${duration(root.duration)} · ${new Date(root.start).toLocaleTimeString()}`);
          option.value = root.id; chooser.append(option);
        }
        if (selectedTrace !== "latest" && !chosen) {
          const gone = el("option", null, "Selected trace is no longer retained"); gone.value = selectedTrace; chooser.append(gone);
        }
        chooser.value = selectedTrace;
        chooser.addEventListener("change", () => { selectedTrace = chooser.value; selectedSpan = null; lastSignature = ""; render(); body.querySelector("select")?.focus(); });
        body.append(chooser);
      }
      body.append(el("p", "activity-note", isWasm
        ? `Recorded WASM trace. Bar width is elapsed span time, including children. ${wasm?.dropped || 0} records dropped. This is not a sampled CPU profile.`
        : "Measured dispatcher phases, including waits. Each phase belongs to this action. Host and browser timing are not merged."));
      if (!visible.length) {
        const message = filter.value.trim() && recorded.length
          ? "No recorded traces match this filter. Clear the filter to see retained traces."
          : isWasm ? "No completed WASM spans retained. Interact with the canvas, then refresh."
          : "No measured action durations retained. Earlier host events have outcomes but no timing; they are not assigned invented durations.";
        body.append(el("p", "activity-note", message));
        if (filter.value.trim()) {
          const clear = el("button", "activity-clear-filter", "Clear filter");
          clear.type = "button";
          clear.addEventListener("click", () => { filter.value = ""; render(); filter.focus(); });
          body.append(clear);
        }
      }
      else {
        const layout = layoutSpans(visible);
        const traceHeading = el("div", "activity-trace-heading");
        traceHeading.append(el("strong", null, chosen.name), el("span", null, [chosen.fields?.actor, chosen.fields?.outcome, duration(chosen.duration)].filter(Boolean).join(" · ")));
        body.append(traceHeading);
        const axis = el("p", "activity-note", `${new Date(layout.start).toLocaleTimeString()} · ${duration(layout.duration)} elapsed · ${visible.length} spans`);
        const chart = el("div", "flame-chart");
        chart.setAttribute("aria-label", `${names[active]}: ${duration(layout.duration)} elapsed`);
        chart.style.setProperty("--chart-height", `${Math.max(1, layout.depth) * 32 + 4}px`);
        const detail = el("section", "flame-detail");
        detail.setAttribute("aria-label", "Selected span");
        detail.append(el("p", null, "Select a bar to inspect a measured phase."));
        const showSpan = span => {
          selectedSpan = span.id;
          selectedTrace = chosen.id;
          const chooser = body.querySelector(".activity-trace-choice");
          if (chooser) chooser.value = chosen.id;
          const heading = el("div", "flame-detail-heading");
          heading.append(el("strong", null, span.name), el("span", null, `${duration(span.duration)} elapsed · ${span.source}`));
          const fields = el("details");
          fields.append(el("summary", null, "Recorded fields"), el("pre", null, JSON.stringify(span.fields, null, 2)));
          detail.replaceChildren(heading, fields);
          if (span.partial) detail.append(el("p", "activity-note", "The opening record is no longer retained."));
          for (const bar of chart.querySelectorAll("button")) bar.setAttribute("aria-pressed", String(bar.dataset.spanId === span.id));
        };
        for (const span of layout.spans) {
          const bar = el("button", "flame-span", `${span.name} · ${duration(span.duration)}`);
          bar.type = "button";
          bar.dataset.spanId = span.id;
          bar.title = `${span.name}: ${duration(span.duration)}${span.partial ? " (opening record evicted)" : ""}`;
          bar.setAttribute("aria-label", bar.title);
          bar.style.setProperty("--span-left", `${span.left}%`);
          bar.style.setProperty("--span-width", `${span.width}%`);
          bar.style.setProperty("--span-top", `${span.lane * 32 + 2}px`);
          const select = () => showSpan(span);
          bar.addEventListener("click", select);
          chart.append(bar);
          if (selectedSpan === span.id) select();
        }
        const spanList = el("details", "flame-span-list");
        spanList.append(el("summary", null, `All ${layout.spans.length} measured spans`));
        for (const span of layout.spans) {
          const row = el("button", null, `${span.parent ? "↳ " : ""}${span.name} · ${duration(span.duration)}`);
          row.type = "button";
          row.addEventListener("click", () => {
            showSpan(span);
          });
          spanList.append(row);
        }
        body.append(axis, chart, detail, spanList);
      }
    }
    body.scrollTop = scroll;
  };
  const refreshData = async () => {
    if (request) return request;
    request = (async () => {
      try {
        const response = await fetch("/activity", { signal: AbortSignal.timeout(8000) });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const data = await response.json();
        if (!Array.isArray(data.events) || !data.durability) throw new Error("Invalid activity snapshot");
        snapshot = data; error = null;
      } catch (e) { error = e.message; }
      readWasm(); render(true);
    })().finally(() => { request = null; });
    return request;
  };
  const setOpen = open => {
    drawer.hidden = !open; toggle.setAttribute("aria-expanded", String(open));
    if (open) { void refreshData(); buttons[Object.keys(names).indexOf(active)].focus(); }
    else toggle.focus();
  };
  toggle.addEventListener("click", () => setOpen(drawer.hidden));
  close.addEventListener("click", () => setOpen(false));
  drawer.addEventListener("keydown", event => { if (event.key === "Escape") { event.stopPropagation(); setOpen(false); } });
  pause.addEventListener("click", () => { paused = !paused; pause.textContent = paused ? "Resume view" : "Pause view"; pause.setAttribute("aria-pressed", String(paused)); if (!paused) void refreshData(); else render(); });
  refresh.addEventListener("click", () => { lastSignature = ""; void refreshData(); });
  category.addEventListener("change", () => { visibleEventCount = 40; render(); });
  filter.addEventListener("input", () => { visibleEventCount = 40; render(); });
  download.addEventListener("click", () => {
    const payload = { schema: "same-page-observability-v1", capturedAt: new Date().toISOString(), host: snapshot, browser: wasm, hostError: error, clocksCorrelated: false };
    const url = URL.createObjectURL(new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" }));
    const link = el("a"); link.href = url; link.download = `same-page-trace-${Date.now()}.json`; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  client.on("surface.activity", () => { if (!drawer.hidden && !paused) void refreshData(); });
  // Poll only while the drawer is open. The host journal also catches external MCP activity.
  setInterval(() => { if (!drawer.hidden && !paused && !document.hidden) void refreshData(); }, 2000);
}
