const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};
const button = (text, action, cls = "quiet-button") => {
  const node = el("button", cls, text); node.type = "button";
  node.addEventListener("click", action); return node;
};
const key = edge => JSON.stringify([edge.from, edge.to, edge.label]);
// A state transition without a guard reads as its event alone.
const said = label => String(label || "").replace(/\s*\[\]$/, "");
// Agent sessions of one client share one name.
const author = name => String(name || "someone").replace(/-\d+$/, "");

export function activate({wasm, application}) {
  if (!wasm || !application) throw new Error('Architecture needs its verified WASM and shared document');
  const panel = document.getElementById('architecture');
  if (!panel) throw new Error('Architecture proposal mount is absent');
  application.registerCapability('architecture', wasm);
  const style = el('style'); style.textContent = STYLE; document.head.append(style);
  let latest = null, disposed = false;
  panel.hidden = false;
  const launcher = button("Compare", () => open());
  launcher.id = "architecture-compare-toggle";
  launcher.title = "Compare the current map with a shared relationship proposal";
  const list = el("div", "architecture-proposal-list");
  panel.append(launcher, list);
  const dialog = el("dialog", "architecture-compare");
  dialog.setAttribute("aria-labelledby", "comparison-title");
  const header = el("header", "comparison-heading");
  const title = el("h2", null, "Current & proposed"); title.id = "comparison-title";
  const close = button("×", () => dialog.close(), "icon-button");
  close.setAttribute("aria-label", "Close comparison");
  header.append(title, close);
  const controls = el("div", "comparison-controls");
  const picker = el("select"); picker.setAttribute("aria-label", "Proposal revision");
  picker.addEventListener("change", () => { selected = picker.value; render(); });
  const start = button("New proposal", () => {
    const report = read();
    save({ title: `Relationship proposal ${report.comparisons.length + 1}`, baseline_revision: report.current_revision });
  });
  controls.append(picker, start);
  const notice = el("p", "comparison-notice"); notice.setAttribute("role", "status");
  const body = el("div", "comparison-body");
  dialog.append(header, controls, notice, body);
  document.body.append(dialog);
  let selected = "", signature = "", rendered;
  const read = () => {
    if (!latest) throw new Error("The shared architecture projection is still loading.");
    return latest;
  };
  function open() {
    dialog.showModal();
    try { render(); } catch (error) { notice.textContent = String(error.message || error); }
  }
  dialog.addEventListener("close", () => launcher.focus());
  async function save(input) {
    try {
      await application.dispatch('architecture_propose', input);
      notice.textContent = 'Proposal recorded; current relationships unchanged.';
    } catch (error) { notice.textContent = `Architecture proposal refused: ${error.message}`; }
  }
  function revise(change) {
    if (!rendered) return;
    save({ title: rendered.proposal.definition.title, baseline_revision: rendered.baseline_revision, previous: rendered.proposal.id, ...change });
  }
  function focus(id) {
    dialog.close();
    const card = [...document.querySelectorAll(".map-card")].find(n => n.dataset.id === id);
    if (card) document.dispatchEvent(new CustomEvent("samepage:map-focus", {detail: {id}}));
  }
  function render() {
    const report = read(); signature = JSON.stringify(report);
    const comparison = report.comparisons.find(c => c.proposal.id === selected) || report.comparisons.at(-1);
    rendered = comparison;
    selected = comparison?.proposal.id || "";
    picker.replaceChildren();
    if (!report.comparisons.length) { const option = el("option", null, "No shared proposal yet"); option.value = ""; picker.append(option); }
    report.comparisons.forEach((c, index) => {
      const option = el("option", null, `${c.proposal.definition.title} · revision ${index + 1} · ${author(c.proposal.created_by)}`);
      option.value = c.proposal.id; picker.append(option);
    });
    picker.value = selected; picker.disabled = !report.comparisons.length;
    start.disabled = !report.current.components.length;
    body.replaceChildren();
    dialog.dataset.proposal = selected;
    dialog.dataset.baselineChanged = String(Boolean(comparison?.baseline_changed));
    if (!comparison) {
      notice.textContent = "Start a shared draft, then add or remove relationships to compare their impact.";
    } else notice.textContent = `Draft by ${author(comparison.proposal.created_by)}. Each change creates a retained revision.`;
    if (comparison) {
      const verdicts = el('div', 'architecture-verdicts');
      const own = application.view().records.find(r => r.id === comparison.proposal.id)?.props || {};
      const decider = comparison.proposal.decided_by ? (own.touched_by_participant && own.touched_by_participant === globalThis.samepagePerson?.id ? 'you' : comparison.proposal.decided_by) : '';
      verdicts.append(el('span', null, `Verdict: ${comparison.proposal.verdict}${decider ? ' by ' + decider : ''}`));
      for (const [label, verdict] of [['Accept proposal', 'accepted'], ['Reject proposal', 'rejected']]) {
        const b = button(label, async () => {
          try { await application.dispatch('architecture_decide', {id:selected, verdict}); }
          catch (error) { notice.textContent = `Architecture verdict refused: ${error.message}`; }
        });
        b.dataset.verdict = verdict; b.disabled = comparison.proposal.verdict === verdict; verdicts.append(b);
      }
      body.append(verdicts);
      // An accepted proposal can be applied: its added links are drawn and its
      // removed links taken off the map, one edit each, by the person.
      const edges = application.view().records.filter(r => r.kind === 'edge');
      const same = (record, edge) => record.props.from === edge.from && record.props.to === edge.to && (record.props.label || '') === (edge.label || '');
      const toAdd = comparison.added.filter(edge => !edges.some(record => same(record, edge)));
      const toRemove = comparison.removed.map(edge => edges.find(record => same(record, edge))).filter(Boolean);
      if (comparison.proposal.verdict === 'accepted' && (toAdd.length || toRemove.length)) {
        const apply = button(`Apply to the map: add ${toAdd.length}, remove ${toRemove.length}`, async () => {
          apply.disabled = true;
          try {
            for (const edge of toAdd) application.applyBatch(await application.plan('map', 'card_link', {from: edge.from, to: edge.to, label: edge.label || ''}));
            for (const record of toRemove) application.applyBatch(await application.plan('map', 'object_remove', {id: record.id}));
            notice.textContent = `Applied to the map: ${toAdd.length} added, ${toRemove.length} removed.`;
            application.toast?.(notice.textContent);
          } catch (error) { notice.textContent = `The map was not changed: ${error.message}`; apply.disabled = false; }
        }, 'comparison-apply');
        body.append(el('p', 'comparison-boundary', 'Accepted. Apply it to change the relationships on the map.'), apply);
      } else if (comparison.proposal.verdict === 'accepted') body.append(el('p', 'comparison-boundary', 'Accepted and applied: the map has these relationships.'));
      else body.append(el('p', 'comparison-boundary', 'A verdict records agreement on this revision. Accepting offers to apply it to the map.'));
    }
    if (comparison?.baseline_changed) {
      body.append(el("p", "comparison-stale", "The current map changed after this proposal began. These columns compare its captured baseline with the proposal. Start a new proposal to use the latest map."));
    }
    const base = comparison?.proposal.definition.baseline || report.current;
    const proposed = comparison?.proposal.definition.proposed || report.current;
    const counts = el("div", "comparison-counts");
    for (const [count, label] of [[comparison?.added.length || 0, "relationships added"], [comparison?.removed.length || 0, "relationships removed"], [comparison?.direct.length || 0, "directly affected"], [comparison?.upstream.length || 0, "upstream to review"]]) {
      const item = el("span"); item.append(el("strong", null, String(count)), document.createTextNode(` ${label}`)); counts.append(item);
    }
    body.append(counts);
    if (comparison) {
      const naming = el("details", "comparison-name");
      const form = el("form");
      const name = el("input"); name.value = comparison.proposal.definition.title;
      name.setAttribute("aria-label", "Proposal title"); name.required = true; name.maxLength = 160;
      const submit = el("button", "quiet-button", "Save name"); submit.type = "submit";
      form.append(name, submit);
      form.addEventListener("submit", event => { event.preventDefault(); revise({ title: name.value.trim() }); });
      naming.append(el("summary", null, "Name this proposal"), form); body.append(naming);
    }
    const legend = el("p", "comparison-legend", "Green + added · Red − removed · Amber: directly affected · Blue: upstream to review");
    body.append(legend);
    const diagrams = el("div", "comparison-diagrams");
    // Components keep the order they have on the map: top to bottom, then left to right.
    const geometry = new Map(application.view().records.filter(r => r.kind === 'node').map(r => [r.id, [Number(r.props.y) || 0, Number(r.props.x) || 0]]));
    for (const [graph, label, side] of [[base, comparison?.baseline_changed ? "Captured current map" : "Current map", "current"], [proposed, "Proposed relationships", "proposed"]]) {
      const section = el("section");
      section.append(el("h3", null, label), diagram(graph, comparison, side, focus, geometry));
      diagrams.append(section);
    }
    body.append(diagrams);
    const labels = new Map(base.components.map(n => [n.id, n.label]));
    const name = id => labels.get(id) || id;
    if (comparison && !comparison.added.length && !comparison.removed.length) {
      body.append(el("p", "comparison-empty", "No relationship changes in this revision. Remove a relationship below or propose a new one."));
    }
    if (comparison?.direct.length || comparison?.upstream.length) {
      const impact = el("section", "comparison-impact");
      impact.append(el("h3", null, "Components to review"));
      const direct = el("div", "comparison-component-list");
      for (const id of comparison.direct) direct.append(button(name(id), () => focus(id), "impact-direct"));
      impact.append(el("p", null, "Direct endpoints of changed relationships"), direct);
      for (const affected of comparison.upstream) {
        const line = el("p", "impact-upstream");
        line.append(button(name(affected.id), () => focus(affected.id)), document.createTextNode(` via ${affected.via.map(name).join(" → ")}`));
        impact.append(line);
      }
      body.append(impact);
    }
    const relationshipHeading = el("h3", null, "Relationships"); body.append(relationshipHeading);
    const table = el("table", "comparison-relationships");
    const thead = el("thead"), headings = el("tr");
    for (const text of ["Connection", "Current", "Proposed", "Change proposal"]) headings.append(el("th", null, text));
    thead.append(headings); table.append(thead);
    const tbody = el("tbody");
    const before = new Set(base.relationships.map(key)), after = new Set(proposed.relationships.map(key));
    const edges = [...new Map([...base.relationships, ...proposed.relationships].map(edge => [key(edge), edge])).values()];
    edges.sort((a, b) => Number(before.has(key(a)) === after.has(key(a))) - Number(before.has(key(b)) === after.has(key(b))));
    for (const edge of edges) {
      const exists = after.has(key(edge)), was = before.has(key(edge));
      const row = el("tr", exists === was ? "" : exists ? "relationship-added" : "relationship-removed");
      const label = el("td"); label.append(el("strong", null, `${name(edge.from)} → ${name(edge.to)}`), el("span", null, edge.label || "Unlabelled relationship"));
      const action = el("td");
      if (comparison) action.append(button(exists ? (was ? "Remove" : "Undo addition") : "Restore", () => revise(exists ? { remove: [edge] } : { add: [edge] })));
      row.append(label, el("td", null, was ? "Present" : "Absent"), el("td", null, exists ? (was ? "Kept" : "+ Added") : "− Removed"), action);
      tbody.append(row);
    }
    table.append(tbody); body.append(table);
    if (comparison) {
      const form = el("form", "comparison-add");
      const from = el("select"), to = el("select"), label = el("input");
      from.setAttribute("aria-label", "Relationship from"); to.setAttribute("aria-label", "Relationship to");
      for (const node of base.components) {
        for (const select of [from, to]) { const option = el("option", null, node.label); option.value = node.id; select.append(option); }
      }
      if (base.components.length > 1) to.selectedIndex = 1;
      label.setAttribute("aria-label", "Relationship meaning"); label.placeholder = "Relationship meaning"; label.maxLength = 500; label.required = true;
      const submit = el("button", "quiet-button", "Add relationship"); submit.type = "submit";
      form.append(from, el("span", null, "→"), to, label, submit);
      form.addEventListener("submit", event => { event.preventDefault(); revise({ add: [{ from: from.value, to: to.value, label: label.value.trim() }] }); });
      body.append(form);
    }
    const boundary = el("details", "comparison-boundary");
    boundary.append(el("summary", null, "Scope and evidence"), el("p", null, report.boundary), el("p", null, "This comparison includes card relationships across the whole page. Drawn connectors, runtime behavior and code edits are outside its scope."));
    body.append(boundary);
  }
  async function update({view}) {
    if (disposed) return;
    latest = await application.project('architecture', view);
    list.replaceChildren();
    for (const comparison of latest.comparisons) {
      const p = comparison.proposal;
      const b = button(`${p.definition.title} · ${p.created_by} · ${p.verdict}${comparison.baseline_changed ? ' · Stale baseline' : ''}`, () => { selected = p.id; open(); });
      b.className = 'architecture-proposal'; b.dataset.id = p.id; b.dataset.author = p.created_by; b.dataset.verdict = p.verdict;
      b.dataset.baselineChanged = String(comparison.baseline_changed); list.append(b);
    }
    if (dialog.open) render();
  }
  const opened = async () => { try { latest = await application.project('architecture'); open(); } catch(error) { notice.textContent = error.message; } };
  document.addEventListener('samepage:architecture-open', opened);
  const unregister = application.registerRenderer('architecture', {render:update});
  return {dispose() { disposed=true; unregister(); document.removeEventListener('samepage:architecture-open', opened); dialog.remove(); style.remove(); panel.replaceChildren(); panel.hidden=true; }};

}

