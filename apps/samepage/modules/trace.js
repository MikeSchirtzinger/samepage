// Timing is derived only from recorded spans or explicitly paired host events.
// Missing endpoints remain missing. Separate clocks are never joined by proximity.
export function wasmSpans(snapshot) {
  const records = snapshot?.records || [];
  const opens = new Map(records.filter(r => r.kind === "span_open").map(r => [r.span_id, r]));
  const anchor = records.find(r => r.kind === "span_open" && Number.isFinite(r.started_monotonic_ms));
  const origin = anchor ? anchor.at_ms - anchor.started_monotonic_ms : null;
  return records.filter(r => r.kind === "span_close" && Number.isFinite(r.duration_ms)).map(r => ({
    id: String(r.span_id), parent: r.parent_span_id === null ? null : String(r.parent_span_id),
    name: r.name, start: origin !== null && Number.isFinite(r.started_monotonic_ms)
      ? origin + r.started_monotonic_ms : opens.get(r.span_id)?.at_ms ?? r.at_ms - r.duration_ms,
    duration: r.duration_ms, fields: r.fields, trace: r.trace_id,
    partial: !opens.has(r.span_id), capability:r.capability, source: `Browser WASM · ${r.capability}`,
  }));
}

export function hostSpans(snapshot) {
  return (snapshot?.events || []).filter(e => e.timing && Number.isFinite(e.timing.durationUs)).flatMap(e => {
    const root = {
      id: e.correlation.eventId, parent: null, name: e.kind.replace(/^action\./, ""),
      start: e.timing.startedAtMs, duration: e.timing.durationUs / 1000,
      fields: { actor: e.actor.label, outcome: e.outcome, before: e.stateRevisionBefore, after: e.stateRevisionAfter },
      trace: e.correlation.eventId, run: e.correlation.runId, source: "Host dispatcher", partial: false,
    };
    return [root, ...(e.timing.spans || []).map((span, index) => ({
      ...root, id: `${root.id}:${index}`, parent: root.id, name: span.name,
      start: root.start + span.offsetUs / 1000, duration: span.durationUs / 1000,
    }))];
  });
}

export function layoutSpans(spans) {
  if (!spans.length) return { spans: [], start: 0, duration: 0, depth: 0 };
  const byId = new Map(spans.map(span => [span.id, span]));
  const start = Math.min(...spans.map(s => s.start));
  const end = Math.max(...spans.map(s => s.start + s.duration));
  const duration = Math.max(end - start, 0);
  const scale = duration || 1;
  const lanes = [];
  const placed = new Map();
  const sorted = [...spans].sort((a, b) => a.start - b.start || b.duration - a.duration);
  const place = (span, ancestry = new Set()) => {
    if (placed.has(span.id)) return placed.get(span.id);
    if (ancestry.has(span.id)) return null;
    ancestry.add(span.id);
    const parent = span.parent && byId.get(span.parent);
    const parentPlaced = parent && place(parent, ancestry);
    let lane = parentPlaced ? parentPlaced.lane + 1 : 0;
    while ((lanes[lane] || []).some(s => span.start < s.start + Math.max(s.duration, .001) && span.start + Math.max(span.duration, .001) > s.start)) lane++;
    const result = { ...span, lane, left: (span.start - start) / scale * 100, width: Math.max(span.duration / scale * 100, .15) };
    (lanes[lane] ||= []).push(span);
    placed.set(span.id, result);
    return result;
  };
  sorted.forEach(span => place(span));
  return { spans: [...placed.values()], start, duration, depth: lanes.length };
}

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const duration = ms => ms < 1 ? `${(ms * 1000).toFixed(0)} µs` : ms < 1000 ? `${ms.toFixed(2)} ms` : `${(ms / 1000).toFixed(2)} s`;

// Who acted, in words: the participant's name and what kind of participant it
// is, "you" for this person. An older host sends only "Agent" or "Human".
const ACTOR_KIND = { agent: "agent", human: "person", host: "host" };
function actorText(actor) {
  const label = actor?.label || "unknown";
  if (actor?.participantId && actor.participantId === globalThis.samepagePerson?.id) return "you";
  const kind = ACTOR_KIND[actor?.kind];
  // A label that only names the class ("Agent", "Human") needs no kind beside it.
  return kind && ![kind, actor.kind].includes(label.toLowerCase()) ? `${label} (${kind})` : label;
}

