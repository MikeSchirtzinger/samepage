const el = (tag, text, className) => {
  const node = document.createElement(tag);
  if (text) node.textContent = text;
  if (className) node.className = className;
  return node;
};
const renderDecision = (article, text) => {
  article.replaceChildren();
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    const heading = line.match(/^(#{1,3})\s+(.*)$/);
    article.append(el(heading ? "h3" : "p", heading ? heading[2] : line));
  }
};

export async function openDecisionHistory(ctx) {
  const dialog = el("dialog", null, "context-dialog decision-dialog");
  dialog.setAttribute("aria-label", "Project decisions and rationale");
  const body = el("div", "Loading project decisions...");
  const close = el("button", "Close"); close.type = "button"; close.addEventListener("click", () => dialog.close());
  dialog.append(el("h2", "Decisions and their reasons"), body, close);
  document.body.append(dialog); dialog.addEventListener("close", () => dialog.remove(), { once: true }); dialog.showModal();
  try {
    const result = await ctx.action("atlas_decisions_read", {});
    const decisions = JSON.parse(result.result);
    body.replaceChildren();
    if (!decisions.length) body.append(el("p", "No decisions have been recorded for this project yet."));
    for (const decision of decisions) {
      const details = el("details");
      details.append(el("summary", `${decision.title} · ${decision.status}`), el("small", decision.path, "muted"));
      const article = el("article", null, "decision-history");
      renderDecision(article, decision.body);
      details.append(article); body.append(details);
    }
  } catch (error) { body.textContent = String(error.message || error); }
}

export function openDecisionReview(ids, ctx) {
  const snapshot = () => JSON.parse(globalThis.atlas.doc.snapshot());
  const page = snapshot();
  const sameIds = values => JSON.stringify([...values].sort()) === JSON.stringify([...ids].sort());
  let drafts = (page.decision_drafts || []).filter(item => sameIds(item.input.ids));
  // CRDT iteration order is not proposal chronology. Multiple proposals need
  // an explicit choice instead of silently choosing a participant's draft.
  let draftId = drafts.length === 1 ? drafts[0].id : "";
  let preview = null;
  const dialog = el("dialog", null, "context-dialog decision-dialog");
  dialog.setAttribute("aria-labelledby", "decision-review-title");
  const title = el("h2", "Review decision"); title.id = "decision-review-title";
  const subtitle = el("p", "Keep the decision and its reasoning with the project. Cement activates the reviewed structural checks in G8.", "muted");
  const form = el("form");
  const editor = el("div", null, "decision-editor"); form.append(editor);
  const empty = el("section", null, "decision-awaiting-draft");
  empty.append(el("h3", "Let the agent prepare the record"), el("p", "The proposal should include the decision, rationale, alternatives, tradeoffs, and exact G8 checks. It will appear here ready for your review.", "muted"));
  const request = el("button", "Ask agent to prepare the decision"); request.type = "button";
  request.addEventListener("click", () => {
    document.dispatchEvent(new CustomEvent("atlas-request-decision-draft", { detail: { ids } })); dialog.close();
  });
  empty.append(request); form.prepend(empty);
  const controls = {};
  const add = (key, label, rows = 2) => {
    const wrapper = el("label", label);
    const input = el("textarea"); input.rows = rows; input.name = key; input.required = true;
    wrapper.append(input); editor.append(wrapper); controls[key] = input;
  };
  add("title", "Decision title", 1);
  add("decision", "What we decided");
  add("rationale", "Why this approach");
  add("alternatives", "Alternatives and why we rejected them", 3);
  add("tradeoffs", "Tradeoffs we accept");
  add("consequences", "What this means for later work");
  add("not_enforced", "What these checks will not verify", 2); controls.not_enforced.required = false;
  const checksSummary = el("div", null, "decision-checks");
  const technical = el("details");
  technical.append(el("summary", "Exact G8 check specifications"));
  const checks = el("textarea"); checks.rows = 10; checks.spellcheck = false; checks.setAttribute("aria-label", "G8 structural check specifications JSON");
  technical.append(el("p", "Supported checks: enum structure and forbidden dependencies. An agent can prepare these. Behavior outside these checks remains unproven.", "muted"), checks);
  editor.append(checksSummary, technical);
  const selector = el("details", null, "decision-proposals"); selector.open = true;
  selector.setAttribute("aria-label", "Prepared decision proposals");
  const populate = draft => {
    const input = draft?.input || {};
    draftId = draft?.id || "";
    for (const key of ["title", "decision", "rationale", "tradeoffs", "consequences"]) controls[key].value = input[key] || "";
    controls.alternatives.value = (input.alternatives || []).map(item => `${item.option}: ${item.reason_not_chosen}`).join("\n");
    controls.not_enforced.value = (input.not_enforced || []).join("\n");
    checks.value = JSON.stringify(input.checks || [], null, 2);
    checksSummary.replaceChildren(el("h3", "What G8 will protect"));
    for (const check of input.checks || []) checksSummary.append(el("p", `${check.id}: ${check.requirement}`));
    if (!input.checks?.length) checksSummary.append(el("p", "No checks proposed yet. Ask the agent to prepare an explicit check for each enforceable requirement.", "muted"));
  };
  const renderProposals = () => {
    selector.replaceChildren(el("summary", "Choose a prepared proposal"));
    for (const draft of drafts) {
      const choice = el("button", draft.input.title, "decision-proposal-choice"); choice.type = "button";
      choice.dataset.draftId = draft.id;
      choice.append(el("small", draft.input.decision, "muted"));
      choice.addEventListener("click", () => {
        populate(draft); invalidate(); selector.open = false;
        selector.querySelector("summary").textContent = "Change proposal";
        void loadPreview();
      });
      selector.append(choice);
    }
  };
  renderProposals();
  populate(drafts.length === 1 ? drafts[0] : null);
  const outputLabel = el("label", "Keep the review bundle in a new directory");
  const directory = el("input"); directory.name = "directory"; directory.required = true; directory.placeholder = "e.g. .samepage/decisions/persistence-v1";
  directory.value = `.samepage/decisions/review-${Date.now()}`;
  const outputOptions = el("details"); outputOptions.append(el("summary", "Review bundle location"));
  outputLabel.append(directory); outputOptions.append(outputLabel); form.append(outputOptions);
  const message = el("p", null, "decision-status muted"); message.setAttribute("role", "status");
  const exact = el("details"); exact.hidden = true;
  exact.append(el("summary", "Exact files and G8 check specifications"));
  const artifact = el("pre"); exact.append(artifact);
  const reviewBody = el("article", null, "decision-history"); reviewBody.hidden = true;
  const recordTabs = el("div", null, "decision-record-tabs"); recordTabs.hidden = true;
  recordTabs.setAttribute("role", "tablist"); recordTabs.setAttribute("aria-label", "Decision record and G8 record");
  const adrTab = el("button", "Decision record"); adrTab.type = "button"; adrTab.setAttribute("role", "tab");
  const g8Tab = el("button", "G8 record"); g8Tab.type = "button"; g8Tab.setAttribute("role", "tab");
  const g8Record = el("section", null, "decision-g8-record"); g8Record.hidden = true;
  const showRecord = kind => {
    reviewBody.hidden = kind !== "adr"; g8Record.hidden = kind !== "g8";
    for (const [tab, active] of [[adrTab, kind === "adr"], [g8Tab, kind === "g8"]]) {
      tab.setAttribute("aria-selected", String(active)); tab.tabIndex = active ? 0 : -1;
    }
  };
  adrTab.addEventListener("click", () => showRecord("adr")); g8Tab.addEventListener("click", () => showRecord("g8"));
  recordTabs.addEventListener("keydown", event => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === "Home" ? adrTab : event.key === "End" ? g8Tab : document.activeElement === adrTab ? g8Tab : adrTab;
    next.click(); next.focus();
  });
  recordTabs.append(adrTab, g8Tab);
  const consent = el("label", null, "decision-consent");
  const accepted = el("input"); accepted.type = "checkbox";
  consent.append(accepted, el("span", "I agree with this decision and approve these G8 checks. Passing structural checks pin the inspected files; changing them requires renewed review."));
  consent.hidden = true;
  const actions = el("div", null, "context-actions");
  const close = el("button", "Close"); close.type = "button"; close.addEventListener("click", () => dialog.close());
  const save = el("button", "Review changes"); save.type = "button";
  const edit = el("button", "Edit proposal"); edit.type = "button"; edit.hidden = true;
  const review = el("button", "Preview decision"); review.type = "button";
  const cement = el("button", "Cement into G8"); cement.type = "submit"; cement.disabled = true;
  actions.append(close, edit, save, review, cement);
  form.append(message, recordTabs, reviewBody, g8Record, exact, consent, actions);
  dialog.append(title, subtitle);
  if (drafts.length > 1) dialog.append(selector);
  dialog.append(form); document.body.append(dialog);
  dialog.addEventListener("close", () => dialog.remove(), { once: true });
  let previewGeneration = 0;
  const invalidate = () => {
    previewGeneration += 1; preview = null; accepted.checked = false; cement.disabled = true;
    exact.hidden = true; consent.hidden = true; recordTabs.hidden = true; g8Record.hidden = true;
    editor.hidden = false; reviewBody.hidden = true; edit.hidden = true;
    save.hidden = false; review.hidden = false; outputOptions.hidden = false;
  };
  edit.addEventListener("click", () => { invalidate(); dialog.scrollTop = 0; });
  for (const control of [...Object.values(controls), checks]) control.addEventListener("input", () => { draftId = ""; invalidate(); });
  save.addEventListener("click", () => {
    try {
      const alternatives = controls.alternatives.value.split("\n").filter(line => line.trim()).map(line => {
        const at = line.indexOf(":");
        if (at < 1) throw new Error("Write each alternative as Approach: reason we rejected it.");
        return { option: line.slice(0, at).trim(), reason_not_chosen: line.slice(at + 1).trim() };
      });
      const input = Object.fromEntries(["title", "decision", "rationale", "tradeoffs", "consequences"].map(key => [key, controls[key].value.trim()]));
      draftId = globalThis.atlas.saveDecisionDraft({ ...input, ids, alternatives, checks: JSON.parse(checks.value), not_enforced: controls.not_enforced.value.split("\n").map(line => line.trim()).filter(Boolean) });
      invalidate(); void loadPreview();
    } catch (error) { message.textContent = String(error.message || error); }
  });
  const loadPreview = async () => {
    if (!draftId) { message.textContent = "Save this proposal before previewing it."; return; }
    const generation = ++previewGeneration;
    review.disabled = true; editor.hidden = true; outputOptions.hidden = true;
    save.hidden = true; review.hidden = true; cement.hidden = false; empty.hidden = true;
    message.textContent = "Preparing the exact decision and G8 record...";
    try {
      // Local CRDT writes may arrive at the host after the next click. Retry
      // only that explicit synchronization state; never retry stale approval.
      let result;
      for (let attempt = 0; ; attempt += 1) {
        try {
          result = await ctx.action("atlas_decision_preview", { ids, draft_id: draftId });
          break;
        } catch (error) {
          if (attempt >= 12 || !String(error.message || error).includes("decision draft not found")) throw error;
          message.textContent = "Waiting for the shared page to receive this proposal...";
          await new Promise(resolve => setTimeout(resolve, 250));
        }
      }
      if (generation !== previewGeneration || !dialog.open) return;
      preview = JSON.parse(result.result);
      const adr = Object.entries(preview.writes).find(([name]) => name.endsWith(".md"));
      if (!adr) throw new Error("The preview did not contain a decision record.");
      artifact.textContent = JSON.stringify(preview.writes, null, 2);
      renderDecision(reviewBody, adr[1]); editor.hidden = true;
      g8Record.replaceChildren();
      const files = Object.entries(preview.writes).filter(([name, contents]) => name.endsWith(".json") && contents?.obligations);
      for (const [name, contents] of files) {
        const section = el("section");
        section.append(el("p", name, "muted"));
        for (const obligation of contents.obligations) {
          section.append(el("h3", obligation.id), el("p", obligation.rule.description));
          const code = el("pre"); code.textContent = JSON.stringify(obligation.checker, null, 2);
          section.append(code);
        }
        for (const limit of contents.open_questions || []) section.append(el("p", `Not enforced: ${limit}`, "muted"));
        const raw = el("details"); raw.append(el("summary", "Full G8 record"));
        const code = el("pre"); code.textContent = JSON.stringify(contents, null, 2);
        raw.append(code); section.append(raw); g8Record.append(section);
      }
      if (!files.length) throw new Error("The preview did not contain a G8 record.");
      recordTabs.hidden = false; showRecord("adr");
      edit.hidden = false; save.hidden = true; review.hidden = true;
      exact.hidden = false; exact.open = false; consent.hidden = false;
      message.textContent = "Review the rationale, limits, and checks below. Cement also updates the project's ADR index, G8 specification, and ratification lock.";
      dialog.scrollTop = 0;
    } catch (error) { invalidate(); message.textContent = String(error.message || error); }
    finally { review.disabled = false; }
  };
  review.addEventListener("click", loadPreview);
  accepted.addEventListener("change", () => { cement.disabled = !accepted.checked || !preview; });
  form.addEventListener("submit", async event => {
    event.preventDefault();
    if (!event.isTrusted || !preview || !accepted.checked) return;
    cement.disabled = true;
    try {
      const result = await ctx.action("atlas_cement_decision", { ids, draft_id: draftId, review_token: preview.review_token, directory: directory.value.trim() });
      invalidate();
      message.textContent = result.result;
      editor.hidden = true; outputOptions.hidden = true; save.hidden = true; review.hidden = true; cement.hidden = true;
      title.textContent = "Decision recorded";
    } catch (error) { invalidate(); message.textContent = String(error.message || error); }
  });
  const showReadyDraft = () => {
    empty.hidden = Boolean(drafts.length);
    editor.hidden = true; outputOptions.hidden = true; save.hidden = true; review.hidden = true;
    cement.hidden = !draftId;
    if (draftId) void loadPreview();
    else if (drafts.length > 1) message.textContent = "Choose which prepared proposal to review.";
  };
  const onDraft = event => {
    if (drafts.length || preview || !dialog.open) return;
    const arrived = (event.detail.decision_drafts || []).filter(item => sameIds(item.input.ids));
    if (!arrived.length) return;
    drafts = arrived; populate(arrived.length === 1 ? arrived[0] : null);
    if (arrived.length > 1) { renderProposals(); form.before(selector); }
    showReadyDraft();
  };
  document.addEventListener("atlas-document-changed", onDraft);
  dialog.addEventListener("close", () => { previewGeneration += 1; document.removeEventListener("atlas-document-changed", onDraft); }, { once: true });
  directory.addEventListener("input", () => { accepted.checked = false; cement.disabled = true; });
  dialog.showModal(); showReadyDraft();
}
