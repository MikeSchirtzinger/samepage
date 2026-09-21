function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function waitForReplica(api) {
  return api.waitFor(`(() => window.atlas?.schema?.ready === true
    && document.querySelector("#sync-state")?.textContent === "replica live"
    && Boolean(window.atlas?.explanation?.snapshot?.()))()`, {
    timeoutMs: 30_000,
    retryOnError: true,
  });
}

async function resetToStart(api) {
  const flow = await api.evaluate("window.atlas.explanation.snapshot()");
  if (["active", "paused"].includes(flow.state.status)) {
    await api.waitFor(`Boolean(document.querySelector('[data-explanation-command="stop"]'))`, {
      timeoutMs: 10_000,
      retryOnError: true,
    });
    await api.evaluate(`document.querySelector('[data-explanation-command="stop"]')
      .scrollIntoView({ block: "center" })`);
    await api.trustedClick('[data-explanation-command="stop"]');
    await api.waitFor(`window.atlas.explanation.snapshot().state.status === "stopped"`, {
      timeoutMs: 10_000,
      retryOnError: true,
    });
  }
  await api.waitFor(`Boolean(document.querySelector('[data-explanation-command="restart"]'))`, {
    timeoutMs: 10_000,
    retryOnError: true,
  });
  await api.evaluate(`document.querySelector('[data-explanation-command="restart"]')
    .scrollIntoView({ block: "center" })`);
  await api.trustedClick('[data-explanation-command="restart"]');
  return api.waitFor(`(() => {
    const flow = window.atlas.explanation.snapshot();
    return flow.state.status === "active"
      && flow.state.current_beat === flow.definition.start
      ? flow
      : false;
  })()`, { timeoutMs: 10_000, retryOnError: true });
}

export async function run(api) {
  await api.setViewport(1440, 900);
  await waitForReplica(api);
  assert(await api.evaluate("document.body.dataset.atlasMode === 'learn'"),
    "direct-answer audit did not run in learner mode");
  const started = await resetToStart(api);
  const beat = started.definition.beats.find((candidate) => candidate.id === started.state.current_beat);
  const transition = beat?.advance?.transitions?.find((candidate) => (
    candidate.control === "button" && Array.isArray(candidate.target_ids) && candidate.target_ids.length > 0
  ));
  assert(transition, "the starting beat has no target-bound direct answer");
  await api.waitFor(`Boolean(document.querySelector('.explanation-direct-answer[data-transition-id="${transition.id}"]'))`, {
    timeoutMs: 10_000,
    retryOnError: true,
  });
  await api.evaluate(`document.querySelector('.explanation-direct-answer[data-transition-id="${transition.id}"]')
    .scrollIntoView({ block: "center" })`);
  const before = await api.evaluate(`(() => ({
    camera: { ...window.atlas.camera },
    messages: document.querySelector("#conversation")?.messages?.() || [],
    revision: window.atlas.explanation.snapshot().state.revision,
  }))()`);
  await api.screenshot("01-ready-for-direct-answer.png");
  const trustedClick = await api.trustedClick(
    `.explanation-direct-answer[data-transition-id="${transition.id}"]`,
  );
  const accepted = await api.waitFor(`(() => {
    const flow = window.atlas.explanation.snapshot();
    const input = window.atlas.explanation.inputEvents().find((entry) => (
      entry.transitionId === ${JSON.stringify(transition.id)}
      && entry.trusted === true
      && entry.hostCommitted === true
      && entry.agentAccepted === true
    ));
    const conversation = document.querySelector("#conversation");
    const messages = conversation?.messages?.() || [];
    return input
      && flow.state.current_beat === ${JSON.stringify(transition.next)}
      && messages.length >= ${before.messages.length + 2}
      && conversation.streaming.size === 0
      && conversation.state !== "thinking"
      ? { flow, input, messages, camera: { ...window.atlas.camera } }
      : false;
  })()`, { timeoutMs: 420_000, retryOnError: true });
  const backend = await api.evaluate(`fetch("/atlas/describe", { headers: { accept: "application/json" } })
    .then((response) => response.json())
    .then((payload) => payload.text)`);
  assert(backend.includes(`LAST INPUT: transition=${transition.id}`),
    "backend read-back omitted the direct answer");
  assert(["x", "y", "scale"].every((key) => Math.abs(accepted.camera[key] - before.camera[key]) < 0.001),
    "the direct answer moved the lesson camera");
  assert(accepted.input.source === "answer-button", "the button answer lost its input source");
  await api.screenshot("02-direct-answer-accepted.png");
  const receipt = {
    trustedClick,
    transition: { id: transition.id, label: transition.label, next: transition.next },
    previousRevision: before.revision,
    committedRevision: accepted.flow.state.revision,
    input: accepted.input,
    cameraBefore: before.camera,
    cameraAfter: accepted.camera,
    backendConfirmed: true,
    agentResponse: accepted.messages.at(-1)?.text || "",
  };
  const readyForHuman = await resetToStart(api);
  await api.screenshot("03-reset-for-human.png");
  return {
    status: "learner-button-is-the-answer",
    receipt,
    leftAt: {
      beat: readyForHuman.state.current_beat,
      status: readyForHuman.state.status,
      revision: readyForHuman.state.revision,
    },
  };
}
