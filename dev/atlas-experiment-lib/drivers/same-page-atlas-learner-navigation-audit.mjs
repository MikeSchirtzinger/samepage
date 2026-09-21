function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function waitForWorkspace(api) {
  return api.waitFor(`(() => {
    const catalog = window.atlas?.learning?.catalog?.();
    const snapshot = window.atlas?.doc?.snapshot?.();
    const activeTab = [...document.querySelectorAll("[data-dock-tab]")]
      .find((button) => button.getAttribute("aria-selected") === "true")?.dataset.dockTab;
    return document.body.dataset.atlasMode === "author"
      && document.querySelector("#sync-state")?.textContent === "replica live"
      && catalog?.status === "ready"
      && snapshot
      ? { url: location.href, catalog, snapshot, activeTab }
      : false;
  })()`, { timeoutMs: 30_000, retryOnError: true });
}

async function waitForLesson(api, sourceId) {
  return api.waitFor(`(() => {
    const source = new URLSearchParams(location.search).get("segment-recipe");
    const flow = window.atlas?.explanation?.snapshot?.();
    const catalog = window.atlas?.explanation?.navigation?.catalog?.();
    return source === ${JSON.stringify(sourceId)}
      && document.body.dataset.atlasMode === "learn"
      && document.querySelector("#sync-state")?.textContent === "replica live"
      && Boolean(flow)
      && catalog?.status === "ready"
      ? { source, flow, catalog }
      : false;
  })()`, { timeoutMs: 30_000, retryOnError: true });
}

async function snapshot(api, name) {
  const state = await api.evaluate(`(() => {
    const visible = (selector) => {
      const element = document.querySelector(selector);
      if (!element) return false;
      const style = getComputedStyle(element);
      const bounds = element.getBoundingClientRect();
      return !element.hidden && style.display !== "none" && style.visibility !== "hidden"
        && bounds.width > 0 && bounds.height > 0;
    };
    const activeTab = [...document.querySelectorAll("[data-dock-tab]")]
      .find((button) => button.getAttribute("aria-selected") === "true")?.dataset.dockTab || null;
    const topicCards = [...document.querySelectorAll("[data-learner-topic-id]")].map((link) => ({
      id: link.dataset.learnerTopicId,
      href: link.href,
      title: link.closest("article")?.querySelector("h3, h4")?.textContent || "",
    }));
    return {
      url: location.href,
      origin: location.origin,
      mode: document.body.dataset.atlasMode,
      viewport: { width: innerWidth, height: innerHeight },
      bodyWidth: document.body.scrollWidth,
      activeTab,
      atlasSnapshot: window.atlas?.doc?.snapshot?.() || null,
      flow: window.atlas?.explanation?.snapshot?.() || null,
      catalog: window.atlas?.learning?.catalog?.() || null,
      visibleTopics: window.atlas?.learning?.visibleTopics?.() || [],
      requests: window.atlas?.learning?.requests?.() || [],
      topicCards,
      visible: {
        atlas: visible("#atlas-view"),
        explanation: visible(".atlas-explanation-flow"),
        destination: visible(".atlas-learner-destination"),
        dock: visible(".workspace-dock"),
        learning: visible("#dock-learn"),
        picker: visible(".segment-picker"),
        authorControls: visible(".draw-tools"),
      },
    };
  })()`);
  await api.screenshot(name);
  return state;
}

function returnTarget(href) {
  return new URL(new URL(href).searchParams.get("return-to"));
}

