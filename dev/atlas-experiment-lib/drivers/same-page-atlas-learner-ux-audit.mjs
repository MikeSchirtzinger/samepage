async function snapshot(api, name) {
  const state = await api.evaluate(`(() => {
    const rect = (element) => {
      if (!element) return null;
      const bounds = element.getBoundingClientRect();
      return {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.bottom,
        width: bounds.width,
        height: bounds.height,
      };
    };
    const visible = (selector) => {
      const element = document.querySelector(selector);
      if (!element) return false;
      const style = getComputedStyle(element);
      const bounds = element.getBoundingClientRect();
      return !element.hidden && style.display !== "none" && style.visibility !== "hidden"
        && bounds.width > 0 && bounds.height > 0;
    };
    const view = document.querySelector("#atlas-view");
    const panel = document.querySelector(".atlas-explanation-flow");
    const dock = document.querySelector(".workspace-dock");
    const viewRect = rect(view);
    const panelRect = rect(panel);
    const segmentRects = [...document.querySelectorAll(".segment-object")]
      .map(rect)
      .filter((bounds) => bounds && bounds.width > 0 && bounds.height > 0);
    const contentRect = segmentRects.length ? {
      left: Math.min(...segmentRects.map((bounds) => bounds.left)),
      top: Math.min(...segmentRects.map((bounds) => bounds.top)),
      right: Math.max(...segmentRects.map((bounds) => bounds.right)),
      bottom: Math.max(...segmentRects.map((bounds) => bounds.bottom)),
    } : null;
    const overlapWidth = viewRect && panelRect
      ? Math.max(0, Math.min(viewRect.right, panelRect.right) - Math.max(viewRect.left, panelRect.left))
      : 0;
    const overlapHeight = viewRect && panelRect
      ? Math.max(0, Math.min(viewRect.bottom, panelRect.bottom) - Math.max(viewRect.top, panelRect.top))
      : 0;
    return {
      viewport: { width: innerWidth, height: innerHeight, deviceScaleFactor: devicePixelRatio },
      atlasMode: document.body.dataset.atlasMode || "author",
      camera: { ...window.atlas.camera },
      flow: window.atlas.explanation.snapshot(),
      selection: window.atlas.selection,
      picker: window.atlas.segment.picker(),
      panelParent: panel?.parentElement?.id || panel?.parentElement?.className || null,
      geometry: {
        view: viewRect,
        panel: panelRect,
        dock: rect(dock),
        content: contentRect,
        panelViewOverlapArea: overlapWidth * overlapHeight,
        contentInsideView: !contentRect || !viewRect || (
          contentRect.left >= viewRect.left - 1
          && contentRect.top >= viewRect.top - 1
          && contentRect.right <= viewRect.right + 1
          && contentRect.bottom <= viewRect.bottom + 1
        ),
      },
      visible: {
        dock: visible(".workspace-dock"),
        drawTools: visible(".draw-tools"),
        objectTree: visible(".segment-tree-panel"),
        objectControl: visible(".segment-flap-panel"),
        segmentBadge: visible(".segment-badge"),
        presenceTray: visible(".agui-semantic-presence"),
        explanation: visible(".atlas-explanation-flow"),
      },
      activeDock: document.querySelector('[data-dock-tab][aria-selected="true"]')?.dataset.dockTab || null,
      inspector: document.querySelector("#dock-inspector")?.innerText || "",
    };
  })()`);
  await api.screenshot(name);
  return state;
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

export async function run(api) {
  await api.waitFor(`(() => window.atlas?.schema?.ready === true
    && document.querySelector("#sync-state")?.textContent === "replica live"
    && Boolean(window.atlas?.explanation?.snapshot?.()))()`, {
    timeoutMs: 30_000,
    retryOnError: true,
  });

  await api.setViewport(1440, 900);
  await api.sleep(400);
  const learnerUrl = await api.evaluate("location.href");
  const desktopBefore = await snapshot(api, "01-desktop-before.png");
  const cameraBeforeEvidence = { ...desktopBefore.camera };
  const evidenceClick = await api.trustedClick(
    ".explanation-target-button:not(.explanation-answer-target)",
  );
  await api.sleep(500);
  const desktopAfterEvidence = await snapshot(api, "02-desktop-after-evidence-click.png");

  const segmentId = await api.evaluate(`(() => [...document.querySelectorAll(".segment-object")]
    .reverse()
    .find((element) => {
      const rect = element.getBoundingClientRect();
      const hit = document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2);
      return hit?.closest?.(".segment-object") === element;
    })?.dataset.segmentId || null)()`);
  let canvasClick = null;
  if (segmentId) {
    canvasClick = await api.trustedClick(
      `.segment-object[data-segment-id="${segmentId}"]`,
    );
    await api.sleep(250);
  }
  const desktopAfterCanvas = await snapshot(api, "03-desktop-after-canvas-click.png");

  await api.setViewport(820, 900);
  await api.navigate(learnerUrl);
  await api.waitFor(`(() => window.atlas?.schema?.ready === true
    && document.querySelector("#sync-state")?.textContent === "replica live"
    && Boolean(window.atlas?.explanation?.snapshot?.()))()`, {
    timeoutMs: 30_000,
    retryOnError: true,
  });
  await api.sleep(500);
  const narrow = await snapshot(api, "04-narrow.png");

  await api.setViewport(1024, 700);
  await api.navigate(learnerUrl);
  await api.waitFor(`(() => window.atlas?.schema?.ready === true
    && document.querySelector("#sync-state")?.textContent === "replica live"
    && Boolean(window.atlas?.explanation?.snapshot?.()))()`, {
    timeoutMs: 30_000,
    retryOnError: true,
  });
  await api.sleep(500);
  const short = await snapshot(api, "05-short.png");

  const cameraMovedByEvidence = ["x", "y", "scale"].some((key) => (
    Math.abs(desktopAfterEvidence.camera[key] - cameraBeforeEvidence[key]) > 0.001
  ));
  const snapshots = [desktopBefore, desktopAfterEvidence, desktopAfterCanvas, narrow, short];
  for (const state of snapshots) {
    assert(state.atlasMode === "learn", `${state.viewport.width}x${state.viewport.height} left learner mode`);
    assert(state.geometry.panelViewOverlapArea === 0,
      `${state.viewport.width}x${state.viewport.height} overlapped the lesson and visual`);
    assert(state.geometry.contentInsideView,
      `${state.viewport.width}x${state.viewport.height} cropped the segmented visual`);
    assert(!state.visible.dock && !state.visible.drawTools && !state.visible.objectTree
      && !state.visible.objectControl && !state.visible.segmentBadge,
    `${state.viewport.width}x${state.viewport.height} exposed authoring controls`);
    assert(!state.visible.presenceTray,
      `${state.viewport.width}x${state.viewport.height} overlaid collaboration presence on the lesson`);
  }
  assert(!cameraMovedByEvidence, "clicking lesson evidence moved the camera");
  assert(canvasClick?.isTrusted === true, "the visual itself did not receive a trusted learner click");
  assert(String(desktopBefore.panelParent).includes("atlas-pane"),
    `the lesson panel is still mounted in ${desktopBefore.panelParent}`);
  return {
    status: "generic-learner-ux-closed-loop",
    trusted: { evidenceClick, canvasClick },
    cameraMovedByEvidence,
    desktopBefore,
    desktopAfterEvidence,
    desktopAfterCanvas,
    narrow,
    short,
  };
}
