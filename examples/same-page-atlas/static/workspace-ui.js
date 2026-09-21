import { installActivityDrawer } from "/activity-drawer.js";
import { installSharedContext } from "/shared-context.js";
import { installThemePicker } from "/theme.js";
import { installArchitectureReview } from "/architecture-review.js";
import { installAtlasModeStrip } from "/atlas-mode.js";

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

export function installWorkspaceUI(client) {
  installThemePicker();
  installArchitectureReview();
  installSharedContext();
  installAtlasModeStrip();
  document.getElementById("workspace-decisions")?.addEventListener("click", () => globalThis.atlas?.openDecisionHistory());
  installActivityDrawer(client);
  const closePanel = el("button", "dock-close", "×");
  closePanel.type = "button";
  closePanel.setAttribute("aria-label", "Close supporting panel");
  closePanel.addEventListener("click", () => document.querySelector('[data-collapse="dock"]')?.click());
  document.querySelector(".dock-header")?.append(closePanel);
  const present = document.getElementById("presentation-toggle");
  const togglePresentation = (enabled) => {
    if (document.body.classList.contains("presentation") === enabled) return;
    document.body.classList.toggle("presentation", enabled);
    present.setAttribute("aria-pressed", String(enabled));
    present.textContent = enabled ? "Exit presentation" : "Present";
    window.dispatchEvent(new Event("resize"));
    document.dispatchEvent(new CustomEvent("atlas-presentation", { detail: { enabled } }));
  };
  present.addEventListener("click", () => togglePresentation(!document.body.classList.contains("presentation")));
  document.getElementById("workspace-discuss").addEventListener("click", () => {
    togglePresentation(false);
    document.querySelector('[data-dock-tab="chat"]')?.click();
    document.querySelector("agui-conversation")?.shadowRoot?.querySelector("textarea")?.focus();
  });
  const search = el("section", "canvas-search");
  search.id = "canvas-search";
  search.setAttribute("popover", "auto");
  search.setAttribute("aria-label", "Find on canvas");
  const label = el("label", null, "Find a node, drawing, or exploration");
  const input = el("input");
  input.type = "search";
  input.placeholder = "Search the shared page…";
  input.autocomplete = "off";
  const results = el("div", "canvas-search-results");
  label.append(input);
  search.append(label, results);
  document.body.append(search);
  const render = () => {
    results.replaceChildren();
    const doc = globalThis.atlas?.doc;
    if (!doc) { results.append(el("p", "muted", "The shared replica is still connecting.")); return; }
    let state;
    try { state = JSON.parse(doc.snapshot()); }
    catch { results.append(el("p", "muted", "Could not read the shared replica. Try again when connected.")); return; }
    const query = input.value.trim().toLocaleLowerCase();
    const items = [
      ...(state.explanations || []).map(flow => ({ id: flow.definition.id, title: flow.definition.title, kind: "Exploration", note: flow.definition.goal, exploration: true })),
      ...(state.nodes || []).map(n => ({ id: n.id, title: n.label, kind: "Node", note: n.note })),
      ...(state.shapes || []).map(s => ({ id: s.id, title: s.label || s.text, kind: "Drawing", note: s.form })),
    ].filter(item => item.title && `${item.title} ${item.note || ""}`.toLocaleLowerCase().includes(query));
    for (const item of items.slice(0, 30)) {
      const button = el("button", null, item.title);
      button.type = "button";
      button.append(el("small", null, `${item.kind}${item.note ? ` · ${item.note}` : ""}`));
      button.addEventListener("click", () => {
        search.hidePopover();
        document.dispatchEvent(new CustomEvent("atlas-search-target", { detail: { id: item.id, exploration: Boolean(item.exploration) } }));
      });
      results.append(button);
    }
    if (!items.length) results.append(el("p", "muted", query ? "No matching objects on this page." : "This canvas is empty. Start a discussion or draw something."));
  };
  const openSearch = () => { render(); search.showPopover(); input.focus(); input.select(); };
  document.addEventListener("atlas-open-search", openSearch);
  input.addEventListener("input", render);
  input.addEventListener("keydown", event => {
    if (event.key === "ArrowDown") { event.preventDefault(); results.querySelector("button")?.focus(); }
    if (event.key === "Enter") { event.preventDefault(); results.querySelector("button")?.click(); }
  });
  results.addEventListener("keydown", event => {
    const buttons = [...results.querySelectorAll("button")];
    const index = buttons.indexOf(document.activeElement);
    if (event.key === "ArrowDown") { event.preventDefault(); buttons[(index + 1) % buttons.length]?.focus(); }
    if (event.key === "ArrowUp") { event.preventDefault(); if (index <= 0) input.focus(); else buttons[index - 1]?.focus(); }
  });
  document.addEventListener("keydown", event => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openSearch(); }
    if (event.key === "Escape" && document.body.classList.contains("presentation")) togglePresentation(false);
  });
}
