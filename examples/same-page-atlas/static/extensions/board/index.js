// The board's browser half.
//
// It renders assertions and it writes the human's five actions. It never
// decides what an assertion means — that arrived as data, already meaning
// something, and this file's whole job is to pick how it looks. The same
// stream could drive a very different page without the writer knowing.
//
// Everything from outside goes in through `textContent` or `append`, never
// through `innerHTML`. So a `<script>` inside a claim is a claim containing the
// characters `<script>`, whoever wrote it.
//
// The controls are always present rather than revealed on hover: this page
// repaints whenever either side writes, and a control you have to hover to find
// is a control that vanishes as you reach for it.

import { semanticTargetNamespace } from "/_agui/semantic-targets.js";

const MARKS = [
  ["agree", "✓", "I agree"],
  ["question", "?", "Ask about this"],
  ["disagree", "!", "I think this is wrong"],
  ["important", "*", "This matters"],
];

const MARK_GLYPH = { agree: "✓", question: "?", disagree: "!", important: "*" };
const MARK_WORDS = {
  agree: "I agree",
  question: "Ask about this",
  disagree: "I think this is wrong",
  important: "This matters",
};

const RELATION_WORDS = {
  supports: "supports",
  contradicts: "contradicts",
  depends_on: "depends on",
  refines: "refines",
  answers: "answers",
};