export async function run(api, input) {
  const startSource = input?.expected?.start_source_id;
  const nextSource = input?.expected?.next_source_id;
  assert(typeof startSource === "string" && typeof nextSource === "string" && startSource !== nextSource,
    "tangent audit needs two distinct refresher source ids");

  await api.setViewport(1440, 900);
  const initial = await waitForWorkspace(api);
  const workspace = await snapshot(api, "01-ordinary-workspace.png");
  const initialAtlas = JSON.parse(initial.snapshot);
  assert(workspace.mode === "author" && workspace.visible.atlas && workspace.visible.dock,
    "the audit did not start in the ordinary Atlas workspace");
  assert(!workspace.visible.explanation && !workspace.visible.destination,
    "the ordinary workspace started inside learner chrome");
  assert(initialAtlas.nodes.length === 0 && initialAtlas.shapes.length === 0 && !initialAtlas.explanation,
    "the clean workspace is not an empty canvas baseline");

  const learnClick = await api.trustedClick('[data-dock-tab="learn"]');
  await api.waitFor(`document.querySelector('[data-dock-tab="learn"]')?.getAttribute("aria-selected") === "true"
    && document.querySelectorAll("#dock-learn [data-learner-topic-id]").length >= 2`, {
    timeoutMs: 5_000,
    retryOnError: true,
  });
  const learning = await snapshot(api, "02-learning-tangents-in-workspace.png");
  assert(learning.visible.learning && learning.visible.atlas,
    "opening learning tangents replaced the ordinary canvas");
  assert(learning.topicCards.length >= 2, "the proof catalog did not expose multiple tangents");

  const startCard = learning.topicCards.find((topic) => new URL(topic.href).searchParams.get("segment-recipe") === startSource);
  assert(startCard?.href, "the workspace has no start refresher");
  const startReturn = returnTarget(startCard.href);
  assert(startReturn.origin === workspace.origin
    && startReturn.searchParams.get("workspace-view") === workspace.activeTab,
    "the refresher did not carry the exact ordinary workspace return target");
  const openClick = await api.trustedNavigationClick(`[data-learner-topic-id="${startCard.id}"]`);
  let activeLesson = await waitForLesson(api, startSource);
  if (activeLesson.flow.state.status !== "active") {
    const command = activeLesson.flow.state.status === "paused" ? "resume" : "restart";
    await api.trustedClick(`[data-explanation-command="${command}"]`);
    activeLesson = await waitForLesson(api, startSource);
  }
  const lesson = await snapshot(api, "03-temporary-refresher.png");
  assert(lesson.visible.atlas && lesson.visible.explanation && !lesson.visible.dock,
    "the temporary refresher did not render as one focused learner surface");
  assert(lesson.mode === "learn", "the refresher did not enter learner mode");

  const switchClick = await api.trustedClick('[data-learner-navigation="topics"]');
  await api.waitFor(`window.atlas.explanation.navigation.view() === "topics"
    && document.querySelectorAll(".learner-topic-card").length >= 2`, {
    timeoutMs: 5_000,
    retryOnError: true,
  });
  const chooser = await snapshot(api, "04-switch-refreshers.png");
  const nextCard = chooser.topicCards.find((topic) => new URL(topic.href).searchParams.get("segment-recipe") === nextSource);
  assert(nextCard?.href, "the proof chooser has no second independent refresher");
  assert(returnTarget(nextCard.href).origin === workspace.origin,
    "switching refreshers dropped the workspace return target");
  const nextClick = await api.trustedNavigationClick(`[data-learner-topic-id="${nextCard.id}"]`);
  await waitForLesson(api, nextSource);
  const secondLesson = await snapshot(api, "05-second-independent-refresher.png");
  assert(secondLesson.origin !== lesson.origin, "the second refresher did not reach its independent host");

  const backClick = await api.trustedNavigationClick('.atlas-learner-workspace-link[data-learner-navigation="workspace"]');
  const returned = await waitForWorkspace(api);
  const afterReturn = await snapshot(api, "06-returned-to-ordinary-workspace.png");
  assert(afterReturn.origin === workspace.origin && afterReturn.mode === "author",
    "Back to workspace did not leave learner mode");
  assert(afterReturn.activeTab === workspace.activeTab && afterReturn.visible.atlas && afterReturn.visible.dock,
    "return did not restore the prior ordinary workspace view");
  assert(returned.snapshot === initial.snapshot,
    "the learning tangent changed the main Atlas document");
  assert(!afterReturn.visible.explanation && !afterReturn.visible.destination,
    "learner chrome remained after returning to work");

  const reopenLearn = await api.trustedClick('[data-dock-tab="learn"]');
  await api.waitFor(`document.querySelectorAll("#dock-learn [data-learner-topic-id]").length >= 2`, {
    timeoutMs: 5_000,
    retryOnError: true,
  });
  const removable = (await snapshot(api, "07-before-remove.png")).topicCards.find((topic) => topic.id === nextCard.id);
  assert(removable, "the second refresher was not present before removal");
  const removeClick = await api.trustedClick(`[data-remove-tangent="${removable.id}"]`);
  await api.waitFor(`!document.querySelector('[data-remove-tangent="${removable.id}"]')`, {
    timeoutMs: 5_000,
    retryOnError: true,
  });
  const removed = await snapshot(api, "08-refresher-removed.png");
  assert(!removed.visibleTopics.some((topic) => topic.id === removable.id),
    "Remove left the refresher in the workspace list");
  await api.navigate(removed.url);
  await waitForWorkspace(api);
  const afterReload = await snapshot(api, "09-removal-survives-reload.png");
  assert(!afterReload.visibleTopics.some((topic) => topic.id === removable.id),
    "removed refresher returned after reload");

  await api.trustedClick('[data-dock-tab="learn"]');
  const requestFill = await api.trustedFill(".learning-request-input", "the ownership boundary in this architecture");
  const requestClick = await api.trustedClick('.learning-request-form button[type="submit"]');
  await api.waitFor(`window.atlas.learning.requests().length === 1`, {
    timeoutMs: 5_000,
    retryOnError: true,
  });
  const requested = await snapshot(api, "10-refresher-request-sent-from-work.png");
  assert(requested.requests[0]?.trusted === true
    && requested.requests[0]?.schema === "agui-direct-interaction-v1"
    && requested.requests[0]?.kind === "request"
    && requested.requests[0]?.source === "atlas.learning",
  "new refresher request did not preserve trusted structured context");
  assert(requested.atlasSnapshot === initial.snapshot,
    "asking for clarification changed the main canvas before the agent answered");

  await api.setViewport(820, 900);
  await api.sleep(350);
  const narrow = await snapshot(api, "11-ordinary-workspace-narrow.png");
  assert(narrow.bodyWidth <= narrow.viewport.width,
    `narrow workspace overflows horizontally: ${narrow.bodyWidth} > ${narrow.viewport.width}`);
  await api.setViewport(1440, 900);
  await api.sleep(350);
  await api.trustedClick('[data-dock-tab="board"]');
  const readyForHuman = await snapshot(api, "12-ordinary-workspace-ready.png");

  return {
    status: "learning-tangents-return-to-ordinary-work",
    trusted: {
      learnClick,
      openClick,
      switchClick,
      nextClick,
      backClick,
      reopenLearn,
      removeClick,
      requestFill,
      requestClick,
    },
    workspaceUnchanged: true,
    removalPersisted: true,
    workspace,
    learning,
    lesson,
    chooser,
    secondLesson,
    afterReturn,
    removed,
    afterReload,
    requested,
    narrow,
    readyForHuman,
  };
}
