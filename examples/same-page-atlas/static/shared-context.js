const make = (tag, text, className) => {
  const node = document.createElement(tag);
  if (text) node.textContent = text;
  if (className) node.className = className;
  return node;
};
const levels = [
  ["purpose", "Purpose", "Why this project exists"],
  ["system", "System", "How the parts work together"],
  ["component", "Component", "One part and its responsibilities"],
  ["implementation", "Implementation", "Code, rules, and exact behavior"],
];

// Hover previews shared scope. A deliberate click writes through the Rust
// peer. Camera zoom and the containment lens remain local preferences.
export function installSharedContext() {
  let page = null, selection = [], observedHeads = [];
  const bar = make("section", null, "shared-context");
  bar.setAttribute("aria-label", "Shared conversation scope");
  const toggle = make("button", null, "scope-toggle");
  toggle.type = "button"; toggle.setAttribute("aria-expanded", "false"); toggle.setAttribute("aria-controls", "scope-map");
  const miniature = make("span", null, "scope-miniature"); miniature.setAttribute("aria-hidden", "true");
  for (const [key] of levels) { const band = make("i"); band.dataset.level = key; miniature.append(band); }
  const altitude = make("span", "Scope", "context-altitude");
  toggle.append(miniature, altitude);
  const question = make("button", null, "context-question"); question.type = "button";
  const questionText = make("span", "What are we trying to understand?", "context-question-text"); question.append(questionText);
  bar.append(toggle, question); document.querySelector(".header-spacer")?.before(bar);
  const map = make("section", null, "scope-map");
  map.id = "scope-map"; map.setAttribute("popover", "auto"); map.setAttribute("aria-label", "Project scope map");
  const mapHeader = make("div", null, "scope-map-heading");
  const close = make("button", "×", "scope-close"); close.type = "button"; close.setAttribute("aria-label", "Close scope map");
  close.addEventListener("click", () => map.hidePopover());
  mapHeader.append(make("strong", "Where are we in the project?"), close);
  const funnel = make("div", null, "scope-funnel");
  const preview = make("p", null, "scope-preview"); preview.setAttribute("aria-live", "polite");
  const error = make("p", null, "context-error"); error.setAttribute("role", "alert");
  const names = ids => ids.map(id => [...(page?.nodes || []), ...(page?.shapes || [])].find(item => item.id === id)?.label || id);
  const current = () => page?.context?.current;
  const commit = patch => {
    const context = current();
    if (!context || page.context.heads.length !== 1) {
      error.textContent = "Set a shared question first. Conflicting questions need explicit reconciliation."; return;
    }
    try {
      globalThis.atlas.setContext({ question: context.question, altitude: context.altitude, subjects: context.subjects,
        assumptions: context.assumptions, replaces: [...page.context.heads], ...patch });
      error.textContent = "";
    } catch (failure) { error.textContent = String(failure); }
  };
  for (const [key, title, detail] of levels) {
    const button = make("button", null, "scope-band"); button.type = "button"; button.dataset.level = key;
    button.append(make("strong", title), make("span", detail)); button.setAttribute("aria-label", `${title}: ${detail}`);
    const explain = () => { preview.textContent = `${title} · ${detail}. Click to share this level.`; };
    button.addEventListener("pointerenter", explain); button.addEventListener("focus", explain);
    button.addEventListener("click", () => commit({ altitude: key })); funnel.append(button);
  }
  const subjects = make("div", null, "scope-subjects");
  const subjectLabel = make("p", null, "scope-subject-label");
  const useSelection = make("button", "Use selected objects", "scope-use-selection"); useSelection.type = "button";
  useSelection.addEventListener("click", () => commit({ subjects: selection }));
  const whole = make("button", "Whole project", "scope-whole"); whole.type = "button";
  whole.addEventListener("click", () => commit({ subjects: [] })); subjects.append(subjectLabel, useSelection, whole);
  const details = make("details", null, "scope-background"); details.append(make("summary", "Question, assumptions, and history"));
  const assumptions = make("p", null, "scope-assumptions"); const history = make("div", null, "context-history");
  const edit = make("button", "Edit the shared question"); edit.type = "button";
  details.append(assumptions, edit, history);
  map.append(mapHeader, funnel, preview, subjects, error, details); document.body.append(map);
  const position = () => {
    const box = toggle.getBoundingClientRect();
    map.style.left = `${Math.max(8, Math.min(innerWidth - map.offsetWidth - 8, box.left))}px`;
    map.style.top = `${Math.max(8, Math.min(innerHeight - map.offsetHeight - 8, box.bottom + 8))}px`;
  };
  let hoverTimer;
  const open = () => { clearTimeout(hoverTimer); map.showPopover(); position(); };
  toggle.addEventListener("click", () => { if (map.matches(":popover-open")) map.hidePopover(); else open(); });
  toggle.addEventListener("pointerenter", () => { hoverTimer = setTimeout(open, 200); });
  toggle.addEventListener("pointerleave", () => clearTimeout(hoverTimer));
  map.addEventListener("toggle", () => {
    toggle.setAttribute("aria-expanded", String(map.matches(":popover-open")));
    if (map.matches(":popover-open")) position();
  });
  const dialog = make("dialog", null, "context-dialog"); dialog.setAttribute("aria-label", "Edit shared question");
  const form = make("form"); const queryLabel = make("label", "What are we trying to understand?");
  const query = make("textarea"); query.name = "question"; query.required = true; query.maxLength = 2000; query.rows = 2; queryLabel.append(query);
  const assumptionDetails = make("details"); assumptionDetails.append(make("summary", "Assumptions (optional)"));
  const assumptionInput = make("textarea"); assumptionInput.name = "assumptions"; assumptionInput.rows = 3; assumptionInput.maxLength = 4000;
  assumptionInput.setAttribute("aria-label", "Assumptions"); assumptionDetails.append(assumptionInput);
  const conflicts = make("div", null, "context-conflict");
  const formError = make("p", null, "context-error"); formError.setAttribute("role", "alert");
  const actions = make("div", null, "context-actions");
  const cancel = make("button", "Cancel"); cancel.type = "button"; cancel.addEventListener("click", () => dialog.close());
  const save = make("button", "Share question"); save.type = "submit";
  actions.append(cancel, save); form.append(conflicts, queryLabel, assumptionDetails, formError, actions); dialog.append(form); document.body.append(dialog);
  const openEditor = () => {
    if (!page) return;
    map.hidePopover(); observedHeads = [...page.context.heads]; query.value = current()?.question || ""; assumptionInput.value = current()?.assumptions || "";
    conflicts.replaceChildren();
    if (observedHeads.length > 1) {
      conflicts.append(make("p", "There are different questions on the shared page. Choose the question we should pursue together."));
      for (const entry of page.context.entries.filter(item => observedHeads.includes(item.id))) {
        const pick = make("button", entry.question); pick.type = "button";
        pick.addEventListener("click", () => { query.value = entry.question; assumptionInput.value = entry.assumptions; }); conflicts.append(pick);
      }
    }
    formError.textContent = ""; dialog.showModal(); query.focus();
  };
  question.addEventListener("click", openEditor); edit.addEventListener("click", openEditor);
  form.addEventListener("submit", event => {
    event.preventDefault();
    try {
      globalThis.atlas.setContext({ question: query.value.trim(), altitude: current()?.altitude || "system",
        subjects: current()?.subjects || selection, assumptions: assumptionInput.value.trim(), replaces: observedHeads }); dialog.close();
    } catch (failure) { formError.textContent = String(failure); }
  });
  const renderSelection = () => {
    useSelection.disabled = !selection.length;
    useSelection.textContent = selection.length ? `Discuss ${names(selection).join(" + ")}` : "Select canvas objects to narrow scope";
  };
  document.addEventListener("atlas-selection-changed", event => { selection = event.detail?.ids || []; renderSelection(); });
  document.addEventListener("atlas-document-changed", event => {
    page = event.detail; const context = current(); const ambiguous = (page.context?.heads.length || 0) > 1;
    altitude.textContent = levels.find(([key]) => key === context?.altitude)?.[1] || "Scope";
    questionText.textContent = context?.question || (ambiguous ? "Reconcile our questions" : "What are we trying to understand?");
    question.title = `${question.textContent}\nClick to edit the shared question`;
    toggle.setAttribute("aria-label", `Project scope: ${altitude.textContent}. ${names(context?.subjects || []).join(", ") || "Whole project"}`);
    for (const band of [...funnel.children, ...miniature.children]) {
      const active = band.dataset.level === context?.altitude; band.classList.toggle("active", active);
      if (band.tagName === "BUTTON") band.setAttribute("aria-pressed", String(active));
    }
    subjectLabel.textContent = `Discussing: ${names(context?.subjects || []).join(" + ") || "Whole project"}`;
    whole.disabled = !context?.subjects.length;
    preview.textContent = context ? `${altitude.textContent} · ${levels.find(([key]) => key === context.altitude)?.[2]}` : "Select a shared question to begin.";
    assumptions.textContent = context?.assumptions || "No assumptions recorded.";
    history.replaceChildren();
    for (const entry of [...(page.context?.entries || [])].reverse()) {
      const item = make("article"); item.append(make("strong", entry.question), make("small", `${entry.altitude} · ${entry.created_by}`)); history.append(item);
    }
    bar.classList.toggle("needs-reconciliation", ambiguous); renderSelection();
  });
}
