// The Map's three modes: Explain (claimed cards only, nothing verified),
// Map (every card renders its computed trust tier; an extractor lane no
// card claims draws itself as undeclared), Cement (export the agreed Map to
// G8 obligations). Shared state, not per-session: a human clicking a mode
// button here and an agent calling atlas_map_mode_set see and change the
// same value. This module owns the header strip and the legend; the tier
// chips and borders it turns on live are painted by extensions/atlas/index.js
// against the same body[data-map-mode] attribute this sets.
const MODES = ["explain", "map", "cement"];
const MODE_LABEL = { explain: "Explain", map: "Map", cement: "Cement" };
const POLL_MS = 3000;

const LEGEND_ROWS = [
  ["claimed", "Default for anything an agent or person authored. Not yet checked against anything."],
  ["verified", "Bound to a file via source link; its hash is fresh."],
  ["drifted", "Was verified; the bound file's hash has since changed."],
  ["gate unmet", "Cemented in G8; g8 check fails or has not been run."],
  ["gate met", "Cemented in G8; g8 check passes."],
  ["proposed", "Carries a replaces link to an existing card, which draws struck through."],
  ["undeclared", "The extractor found this in the code; no card claims it. Cannot be dismissed: claim it or cement its removal."],
];

export function installAtlasModeStrip() {
  const strip = document.createElement("div");
  strip.className = "atlas-mode-strip";
  strip.setAttribute("role", "tablist");
  strip.setAttribute("aria-label", "Atlas mode");

  const buttons = {};
  for (const mode of MODES) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "atlas-mode-button";
    button.textContent = MODE_LABEL[mode];
    button.setAttribute("role", "tab");
    button.setAttribute("aria-pressed", "false");
    button.addEventListener("click", () => setMode(mode));
    buttons[mode] = button;
    strip.append(button);
  }
  const obligationsCount = document.createElement("span");
  obligationsCount.className = "atlas-mode-obligations-count";
  obligationsCount.hidden = true;
  strip.append(obligationsCount);

  const legendToggle = document.createElement("button");
  legendToggle.type = "button";
  legendToggle.className = "atlas-legend-toggle";
  legendToggle.textContent = "Legend";
  legendToggle.setAttribute("aria-expanded", "false");
  legendToggle.setAttribute("aria-controls", "atlas-legend");

  const legend = document.createElement("div");
  legend.className = "atlas-legend";
  legend.id = "atlas-legend";
  legend.setAttribute("popover", "auto");
  legend.setAttribute("aria-label", "Trust tier legend");
  const heading = document.createElement("h3");
  heading.textContent = "Trust tiers";
  const list = document.createElement("dl");
  for (const [term, detail] of LEGEND_ROWS) {
    const dt = document.createElement("dt");
    dt.textContent = term;
    const dd = document.createElement("dd");
    dd.textContent = detail;
    list.append(dt, dd);
  }
  const limit = document.createElement("p");
  limit.className = "legend-limit";
  limit.textContent =
    "Limit: static extraction misses dynamic spawns, anything built from a runtime string, loaded as a plugin, or started by a shell script this scan never reads.";
  legend.append(heading, list, limit);
  // Hidden by default: a popover with no explicit show starts closed.
  function positionLegend() {
    const box = legendToggle.getBoundingClientRect();
    legend.style.left = `${Math.max(8, Math.min(innerWidth - legend.offsetWidth - 8, box.left))}px`;
    legend.style.top = `${box.bottom + 8}px`;
  }
  legendToggle.addEventListener("click", () => {
    if (legend.matches(":popover-open")) legend.hidePopover();
    else {
      legend.showPopover();
      positionLegend();
    }
  });
  legend.addEventListener("toggle", () => {
    legendToggle.setAttribute("aria-expanded", String(legend.matches(":popover-open")));
    if (legend.matches(":popover-open")) positionLegend();
  });

  // A genuinely separate row below the header, not a wrapped flex child
  // inside it: .app-header runs flex-wrap: nowrap at desktop widths (see
  // workspace-polish.css), by design, so a wrapped-flex-item approach here
  // would either fight that rule or depend on removing it elsewhere. Two
  // more items competing with the scope question, page checks, and
  // workspace actions for the SAME line is exactly what squeezed the
  // question down to one word per line once these joined it; a sibling
  // element under the header can never re-create that competition, at any
  // width (see styles.css/.atlas-mode-row and the word-wrap-collapse check
  // in tests/visual-audit/run-audit.mjs).
  const row = document.createElement("div");
  row.className = "atlas-mode-row";
  row.append(strip, legendToggle);
  document.querySelector(".app-header")?.after(row);
  document.body.append(legend);

  function paint(mode, count) {
    document.body.dataset.mapMode = mode;
    for (const [key, button] of Object.entries(buttons)) {
      button.setAttribute("aria-pressed", String(key === mode));
    }
    if (mode === "cement") {
      obligationsCount.hidden = false;
      obligationsCount.textContent = `${count ?? 0} obligation${count === 1 ? "" : "s"}`;
    } else {
      obligationsCount.hidden = true;
    }
    document.dispatchEvent(new CustomEvent("atlas-map-mode-changed", { detail: { mode } }));
  }

  async function poll() {
    try {
      const response = await fetch("/atlas/mode", { cache: "no-store" });
      const body = await response.json();
      if (body.ok) paint(body.mode, body.obligations_count);
    } catch {
      // A quiet miss: the strip just keeps showing the last mode it knew.
    }
  }

  async function setMode(mode) {
    try {
      await fetch("/atlas/mode", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ mode }),
      });
    } finally {
      poll();
    }
  }

  poll();
  setInterval(poll, POLL_MS);
}