function installActivityDrawer(client, wasm, application) {
  const labelOf = (id) => application?.view?.(["node", "shape", "edge"]).records.find((record) => record.id === id)?.props?.label || id;
  const toggle = document.getElementById("activity-toggle");
  const drawer = el("section", "activity-drawer");
  drawer.id = "activity-drawer";
  drawer.hidden = true;
  drawer.setAttribute("aria-label", "Page activity and tracing");
  const header = el("header", "activity-header");
  const status = el("span", "activity-status", "Connecting to host journal…");
  status.setAttribute("role", "status");
  const controls = el("div", "activity-controls");
  const clear = el("button", null, "Clear spans"); clear.id="trace-clear"; clear.type="button";
  clear.addEventListener("click",()=>{wasm.trace_operation(JSON.stringify({operation:"clear"}));lastSignature="";void refreshData();});
  const benchmark = el("button",null,"Benchmark"); benchmark.id="trace-benchmark"; benchmark.type="button";
  benchmark.addEventListener("click",()=>{const result=JSON.parse(wasm.trace_operation(JSON.stringify({operation:"benchmark",iterations:100,samples:3})));lastSignature="";void refreshData();benchmarkResult.textContent=result.error?result.error.message:`Benchmark: ${result.ok.overhead_us_per_iteration.toFixed(2)} µs / iteration`;});
  controls.append(clear,benchmark);
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
  const benchmarkResult=el("p","activity-note");benchmarkResult.id="trace-benchmark-result";
  drawer.append(header, benchmarkResult, tabs, body);
  document.body.append(drawer);
  let active = "wasm", snapshot = null, browser = null, paused = false, request = null;
  let lastSignature = "", error = null, selectedSpan = null;
  let selectedTrace = "all", visibleEventCount = 40;
  const expandedEvents = new Set();
  const names = { journal: "Timeline", host: "Action flame graph", wasm: "WASM flame graph", coverage: "Coverage" };
  const buttons = Object.entries(names).map(([id, text]) => {
    const button = el("button", null, text);
    button.type = "button";
    button.id = `activity-tab-${id}`;
    button.setAttribute("role", "tab");
    button.setAttribute("aria-controls", "activity-body");
    button.addEventListener("click", () => { active = id; lastSignature = ""; selectedSpan = null; selectedTrace = id === "wasm" ? "all" : "latest"; visibleEventCount = 40; render(); });
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
    try { browser = JSON.parse(wasm.trace_operation(JSON.stringify({operation:"snapshot"}))).ok; }
    catch { browser = null; }
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
    const signature = JSON.stringify([active, filter.value, category.value, events.at(-1)?.sequence, snapshot?.durability, active === "wasm" || active === "coverage" ? browser?.emitted : null, selectedTrace, visibleEventCount, error]);
    if (signature === lastSignature) return;
    // Keep controls and expanded evidence stable while the reader is using them.
    if (preserveInteraction && body.contains(document.activeElement) && document.activeElement.matches("select, button, summary")) return;
    lastSignature = signature;
    const scroll = body.scrollTop;
    body.replaceChildren();
    if (active === "journal") {
      body.append(el("p", "activity-note", "Host-observed events, newest first. Human and agent actions retain their caller, outcome, and state revisions."));
      if (!events.length) body.append(el("p", "activity-note", error ? "No host evidence is available. Refresh to retry." : "No retained events yet. An empty journal does not establish inactivity."));
      const matchingEvents = [...events].reverse().filter(e => (category.value === "all" || e.category === category.value) && matches(`${e.kind} ${actorText(e.actor)} ${e.outcome}`));
      if (events.length && !matchingEvents.length) body.append(el("p", "activity-note", "No events match this filter."));
      for (const event of matchingEvents.slice(0, visibleEventCount)) {
        const row = el("article", "activity-event");
        row.dataset.outcome = event.outcome;
        row.dataset.sequence = event.sequence;
        const time = el("time", null, new Date(event.atMs).toLocaleTimeString([], { hour12: false }));
        time.dateTime = new Date(event.atMs).toISOString();
        row.append(time, el("span", "activity-event-actor", actorText(event.actor)), el("span", "activity-event-name", event.kind.replace(/^action\./, "")), el("span", "activity-event-outcome", event.outcome));
        const details = el("details");
        details.open = expandedEvents.has(event.correlation.eventId);
        details.addEventListener("toggle", () => {
          if (!details.isConnected) return;
          if (details.open) expandedEvents.add(event.correlation.eventId);
          else expandedEvents.delete(event.correlation.eventId);
        });
        details.append(el("summary", null, "Recorded fields"));
        if (Array.isArray(event.targets) && event.targets.length) details.append(el("p", "activity-event-targets", `Acted on: ${event.targets.map(labelOf).join(", ")}`));
        details.append(el("pre", null, JSON.stringify(event, null, 2)));
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
      const browserCoverage = el("section");
      browserCoverage.append(el("h3", null, "Browser WASM · partial"), el("p", null, `${browser?.records?.length || 0} records retained; ${browser?.dropped || 0} dropped from the bounded ring. Measured instrumented spans only. Host and browser clocks are separate; no cross-host causal link is inferred.`));
      grid.append(browserCoverage); body.append(grid);
    } else {
      const isWasm = active === "wasm";
      const recorded = isWasm ? wasmSpans(browser) : hostSpans(snapshot);
      const matchingTraces = new Set(recorded.filter(s => matches(`${s.name} ${s.fields?.actor || ""}`)).map(s => s.trace));
      const spans = recorded.filter(s => matchingTraces.has(s.trace));
      const roots = spans.filter(s => !s.parent);
      const defaultRoot = isWasm ? roots.findLast(s => s.name === "doc.render") || roots.at(-1) : roots.at(-1);
      const chosen = selectedTrace === "latest" || selectedTrace === "all" ? defaultRoot : roots.find(s => s.id === selectedTrace);
      const visible = isWasm && selectedTrace === "all" ? spans : chosen ? spans.filter(s => s.trace === chosen.trace) : [];
      if (roots.length) {
        const chooser = el("select", "activity-trace-choice");
        chooser.setAttribute("aria-label", "Choose a recorded trace");
        if (isWasm) {const all=el("option",null,"All retained capabilities");all.value="all";chooser.append(all);}
        const latest = el("option", null, isWasm ? (defaultRoot?.name === "doc.render" ? "Follow latest render" : "Follow latest retained trace") : "Follow latest action");
        latest.value = "latest"; chooser.append(latest);
        for (const root of [...roots].reverse()) {
          const option = el("option", null, `${root.name} · ${duration(root.duration)} · ${new Date(root.start).toLocaleTimeString()}`);
          option.value = root.id; chooser.append(option);
        }
        if (selectedTrace !== "latest" && selectedTrace !== "all" && !chosen) {
          const gone = el("option", null, "Selected trace is no longer retained"); gone.value = selectedTrace; chooser.append(gone);
        }
        chooser.value = selectedTrace;
        chooser.addEventListener("change", () => { selectedTrace = chooser.value; selectedSpan = null; lastSignature = ""; render(); body.querySelector("select")?.focus(); });
        body.append(chooser);
      }
      body.append(el("p", "activity-note", isWasm
        ? `Recorded WASM trace. Bar width is elapsed span time, including children. ${browser?.dropped || 0} records dropped. This is not a sampled CPU profile.`
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
        traceHeading.append(el("strong", null, isWasm && selectedTrace === "all" ? "Spans from every loaded capability" : chosen.name), el("span", null, [chosen.fields?.actor, chosen.fields?.outcome, duration(chosen.duration)].filter(Boolean).join(" · ")));
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
          const row = el("button", null, `${span.parent ? "↳ " : ""}${span.name} · ${duration(span.duration)} · ${span.id}${span.parent ? ` ← ${span.parent}` : ""}`);
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
  close.addEventListener("click", () => setOpen(false));
  drawer.addEventListener("keydown", event => { if (event.key === "Escape") { event.stopPropagation(); setOpen(false); } });
  pause.addEventListener("click", () => { paused = !paused; pause.textContent = paused ? "Resume view" : "Pause view"; pause.setAttribute("aria-pressed", String(paused)); if (!paused) void refreshData(); else render(); });
  refresh.addEventListener("click", () => { lastSignature = ""; void refreshData(); });
  category.addEventListener("change", () => { visibleEventCount = 40; render(); });
  filter.addEventListener("input", () => { visibleEventCount = 40; render(); });
  download.addEventListener("click", () => {
    const payload = { schema: "same-page-observability-v1", capturedAt: new Date().toISOString(), host: snapshot, browser: browser, hostError: error, clocksCorrelated: false };
    const url = URL.createObjectURL(new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" }));
    const link = el("a"); link.href = url; link.download = `same-page-trace-${Date.now()}.json`; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  client.on("surface.activity", () => { if (!drawer.hidden && !paused) void refreshData(); });
  // Poll only while the drawer is open. The host journal also catches external MCP activity.
  setInterval(() => { if (!drawer.hidden && !paused && !document.hidden) void refreshData(); }, 2000);
  document.addEventListener("samepage:trace-open",event=>setOpen(event.detail?.open ?? true));
  setOpen(true);

}


export function activate({wasm, application, client}) {
  if (!wasm || !application) throw new Error("trace needs its verified WASM artifact");
  application.registerCapability("trace",wasm);
  globalThis.__samepageTraceSnapshot = () => JSON.parse(wasm.trace_operation(JSON.stringify({operation:"snapshot"}))).ok;
  globalThis.__samepageTrace = raw => {
    const record=JSON.parse(raw);
    const response=JSON.parse(wasm.trace_operation(JSON.stringify({operation:"record",record})));
    if (response.error) throw new Error(`trace: ${response.error.message}`);
    const mark=`samepage:${record.span_id}`;
    if (record.kind==="span_open") performance.mark(mark);
    else if (record.kind==="span_close") {
      if (performance.getEntriesByName(mark,"mark").length) {
        performance.clearMeasures(record.name);
        performance.measure(record.name,{start:mark,end:performance.now()});
        performance.clearMarks(mark);
      }
    }
  };
  const style=document.createElement("style");style.textContent=STYLE;document.head.append(style);
  installActivityDrawer(client,wasm,application);
}
const STYLE=`
.activity-drawer {position:fixed;inset:120px 18px 20px;z-index:30;background:#f7f9fc;color:#253044;border:1px solid #8291a8;border-radius:12px;box-shadow:0 12px 60px #17233855;display:flex;flex-direction:column;padding:18px;}
.activity-drawer[hidden]{display:none}.activity-header{display:flex;flex-wrap:wrap;align-items:center;gap:16px}.activity-header h2{margin:0}.activity-controls{display:flex;gap:8px}.activity-controls button{padding:8px}.activity-status{flex:1}.activity-tabs{display:flex;gap:12px;padding:16px 0}.activity-body{overflow:auto;flex:1}.activity-note{color:#59647b}.flame-chart{height:var(--chart-height);position:relative;background:#e4ebf4;min-height:80px}.flame-span{position:absolute;left:var(--span-left);width:var(--span-width);top:var(--span-top);height:28px;background:#c2ddf8;border:1px solid #729eca;text-align:left;white-space:nowrap;overflow:hidden;font-size:12px}.activity-trace-heading{display:flex;gap:16px;align-items:center}.flame-detail{padding:16px;background:white}.flame-span-list button{display:block;margin:6px}.activity-event{display:flex;gap:12px;flex-wrap:wrap;padding:10px;border-bottom:1px solid #ccd6e4}.activity-event details{flex-basis:100%}.activity-body pre{white-space:pre-wrap;overflow-wrap:anywhere}.activity-coverage{display:grid;grid-template-columns:repeat(2,1fr);gap:16px}
`;