function diagram(graph, comparison, side, focus, geometry = new Map()) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = (tag, attributes = {}, text) => {
    const node = document.createElementNS(ns, tag);
    for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const place = (node) => geometry.get(node.id) || [Infinity, Infinity];
  const nodes = [...graph.components].sort((a, b) => place(a)[0] - place(b)[0] || place(a)[1] - place(b)[1] || a.label.localeCompare(b.label));
  const height = Math.max(120, nodes.length * 64 + 20);
  const root = svg("svg", { viewBox: `0 0 480 ${height}`, role: "img", "aria-label": `${side} relationship map`, class: "comparison-graph" });
  root.append(svg("title", {}, `${side} map: ${nodes.length} components and ${graph.relationships.length} relationships. Full connection labels follow in the table.`));
  const definitions = svg("defs");
  for (const style of ["kept", "added", "removed"]) {
    const marker = svg("marker", { id: `comparison-${side}-${style}`, viewBox: "0 0 10 10", refX: 9, refY: 5, markerWidth: 7, markerHeight: 7, orient: "auto-start-reverse" });
    marker.append(svg("path", { d: "M 0 0 L 10 5 L 0 10 z", class: `arrow-${style}` })); definitions.append(marker);
  }
  root.append(definitions);
  const y = new Map(nodes.map((n, i) => [n.id, i * 64 + 40]));
  const changed = new Set((side === "current" ? comparison?.removed : comparison?.added)?.map(key) || []);
  graph.relationships.forEach((edge, index) => {
    const start = y.get(edge.from), end = y.get(edge.to);
    if (start === undefined || end === undefined) return;
    const state = changed.has(key(edge)) ? side === "current" ? "removed" : "added" : "kept";
    const x = 330 + (index % 10) * 13;
    const d = start === end ? `M 312 ${start - 8} C ${x + 35} ${start - 38}, ${x + 35} ${start + 38}, 312 ${start + 8}` : `M 312 ${start} H ${x - 8} Q ${x} ${start} ${x} ${start + Math.sign(end - start) * 8} V ${end - Math.sign(end - start) * 8} Q ${x} ${end} ${x - 8} ${end} H 312`;
    const path = svg("path", { d, class: `edge-${state}`, "marker-end": `url(#comparison-${side}-${state})` });
    path.append(svg("title", {}, `${edge.label || "Relationship"}: ${nodes.find(n => n.id === edge.from)?.label} → ${nodes.find(n => n.id === edge.to)?.label}`)); root.append(path);
    if (edge.label && start !== end) {
      const words = said(edge.label).length > 14 ? `${said(edge.label).slice(0, 13)}…` : said(edge.label);
      const text = svg("text", { x: x + 5, y: (start + end) / 2 + 4, class: `edge-label edge-label-${state}` }, words);
      text.append(svg("title", {}, said(edge.label))); root.append(text);
    }
  });
  for (const node of nodes) {
    const state = comparison?.direct.includes(node.id) ? "direct" : comparison?.upstream.some(i => i.id === node.id) ? "upstream" : "unchanged";
    const group = svg("g", { class: `comparison-node node-${state}`, tabindex: 0, role: "button", "aria-label": `Locate ${node.label}` });
    group.append(svg("rect", { x: 12, y: y.get(node.id) - 22, width: 298, height: 44, rx: 7 }));
    const limit = innerWidth < 761 ? 26 : 31;
    const text = svg("text", { x: 26, y: y.get(node.id) + 5 }, node.label.length > limit ? node.label.slice(0, limit - 2) + "…" : node.label);
    group.append(svg("title", {}, `${node.label}${node.path ? ` · ${node.path}` : ""}`), text);
    group.addEventListener("click", () => focus(node.id));
    group.addEventListener("keydown", e => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); focus(node.id); } });
    root.append(group);
  }
  return root;
}