export async function activate({ client, mount, action }) {
  const targets = semanticTargetNamespace("board");
  const header = el("div", "board-head");
  const subject = el("h2", "board-subject");
  const meta = el("p", "board-meta");
  const seen = button("quiet-button", "I've looked");
  const clear = button("quiet-button", "Clear");
  const controls = el("div", "board-head-controls");
  controls.append(seen, clear);
  header.append(subject, meta, controls);

  const list = el("div", "board-list");
  const status = el("div", "board-status");
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");

  const cementForm = el("form", "board-cement");
  const cementDir = document.createElement("input");
  cementDir.name = "directory";
  cementDir.placeholder = "New cement directory…";
  cementDir.autocomplete = "off";
  cementDir.setAttribute("aria-label", "New cement output directory");
  const cementButton = button("send-button", "Cement");
  cementButton.type = "submit";
  const cementReadiness = line("span", "board-cement-readiness", "Nothing to cement");
  cementForm.append(cementDir, cementButton, cementReadiness);
  const decisionDetails = el("details", "board-decision-details");
  const decisionSummary = el("summary", "");
  decisionSummary.textContent = "Record an agreed decision";
  decisionDetails.append(decisionSummary, cementForm);

  const form = el("form", "board-compose");
  const kind = document.createElement("select");
  kind.name = "kind";
  for (const [value, label] of [["claim", "claim"], ["question", "question"]]) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    kind.append(option);
  }
  const text = document.createElement("input");
  text.name = "text";
  text.placeholder = "Say something of your own…";
  text.autocomplete = "off";
  const send = button("send-button", "Add");
  send.type = "submit";
  const composeContext = el("div", "board-compose-context");
  composeContext.hidden = true;
  const composeTarget = line("span", "board-compose-target", "");
  const composeAttachment = line("span", "board-compose-attachment", "not attached to the picture");
  composeAttachment.title = "Board assertions cannot store a reference to an Atlas picture object yet.";
  composeContext.append(composeTarget, composeAttachment);
  form.append(composeContext, kind, text, send);

  mount.replaceChildren(header, list, status, form, decisionDetails);

  let current = { entries: [], unseen: [] };
  let selectedId = null;
  let atlasTarget = null;

  function showAtlasTarget(target) {
    // A set is one thing the human is pointing at, so the chip says so in one
    // phrase. `label` stays the first member's name for anything that still
    // reads a single target; `summary` is what a human sees.
    atlasTarget = target?.id && target?.label
      ? {
        id: target.id,
        label: target.label,
        count: target.count || 1,
        summary: target.summary || target.label,
      }
      : null;
    composeContext.hidden = !atlasTarget;
    composeTarget.textContent = atlasTarget ? `Pointing at ${atlasTarget.summary}` : "";
  }

  function onAtlasSelection(event) {
    showAtlasTarget(event.detail);
  }
  document.addEventListener("atlas-selection-changed", onAtlasSelection);
  showAtlasTarget(window.atlas?.selection || null);

  function render(value) {
    current = value || { entries: [], unseen: [] };
    const entries = Array.isArray(current.entries) ? current.entries : [];
    const unseen = new Set(current.unseen || []);

    subject.textContent = current.subject || "Nothing named yet";
    subject.classList.toggle("unnamed", !current.subject);

    const fresh = entries.filter((entry) => unseen.has(entry.id)).length;
    meta.textContent = fresh
      ? `${fresh} new since revision ${current.seen_at ?? 0}`
      : `revision ${current.revision ?? 0} · nothing new`;
    meta.classList.toggle("fresh", fresh > 0);
    seen.disabled = fresh === 0;
    seen.title = fresh === 0
      ? "I've looked is available when there are new board items to acknowledge."
      : "Record that you have reviewed the new board items.";
    clear.disabled = entries.length === 0;
    clear.title = entries.length === 0
      ? "There is nothing on the board to clear."
      : "Remove every board item after a second confirmation click.";

    const unsettled = entries.filter((entry) => !isFreshAgreement(entry));
    const ready = entries.length > 0 && unsettled.length === 0;
    cementButton.disabled = !ready;
    cementDir.disabled = !ready;
    cementButton.title = ready
      ? "Write the agreed board into a new directory."
      : entries.length === 0
        ? "Cement needs at least one board item."
        : "Cement is available after every current item has a fresh ✓ agreement.";
    cementReadiness.textContent = ready
      ? `${entries.length} signed · advisory draft`
      : entries.length === 0
        ? "Nothing to cement"
        : `${unsettled.length} need a fresh ✓`;

    // Relations and evidence are clauses about something else, so they render
    // beneath the statement they bear on rather than as entries of their own.
    const outgoing = new Map();
    const evidence = new Map();
    const byId = new Map(entries.map((entry) => [entry.id, entry]));
    for (const entry of entries) {
      if (entry.kind === "relation") {
        push(outgoing, entry.from, entry);
      } else if (entry.kind === "evidence") {
        push(evidence, entry.about, entry);
      }
    }

    targets.reset();
    list.replaceChildren();
    const statements = entries.filter(
      (entry) => entry.kind !== "relation" && entry.kind !== "evidence"
    );
    if (statements.length === 0) {
      const empty = el("div", "board-empty");
      empty.append(
        line("div", "", "Nothing asserted yet."),
        line(
          "div",
          "muted",
          "Ask the agent what it thinks, or write the first claim yourself. Whatever it says, the ✓ is yours."
        )
      );
      list.append(empty);
      return;
    }

    for (const entry of statements) {
      list.append(
        card(entry, {
          unseen: unseen.has(entry.id),
          relations: outgoing.get(entry.id) || [],
          evidence: evidence.get(entry.id) || [],
          byId,
        })
      );
    }
  }

  function card(entry, { unseen, relations, evidence, byId }) {
    const article = el("article", `board-card ${entry.kind}`);
    if (unseen) article.classList.add("unseen");
    if (entry.mark) article.classList.add(`marked-${entry.mark.as}`);
    article.dataset.id = entry.id;
    if (selectedId === entry.id) article.classList.add("pointed");

    const said = el("p", "board-said");
    said.textContent = statement(entry);
    article.append(said);

    if (entry.kind === "choice" && entry.tradeoff) {
      article.append(line("p", "board-tradeoff", `costs: ${entry.tradeoff}`));
    }
    if (entry.kind === "decision") {
      const over = (entry.over || [])
        .map((id) => short(byId.get(id)) || id)
        .join(", ");
      article.append(
        line("p", "board-because", over ? `over ${over} — ${entry.because}` : entry.because)
      );
    }

    for (const relation of relations) {
      const target = byId.get(relation.to);
      article.append(
        clause(
          relation,
          line(
            "p",
            "board-relation",
            `${RELATION_WORDS[relation.how] || relation.how} ${short(target) || relation.to}`
          )
        )
      );
    }
    for (const found of evidence) {
      const verdict = found.verdict || "unverified";
      article.append(
        clause(
          found,
          line("p", `board-evidence ${verdict}`, `${verdict}: ${found.source}`)
        )
      );
    }

    const byline = el("p", "board-byline");
    byline.append(line("span", `by-${entry.author}`, entry.author));
    byline.append(line("span", "board-id", entry.id));
    if (entry.kind === "claim" && entry.status) {
      byline.append(line("span", `board-status-${entry.status}`, entry.status));
    }
    if (entry.kind === "question" && entry.blocking) {
      byline.append(line("span", "board-blocking", "blocking"));
    }
    article.append(byline);

    if (entry.mark) {
      const mark = el("p", "board-mark");
      mark.append(line("span", "glyph", MARK_GLYPH[entry.mark.as] || entry.mark.glyph));
      mark.append(
        line("span", "", entry.mark.note ? `you: ${entry.mark.note}` : MARK_WORDS[entry.mark.as] || entry.mark.as)
      );
      article.append(mark);
    }

    article.append(markControls(entry));

    // Pointing: click a statement and "this" in the composer binds to it.
    article.addEventListener("click", (event) => {
      if (event.target.closest("button")) return;
      for (const other of list.querySelectorAll(".board-card.pointed")) {
        other.classList.remove("pointed");
      }
      selectedId = entry.id;
      article.classList.add("pointed");
      targets.select(entry.id).catch((error) => {
        status.textContent = error instanceof Error ? error.message : String(error);
      });
    });

    targets.register({
      id: entry.id,
      label: short(entry) || entry.id,
      element: article,
    });
    return article;
  }

  // Relations and evidence stay clauses under the assertion they bear on, but
  // they remain assertions in their own right. Their inline controls are what
  // lets the human sign every item before cementing instead of needing a hidden
  // console-only path for two kinds of assertion.
  function clause(entry, content) {
    const section = el("div", "board-clause");
    section.append(content);
    const meta = el("p", "board-clause-meta");
    meta.append(
      line("span", `by-${entry.author}`, entry.author),
      line("span", "board-id", entry.id)
    );
    if (entry.mark) {
      meta.append(line(
        "span",
        "board-clause-mark",
        `${MARK_GLYPH[entry.mark.as] || entry.mark.glyph} ${MARK_WORDS[entry.mark.as] || entry.mark.as}`,
      ));
    }
    section.append(meta, markControls(entry, "board-controls compact"));
    return section;
  }

  function markControls(entry, className = "board-controls") {
    const row = el("div", className);
    for (const [value, glyph, title] of MARKS) {
      const control = button("mark-button", glyph);
      control.title = `${glyph} ${title}`;
      control.setAttribute("aria-label", title);
      control.dataset.mark = value;
      if (entry.mark && entry.mark.as === value) control.classList.add("on");
      control.addEventListener("click", () => {
        if (entry.mark && entry.mark.as === value) {
          run(() => action("board_unmark", { id: entry.id }));
        } else {
          run(() => action("board_mark", { id: entry.id, mark: value }));
        }
      });
      row.append(control);
    }
    return row;
  }

  async function run(call) {
    status.textContent = "";
    status.classList.remove("success");
    try {
      return await call();
    } catch (cause) {
      status.textContent = cause instanceof Error ? cause.message : String(cause);
      return null;
    }
  }

  seen.addEventListener("click", () => run(() => action("board_seen", {})));
  clear.addEventListener("click", () => {
    if (!current.entries?.length) return;
    if (current.entries && current.entries.length && !confirmClear()) return;
    run(() => action("board_clear", {}));
  });

  function confirmClear() {
    // Deliberately not window.confirm: a modal dialog blocks the extension's
    // own event loop and the page is live. Two clicks instead of one.
    if (clear.dataset.armed === "yes") {
      clear.dataset.armed = "";
      clear.textContent = "Clear";
      return true;
    }
    clear.dataset.armed = "yes";
    clear.textContent = "Clear — sure?";
    setTimeout(() => {
      if (clear.dataset.armed !== "yes") return;
      clear.dataset.armed = "";
      clear.textContent = "Clear";
    }, 4000);
    return false;
  }

  async function submit(event) {
    event.preventDefault();
    const said = text.value.trim();
    if (!said) return;
    const pointed = atlasTarget;
    send.disabled = true;
    await run(async () => {
      await action("board_compose", { kind: kind.value, text: said });
      // Clearing only after the action resolves makes the empty field an
      // honest success signal. A refused write leaves the human's words put.
      text.value = "";
      if (pointed) {
        status.classList.add("success");
        status.textContent = `Added while pointing at ${pointed.summary}; not attached to the picture.`;
      }
    });
    send.disabled = false;
  }
  form.addEventListener("submit", submit);

  async function submitCement(event) {
    event.preventDefault();
    const directory = cementDir.value.trim();
    if (!directory) return;
    cementButton.disabled = true;
    const result = await run(() => action("atlas_cement", { directory }));
    if (result && result.ok !== false) {
      status.classList.add("success");
      status.textContent = result.result || `Cemented to ${directory}`;
      cementDir.value = "";
    }
    cementButton.disabled = !current.entries?.length ||
      current.entries.some((entry) => !isFreshAgreement(entry));
  }
  cementForm.addEventListener("submit", submitCement);

  const unsubscribeSelection = targets.onSelection((target) => {
    selectedId = target?.targetId || null;
    for (const card of list.querySelectorAll(".board-card.pointed")) {
      card.classList.toggle("pointed", card.dataset.id === selectedId);
    }
  });

  render(null);
  const unsubscribe = client.on("board.changed", (event) => render(event.value));

  return {
    render,
    dispose() {
      unsubscribe();
      unsubscribeSelection();
      document.removeEventListener("atlas-selection-changed", onAtlasSelection);
      targets.reset();
      form.removeEventListener("submit", submit);
      cementForm.removeEventListener("submit", submitCement);
    },
  };
}

