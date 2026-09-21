import { installArchitectureCompare, installProjectFacts } from "/architecture-compare.js";
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

export function installArchitectureReview() {
  const launcher = el("button", "quiet-button", "Review");
  launcher.id = "architecture-review-toggle";
  launcher.type = "button";
  launcher.setAttribute("aria-controls", "architecture-review");
  launcher.setAttribute("aria-expanded", "false");
  document.querySelector("#workspace-decisions")?.before(launcher);
  installArchitectureCompare();
  installProjectFacts();
  const decisions = el("button", "quiet-button", "Review decisions");
  decisions.type = "button";
  decisions.addEventListener("click", () => globalThis.atlas?.openDecisionHistory());
  document.querySelector(".workspace-file-actions")?.prepend(decisions);
  const panel = el("aside", "architecture-review");
  panel.id = "architecture-review";
  panel.hidden = true;
  panel.setAttribute("aria-label", "Architecture review");
  const header = el("header");
  const heading = el("h2", null, "Architecture review");
  const close = el("button", "icon-button", "×");
  close.type = "button";
  close.setAttribute("aria-label", "Close architecture review");
  header.append(heading, close);
  const controls = el("div", "review-controls");
  const refresh = el("button", "quiet-button", "Check sources again");
  refresh.type = "button";
  const filter = el("select");
  filter.setAttribute("aria-label", "Review focus");
  for (const [value, title] of [["all", "All objects"], ["gaps", "Evidence gaps"], ["challenged", "Open claims"], ["decisions", "Trade-offs"]]) {
    const option = el("option", null, title); option.value = value; filter.append(option);
  }
  controls.append(filter, refresh);
  const content = el("div", "review-content");
  content.setAttribute("aria-live", "polite");
  panel.append(header, controls, content);
  document.getElementById("page-grid")?.append(panel);
  let report, checkedAt, request = 0;
  function toggle(open) {
    panel.hidden = !open;
    document.getElementById("page-grid")?.classList.toggle("review-open", open);
    launcher.setAttribute("aria-expanded", String(open));
    window.dispatchEvent(new Event("resize"));
    if (open) { load(); close.focus(); } else launcher.focus();
  }
  launcher.addEventListener("click", () => toggle(panel.hidden));
  const yieldToDetails = () => { if (!panel.hidden) toggle(false); };
  document.addEventListener("atlas-open-inspector", yieldToDetails);
  document.getElementById("workspace-discuss")?.addEventListener("click", yieldToDetails);
  close.addEventListener("click", () => toggle(false));
  panel.addEventListener("keydown", event => { if (event.key === "Escape") { event.stopPropagation(); toggle(false); } });
  refresh.addEventListener("click", load);
  filter.addEventListener("change", render);
  // Semantic edits invalidate a displayed review. Camera and selection do not.
  document.addEventListener("atlas-document-changed", () => {
    if (!panel.hidden && report) {
      const warning = content.querySelector(".review-freshness");
      if (warning) warning.textContent = "The shared page changed. Check sources again to refresh this review.";
    }
  });
  async function load() {
    const current = ++request;
    refresh.disabled = true;
    content.replaceChildren(el("p", "muted", "Reading the shared page and its source references…"));
    try {
      const response = await fetch("/atlas/review", { cache: "no-store" });
      const payload = await response.json();
      if (!response.ok || !payload.ok) throw new Error(payload.error || "Architecture review is unavailable on this host.");
      if (current !== request) return;
      report = payload.review;
      checkedAt = new Date();
      render();
    } catch (error) {
      if (current === request) content.replaceChildren(el("p", "review-error", `Could not review this page: ${error.message}`));
    } finally { if (current === request) refresh.disabled = false; }
  }
  function render() {
    if (!report) return;
    const nodes = report.nodes;
    const gaps = n => !n.evidence.length || n.evidence.some(e => e.state !== "readable");
    const open = n => n.challenge.unsettled > 0;
    content.replaceChildren();
    const repo = el("p", "review-repository", report.repository.split("/").pop());
    repo.title = report.repository;
    content.append(repo, el("p", "review-freshness", `Sources checked ${checkedAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}. Recheck after code changes.`));
    const summary = el("div", "review-counts");
    for (const [value, label] of [[nodes.length, "objects"], [nodes.filter(gaps).length, "source gaps"], [nodes.reduce((sum, node) => sum + node.challenge.unsettled, 0), "open claims"]]) {
      const item = el("div"); item.append(el("strong", null, String(value)), el("span", null, label)); summary.append(item);
    }
    const boundary = el("details", "review-boundary");
    boundary.append(el("summary", null, "What this check establishes"), el("p", null, report.evidence_boundary));
    content.append(summary, boundary);
    const selected = new Set(globalThis.atlas?.selection?.ids || []);
    const visible = nodes.filter(n => filter.value === "all" || (filter.value === "gaps" && gaps(n)) || (filter.value === "challenged" && open(n)) || (filter.value === "decisions" && n.draft_ids.length));
    visible.sort((a, b) => Number(selected.has(b.id)) - Number(selected.has(a.id)));
    for (const node of visible) {
      const card = el("article", "review-object");
      card.dataset.id = node.id;
      const button = el("button", "review-object-title", node.label);
      button.type = "button";
      button.addEventListener("click", () => {
        if (innerWidth < 761) toggle(false);
        document.dispatchEvent(new CustomEvent("atlas-search-target", { detail: { id: node.id, exploration: false } }));
      });
      card.append(button);
      const links = el("p", "review-connections", `${node.incoming.length} incoming · ${node.outgoing.length} outgoing`);
      const names = ids => ids.map(id => nodes.find(n => n.id === id)?.label || id).join(", ");
      card.append(links);
      if (node.incoming.length || node.outgoing.length) {
        const details = el("details"); details.append(el("summary", null, "Connected objects"));
        if (node.incoming.length) details.append(el("p", null, `From: ${names(node.incoming)}`));
        if (node.outgoing.length) details.append(el("p", null, `To: ${names(node.outgoing)}`));
        card.append(details);
      }
      if (!node.evidence.length) card.append(el("p", "review-gap", "No linked source. Treat the implementation as unestablished."));
      for (const source of node.evidence) {
        card.append(el("p", source.state === "readable" ? "review-source" : "review-error", `${source.path}${source.lines ? `:${source.lines}` : ""} · ${source.state === "readable" ? "readable now" : source.state}`));
        if (source.error) card.append(el("p", "review-error", source.error));
      }
      if (!node.claims.length) card.append(el("p", "review-gap", "No explicit claims to review."));
      for (const claim of node.claims) {
        card.append(el("p", "review-claim", `${claim.basis} · ${claim.verdict}: ${claim.text}`));
        if (claim.revision) card.append(el("p", "review-gap", `Pinned to ${claim.revision.slice(0, 8)}. Current-code freshness is unverified.`));
      }
      const drafts = report.decision_drafts.filter(d => node.draft_ids.includes(d.id));
      if (!drafts.length) card.append(el("p", "review-gap", "Trade-offs and alternatives are not recorded."));
      for (const draft of drafts) {
        const details = el("details", "review-tradeoffs");
        details.append(el("summary", null, `Proposed decision: ${draft.input.title}`));
        for (const [label, value] of [["Decision", draft.input.decision], ["Rationale", draft.input.rationale], ["Trade-offs", draft.input.tradeoffs], ["Consequences", draft.input.consequences]]) {
          details.append(el("h3", null, label), el("p", null, value));
        }
        details.append(el("h3", null, "Alternatives"));
        for (const alt of draft.input.alternatives) details.append(el("p", null, `${alt.option}: ${alt.reason_not_chosen}`));
        if (draft.input.not_enforced.length) details.append(el("h3", null, "Still requires validation"), ...draft.input.not_enforced.map(value => el("p", null, value)));
        card.append(details);
      }
      content.append(card);
    }
    if (!visible.length) content.append(el("p", "muted", filter.value === "decisions" ? "No decision drafts with trade-offs are recorded on this page." : "No objects match this review focus."));
  }
}
