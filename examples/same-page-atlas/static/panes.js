// Workspace layout for the shared Atlas page.
//
// The canvas is the primary artifact. Board, inspector, and conversation are
// three views of one supporting workspace, so they share one dock instead of
// claiming three permanent columns. Width, open state, and active tab are
// local preferences. They never enter the shared CRDT document.

const KEY = "agui.atlas.workspace";
const LEGACY_KEY = "agui.atlas.panes";
const DOCK = { fallback: 360, min: 320, max: 720 };
const ATLAS_MIN = 520;
const RAIL = 44;
const GUTTER = 7;
const NUDGE = 16;
const TABS = ["chat", "guide", "board", "inspector", "learn"];
const LABELS = { chat: "Discuss", guide: "Explore", board: "Board", inspector: "Inspect", learn: "Learn" };

const clamp = (value, low, high) => Math.min(high, Math.max(low, value));

function readJson(key) {
  try {
    const value = JSON.parse(localStorage.getItem(key) || "{}");
    return value && typeof value === "object" ? value : {};
  } catch {
    return {};
  }
}

function readStored() {
  const stored = readJson(KEY);
  if (Object.keys(stored).length) return stored;

  // Carry the useful part of the previous two-column preference forward.
  // Two collapsed legacy columns become one collapsed dock. Any visible
  // legacy column becomes one visible workspace rather than resetting it.
  const legacy = readJson(LEGACY_KEY);
  if (!Object.keys(legacy).length) return {};
  return {
    width: Number(legacy.side) || DOCK.fallback,
    collapsed: Boolean(legacy.collapsedBoard && legacy.collapsedSide),
    active: legacy.collapsedSide ? "board" : "inspector",
  };
}

