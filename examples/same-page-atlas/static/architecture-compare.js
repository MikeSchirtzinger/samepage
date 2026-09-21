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

export function installArchitectureCompare() {
  const launcher = button("Compare", () => open());
  launcher.id = "architecture-compare-toggle";
  launcher.title = "Compare the current map with a shared relationship proposal";
  document.querySelector("#architecture-review-toggle")?.before(launcher);
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
    if (!globalThis.atlas?.architectureReview) throw new Error("The shared replica is still loading.");
    return globalThis.atlas.architectureReview();
  };
  function open() {
    dialog.showModal();
    try { render(); } catch (error) { notice.textContent = String(error.message || error); }
  }
  dialog.addEventListener("close", () => launcher.focus());
  document.addEventListener("atlas-document-changed", () => {
    if (!dialog.open) return;
    try { if (JSON.stringify(read()) !== signature) render(); }
    catch (error) { notice.textContent = String(error.message || error); }
  });
  async function save(input) {
    try {
      selected = globalThis.atlas.proposeArchitecture(input);
      render();
      dialog.scrollTop = 0;
      notice.textContent = "Proposal saved in this replica. Checking shared read-back…";
      const id = selected;
      let confirmed = false;
      for (let attempt = 0; attempt < 8; attempt++) {
        const response = await fetch("/atlas/review", { cache: "no-store" });
        if (!response.ok) throw new Error("Shared read-back is unavailable.");
        const payload = await response.json();
        confirmed = payload.review?.architecture?.comparisons?.some(c => c.proposal.id === id);
        if (confirmed) break;
        await new Promise(resolve => setTimeout(resolve, 150));
      }
      if (selected === id) notice.textContent = confirmed ? "Shared proposal confirmed. Current map unchanged." : "Saved locally; shared confirmation is pending. Reopen Compare to check.";
    } catch (error) { notice.textContent = `Could not confirm the proposal: ${error.message || error}`; }
  }
  function revise(change) {
    if (!rendered) return;
    save({ title: rendered.proposal.definition.title, baseline_revision: rendered.baseline_revision, previous: rendered.proposal.id, ...change });
  }
  function focus(id) {
    dialog.close();
    document.dispatchEvent(new CustomEvent("atlas-search-target", { detail: { id, exploration: false } }));
  }
  function render() {
    const report = read(); signature = JSON.stringify(report);
    const comparison = report.comparisons.find(c => c.proposal.id === selected) || report.comparisons.at(-1);
    rendered = comparison;
    selected = comparison?.proposal.id || "";
    picker.replaceChildren();
    if (!report.comparisons.length) { const option = el("option", null, "No shared proposal yet"); option.value = ""; picker.append(option); }
    report.comparisons.forEach((c, index) => {
      const option = el("option", null, `${c.proposal.definition.title} · revision ${index + 1} · ${c.proposal.created_by}`);
      option.value = c.proposal.id; picker.append(option);
    });
    picker.value = selected; picker.disabled = !report.comparisons.length;
    start.disabled = !report.current.components.length;
    body.replaceChildren();
    dialog.dataset.proposal = selected;
    dialog.dataset.baselineChanged = String(Boolean(comparison?.baseline_changed));
    if (!comparison) {
      notice.textContent = "Start a shared draft, then add or remove relationships to compare their impact.";
    } else notice.textContent = `Draft by ${comparison.proposal.created_by}. Each change creates a retained revision.`;
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
    for (const [graph, label, side] of [[base, comparison?.baseline_changed ? "Captured current map" : "Current map", "current"], [proposed, "Proposed relationships", "proposed"]]) {
      const section = el("section");
      section.append(el("h3", null, label), diagram(graph, comparison, side, focus));
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
}

function diagram(graph, comparison, side, focus) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = (tag, attributes = {}, text) => {
    const node = document.createElementNS(ns, tag);
    for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, value);
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const nodes = [...graph.components].sort((a, b) => a.label.localeCompare(b.label));
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

export function installProjectFacts() {
  const facts = el("details", "project-facts");
  facts.id = "project-facts";
  const summary = el("summary", null, "Project facts");
  const content = el("div", "project-facts-content");
  facts.append(summary, content);
  document.querySelector(".app-header .header-spacer")?.before(facts);
  async function refresh() {
    try {
      const response = await fetch("/atlas/review", { cache: "no-store" });
      if (!response.ok) throw new Error("Project facts unavailable");
      const payload = await response.json(), report = payload.review;
      if (!payload.ok || !report) throw new Error("Project facts unavailable");
      const project = report.project_facts;
      const graph = report.architecture?.current;
      if (!project) throw new Error("Restart this host to load project facts.");
      content.replaceChildren();
      const counts = graph ? `${graph.components.length} components · ${graph.relationships.length} relationships` : "Map counts unavailable";
      const name = report.repository.split("/").pop();
      if (project.state === "measured") {
        const languages = project.languages.map(([name, count]) => `${name} ${count}`).join(" · ");
        summary.textContent = `${project.languages[0]?.[0] || "Project"} · ${project.tracked_files.toLocaleString()} files · ${counts}`;
        content.append(el("strong", null, name), el("p", null, `${project.branch} · ${project.revision}`),
          el("p", null, `${project.tracked_files.toLocaleString()} tracked files · ${project.cargo_manifests} Cargo manifests`), el("p", null, languages));
        content.append(el("p", "muted", project.boundary), el("code", null, project.commands.join("\n")));
      } else {
        summary.textContent = counts;
        content.append(el("strong", null, name), el("p", null, project.reason));
      }
      content.append(el("p", "muted", `Map counts cover the whole page. Checked ${new Date().toLocaleTimeString()}.`), button("Refresh facts", refresh));
    } catch (error) {
      summary.textContent = "Project facts unavailable";
      content.replaceChildren(el("p", null, error.message), button("Retry", refresh));
    }
  }
  facts.addEventListener("toggle", () => { if (facts.open) refresh(); });
  let timer;
  document.addEventListener("atlas-document-changed", () => { clearTimeout(timer); timer = setTimeout(refresh, 600); });
  refresh();
}