const STYLE = ".comparison-graph .edge-label { font: 11px system-ui, sans-serif; fill: currentColor; opacity: .85; }\n.comparison-apply { margin: 8px 0; }\n" + ".architecture-compare { width: min(1180px, calc(100vw - 48px)); max-height: calc(100dvh - 48px); padding: 0; border: 1px solid var(--line); border-radius: 12px; color: var(--ink); background: var(--panel); font: 14px/1.5 var(--font-ui); }\n.architecture-compare::backdrop { background: rgb(0 0 0 / 65%); }\n.comparison-heading { display: flex; justify-content: space-between; align-items: center; padding: 18px 24px 8px; }\n.comparison-heading h2 { font-size: 23px; margin: 0; }\n.comparison-heading .icon-button { color: var(--ink); width: 36px; height: 36px; }\n.comparison-controls { display: flex; gap: 12px; padding: 8px 24px; }\n.comparison-controls select { flex: 1; min-width: 0; }\n.architecture-compare select, .architecture-compare input { border: 1px solid var(--line); border-radius: 5px; background: var(--panel-2); color: var(--ink); font: inherit; padding: 8px; min-width: 0; }\n.comparison-notice { color: var(--muted); margin: 8px 24px 16px; min-height: 21px; }\n.comparison-body { padding: 0 24px 24px; }\n.comparison-stale { border: 1px solid var(--accent); padding: 12px; border-radius: 5px; }\n.comparison-counts { display: flex; flex-wrap: wrap; gap: 14px 28px; padding: 14px 0; border-block: 1px solid var(--line); color: var(--muted); }\n.comparison-counts strong { font-size: 24px; color: var(--ink); font-weight: 500; margin-right: 4px; }\n.comparison-legend, .comparison-boundary { font-size: 13px; color: var(--muted); }\n.comparison-diagrams { display: grid; grid-template-columns: 1fr 1fr; gap: 24px; }\n.comparison-diagrams section { min-width: 0; background: var(--panel-2); border: 1px solid var(--line); border-radius: 8px; }\n.comparison-diagrams h3 { margin: 14px 18px 0; font-size: 16px; }\n.comparison-graph { display: block; width: 100%; }\n.comparison-graph path[class^=\"edge-\"] { fill: none; stroke-width: 2; }\n.comparison-graph .edge-kept { stroke: var(--muted); opacity: .5; }\n.comparison-graph .edge-added { stroke: #28a879; stroke-width: 3; }\n.comparison-graph .edge-removed { stroke: #de6d70; stroke-width: 3; stroke-dasharray: 6 4; }\n.arrow-kept { fill: var(--muted); }.arrow-added { fill: #28a879; }.arrow-removed { fill: #de6d70; }\n.comparison-node { cursor: pointer; }\n.comparison-node rect { fill: var(--panel); stroke: var(--line); stroke-width: 1.5; }\n.comparison-node text { fill: var(--ink); font: 15px var(--font-ui); }\n.comparison-node.node-direct rect { stroke: #c89036; stroke-width: 2; }\n.comparison-node.node-upstream rect { stroke: #6a9cd4; stroke-width: 2; }\n.comparison-node:hover rect, .comparison-node:focus rect { stroke: var(--accent); stroke-width: 3; }\n.comparison-impact { margin-top: 24px; padding-top: 2px; }\n.comparison-component-list { display: flex; flex-wrap: wrap; gap: 8px; }\n.comparison-component-list button { border: 1px solid #c89036; border-radius: 5px; background: var(--panel-2); color: var(--ink); padding: 7px 11px; font: inherit; cursor: pointer; }\n.impact-upstream { color: var(--muted); overflow-wrap: anywhere; }\n.comparison-relationships { width: 100%; border-collapse: collapse; text-align: left; font-size: 13px; }\n.comparison-relationships th { font-weight: 500; color: var(--muted); padding: 8px; }\n.comparison-relationships td { border-top: 1px solid var(--line); padding: 12px 8px; overflow-wrap: anywhere; }\n.comparison-relationships td:first-child { width: 53%; }\n.comparison-relationships strong { font-weight: 500; display: block; }\n.comparison-relationships td span { display: block; color: var(--muted); margin-top: 4px; }\n.relationship-added td:nth-child(3) { color: #28a879; }.relationship-removed td:nth-child(3) { color: #de6d70; }\n.comparison-add { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 18px 0; border-top: 1px solid var(--line); }\n.comparison-add select { flex: 1 1 180px; width: 180px; }.comparison-add input { flex: 1 1 160px; }\n.comparison-empty { color: var(--muted); }\n.comparison-name { margin-top: 12px; color: var(--muted); font-size: 13px; }\n.comparison-name form { display: flex; gap: 8px; margin-top: 8px; }.comparison-name input { flex: 1; }\n.architecture-compare :focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }\n.project-facts { position: relative; min-width: 0; font-size: 12px; color: var(--muted); margin-left: 22px; }\n/* Wraps rather than ellipsizes: this flex row has no reserved width for it,\n   so its own max-width is never the binding constraint, the header's\n   available space is, and an ellipsis still leaves scrollWidth wider than\n   clientWidth whatever the box shrinks to. Wrapped text does not. */\n.project-facts > summary { cursor: pointer; white-space: normal; overflow-wrap: break-word; max-width: 460px; line-height: 1.3; }\n.project-facts-content { position: absolute; z-index: 130; width: min(400px, calc(100vw - 40px)); left: 0; top: 30px; padding: 18px; border: 1px solid var(--line); border-radius: 8px; background: var(--panel); box-shadow: 0 12px 30px rgb(0 0 0 / 25%); color: var(--ink); font-size: 13px; overflow-wrap: anywhere; }\n.project-facts-content code { font-size: 11px; white-space: pre-wrap; }\n@media (max-width: 1100px) { .project-facts > summary { max-width: 225px; }.project-facts { margin-left: 12px; } }\n@media (max-width: 760px) {\n  .architecture-compare { width: 100vw; max-width: 100vw; height: 100dvh; max-height: 100dvh; border: 0; border-radius: 0; }\n  .comparison-heading, .comparison-controls { padding-inline: 16px; }.comparison-notice { margin-inline: 16px; }.comparison-body { padding-inline: 16px; }\n  .comparison-diagrams { grid-template-columns: 1fr; gap: 14px; }.comparison-counts { gap: 8px 16px; font-size: 12px; }.comparison-counts strong { font-size: 20px; }\n  .comparison-node text { font-size: 20px; }\n  .comparison-relationships { font-size: 12px; }.comparison-relationships th, .comparison-relationships td { padding: 8px 4px; }.comparison-relationships .quiet-button { padding: 6px; font-size: 12px; }.comparison-relationships td:first-child { width: 43%; }\n  .project-facts { order: 10; flex-basis: 100%; margin: 0; }.project-facts > summary { max-width: 100%; }.project-facts-content { top: 24px; }\n}\n\n#architecture { padding: 10px 20px; border-bottom: 1px solid #345066; background:#152433; }\n#architecture[hidden] { display:none; }\n.architecture-proposal-list { display:flex; flex-wrap:wrap; gap:8px; margin-top:8px; }\n#architecture button, .architecture-compare button { cursor:pointer; border:1px solid #628a9b; border-radius:5px; padding:7px 10px; background:#264757; color:#e6f4fa; }\n.architecture-compare { --line:#345066; --panel:#152433; --ink:#eaf0f5; --muted:#a9bcca; --accent:#9ae6c1; --danger:#f1b397; --font-ui:system-ui,sans-serif; --font-mono:monospace; color:var(--ink); }\n.architecture-compare select, .architecture-compare input { background:#101b24; color:var(--ink); padding:8px; border:1px solid #53718a; }\n.architecture-verdicts { display:flex; align-items:center; gap:10px; margin-bottom:12px; }\n.architecture-proposal[data-baseline-changed=\"true\"] { border-color:#e5a784 !important; }\n.architecture-compare button:disabled { opacity:.45; cursor:default; }\n";