export function installPaneLayout(grid) {
  if (!grid) return null;

  const stored = readStored();
  const state = {
    width: Number(stored.width) || DOCK.fallback,
    collapsed: stored.collapsed === undefined ? true : Boolean(stored.collapsed),
    active: TABS.includes(stored.active) ? stored.active : "chat",
    lastWorkActive: TABS.includes(stored.lastWorkActive) && stored.lastWorkActive !== "learn"
      ? stored.lastWorkActive
      : (stored.active !== "learn" && TABS.includes(stored.active) ? stored.active : "chat"),
  };

  const dock = grid.querySelector('[data-pane="dock"]');
  const resizer = grid.querySelector('[data-resize="dock"]');
  const collapse = grid.querySelector('[data-collapse="dock"]');
  const expand = dock?.querySelector('[data-expand="dock"]');
  const tabs = new Map(
    [...grid.querySelectorAll("[data-dock-tab]")]
      .map((button) => [button.dataset.dockTab, button]),
  );
  const panels = new Map(
    [...grid.querySelectorAll("[data-dock-panel]")]
      .map((panel) => [panel.dataset.dockPanel, panel]),
  );

  function save() {
    try {
      localStorage.setItem(KEY, JSON.stringify({
        width: Math.round(state.width),
        collapsed: state.collapsed,
        active: state.active,
        lastWorkActive: state.lastWorkActive,
      }));
    } catch {
      // Storage denial changes persistence, not whether the workspace works.
    }
  }

  function ceiling() {
    return clamp(grid.clientWidth - ATLAS_MIN - GUTTER, DOCK.min, DOCK.max);
  }

  function apply(notify = true) {
    const width = state.collapsed ? RAIL : clamp(state.width, DOCK.min, ceiling());
    grid.style.setProperty("--col-dock", `${Math.round(width)}px`);
    grid.classList.toggle("dock-collapsed", state.collapsed);
    dock?.classList.toggle("collapsed", state.collapsed);

    if (resizer) {
      resizer.setAttribute("aria-valuenow", String(Math.round(width)));
      resizer.toggleAttribute("data-inert", state.collapsed);
    }
    if (collapse) {
      collapse.setAttribute("aria-expanded", String(!state.collapsed));
      collapse.title = state.collapsed ? "Show the workspace dock" : "Hide the workspace dock";
    }
    if (expand) {
      const label = LABELS[state.active];
      expand.querySelector('[aria-hidden="true"]')?.replaceChildren(label);
    }

    for (const name of TABS) {
      const selected = name === state.active;
      const tab = tabs.get(name);
      const panel = panels.get(name);
      tab?.setAttribute("aria-selected", String(selected));
      if (tab) tab.tabIndex = selected ? 0 : -1;
      if (panel) panel.hidden = !selected;
    }
    const questions = grid.querySelector(".exploration-discuss");
    if (questions) {
      const visible = !state.collapsed && state.active === "guide";
      questions.textContent = visible ? "Close questions" : "Questions";
      questions.setAttribute("aria-expanded", String(visible));
    }

    // The board, transcript, canvas, and semantic target host all measure
    // their own boxes. Give them the same signal a window resize provides.
    if (notify) window.dispatchEvent(new Event("resize"));
  }

  function setWidth(next) {
    state.width = clamp(next, DOCK.min, DOCK.max);
    apply();
    save();
  }

  function setCollapsed(collapsed) {
    state.collapsed = collapsed;
    apply();
    save();
    if (!collapsed) tabs.get(state.active)?.focus();
  }

  function setActive(name, { open = true, focus = false } = {}) {
    if (!TABS.includes(name)) return;
    state.active = name;
    if (name !== "learn") state.lastWorkActive = name;
    if (open) state.collapsed = false;
    apply();
    save();
    if (focus) tabs.get(name)?.focus();
  }

  // Both the tabs and their panels stay in the DOM. Switching changes only
  // what is visible, so extension state and the conversation are preserved.
  for (const [name, tab] of tabs) {
    tab.addEventListener("click", () => setActive(name));
    tab.addEventListener("keydown", (event) => {
      const available = TABS.filter(key => tabs.get(key)?.checkVisibility());
      const current = available.indexOf(name);
      let next = null;
      if (event.key === "ArrowRight") next = available[(current + 1) % available.length];
      else if (event.key === "ArrowLeft") next = available[(current - 1 + available.length) % available.length];
      else if (event.key === "Home") next = available[0];
      else if (event.key === "End") next = available[available.length - 1];
      if (!next) return;
      event.preventDefault();
      setActive(next, { focus: true });
    });
  }

  collapse?.addEventListener("click", () => setCollapsed(!state.collapsed));
  expand?.addEventListener("click", () => setCollapsed(false));
  document.addEventListener("atlas-open-inspector", () => setActive("inspector"));
  let blueprintDock = null;
  document.addEventListener("atlas-exploration-view", (event) => {
    const opening = Boolean(event.detail?.open);
    if (opening && !blueprintDock) blueprintDock = { collapsed: state.collapsed, active: state.active };
    grid.classList.toggle("exploration-focused", opening);
    if (opening) { state.collapsed = true; state.active = "guide"; }
    else if (blueprintDock) { Object.assign(state, blueprintDock); blueprintDock = null; }
    apply();
  });
  const compactLayout = matchMedia("(max-width: 960px)");
  compactLayout.addEventListener("change", () => apply());

  const menus = ".workspace-file-menu, .ink-options, .view-options, .canvas-help";
  document.addEventListener("pointerdown", (event) => {
    for (const menu of document.querySelectorAll(menus)) {
      if (!menu.contains(event.target)) menu.open = false;
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    for (const menu of document.querySelectorAll(menus)) {
      if (!menu.open) continue;
      menu.open = false;
      if (menu.contains(document.activeElement)) menu.querySelector("summary")?.focus();
    }
  });

  // The seam sizes the one supporting dock. On narrow layouts the dock is a
  // bounded bottom sheet and the seam is hidden, so pointer resizing applies
  // only while it is a vertical desktop separator.
  resizer?.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || state.collapsed || matchMedia("(max-width: 960px)").matches) return;
    if (event.target.closest("[data-collapse]")) return;
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = clamp(state.width, DOCK.min, ceiling());
    resizer.setPointerCapture(event.pointerId);
    resizer.classList.add("dragging");
    document.body.classList.add("resizing-panes");

    const move = (moveEvent) => setWidth(startWidth - (moveEvent.clientX - startX));
    const done = () => {
      resizer.removeEventListener("pointermove", move);
      resizer.classList.remove("dragging");
      document.body.classList.remove("resizing-panes");
      try {
        resizer.releasePointerCapture(event.pointerId);
      } catch {
        // Pointer capture may already be gone when the pointer leaves Chrome.
      }
    };
    resizer.addEventListener("pointermove", move);
    resizer.addEventListener("pointerup", done, { once: true });
    resizer.addEventListener("pointercancel", done, { once: true });
  });

  resizer?.addEventListener("dblclick", () => {
    if (state.collapsed) setCollapsed(false);
    setWidth(DOCK.fallback);
  });

  resizer?.addEventListener("keydown", (event) => {
    const step = event.shiftKey ? NUDGE * 4 : NUDGE;
    if (event.key === "ArrowLeft") setWidth(state.width + step);
    else if (event.key === "ArrowRight") setWidth(state.width - step);
    else if (event.key === "Home") setWidth(DOCK.fallback);
    else if (event.key === "Enter" || event.key === " ") setCollapsed(!state.collapsed);
    else return;
    event.preventDefault();
  });

  // Selection updates context, never the person's chosen workspace view.
  // Inspection is explicit so a canvas gesture cannot hide a draft or discussion.

  window.addEventListener("resize", () => apply(false));

  apply();
  return {
    setWidth,
    setCollapsed,
    setActive,
    get state() { return state; },
  };
}