// ── rendering helpers ──────────────────────────────────────────────────────

function statement(entry) {
  if (entry.kind === "decision") return `Decided: ${entry.chose}`;
  return entry.text || entry.label || entry.id;
}

// A short reference to another assertion, for a relation's clause.
function short(entry) {
  if (!entry) return "";
  const said = statement(entry);
  return said.length > 60 ? `${said.slice(0, 57)}…` : said;
}

function push(map, key, value) {
  if (!key) return;
  if (!map.has(key)) map.set(key, []);
  map.get(key).push(value);
}

function isFreshAgreement(entry) {
  if (!entry || entry.mark?.as !== "agree" || entry.mark?.by !== "you") return false;
  if (!entry.authored_at || !entry.mark.at) return false;
  if (entry.mark.board_revision !== entry.changed_at) return false;
  if ((entry.mark.board_revision ?? -1) < (entry.authored_at_revision ?? Number.MAX_SAFE_INTEGER)) {
    return false;
  }
  return entry.kind !== "claim" || entry.status === "settled";
}

function el(tag, className) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  return node;
}

function line(tag, className, textContent) {
  const node = el(tag, className);
  node.textContent = textContent;
  return node;
}

function button(className, label) {
  const node = el("button", className);
  node.type = "button";
  node.textContent = label;
  return node;
}
