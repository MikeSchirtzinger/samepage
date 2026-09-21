function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function parseMcpBody(text) {
  const trimmed = text.trim();
  if (!trimmed) return {};
  if (trimmed.startsWith("{")) return JSON.parse(trimmed);
  const payloads = trimmed
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trim())
    .filter(Boolean);
  if (!payloads.length) throw new Error(`MCP response was neither JSON nor an event stream: ${trimmed.slice(0, 240)}`);
  return JSON.parse(payloads.at(-1));
}

async function openMcp(baseUrl, token) {
  const headers = {
    accept: "application/json, text/event-stream",
    authorization: `Bearer ${token}`,
    "content-type": "application/json",
  };
  const response = await fetch(`${baseUrl}/mcp`, {
    method: "POST",
    headers,
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "atlas-evidence-loop-proof", version: "1" },
      },
    }),
  });
  const sessionId = response.headers.get("mcp-session-id");
  const body = parseMcpBody(await response.text());
  if (!response.ok || body.error || !sessionId) {
    throw new Error(`MCP initialize failed: HTTP ${response.status}, ${JSON.stringify(body.error || body)}`);
  }
  let requestId = 1;
  return async (name, args, options = {}) => {
    requestId += 1;
    const toolResponse = await fetch(`${baseUrl}/mcp`, {
      method: "POST",
      headers: {
        ...headers,
        "mcp-protocol-version": "2025-06-18",
        "mcp-session-id": sessionId,
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: requestId,
        method: "tools/call",
        params: { name, arguments: args },
      }),
    });
    const result = parseMcpBody(await toolResponse.text());
    const failed = !toolResponse.ok || Boolean(result.error) || result.result?.isError === true;
    const text = (result.result?.content || [])
      .filter((entry) => entry?.type === "text")
      .map((entry) => entry.text || "")
      .join("");
    if (failed && options.allowError !== true) {
      throw new Error(`MCP ${name} failed: ${text || JSON.stringify(result.error || result)}`);
    }
    return { failed, text, response: result };
  };
}

async function pageState(api) {
  return api.evaluate(`(() => ({
    sync: document.querySelector("#sync-state")?.textContent || "",
    schema: window.atlas?.schema || null,
    segments: window.atlas?.segment.snapshot() || [],
    picker: window.atlas?.segment.picker() || null,
    rendered: window.atlas?.segment.rendered() || [],
    errors: window.atlas?.segment.errors() || [],
  }))()`);
}

async function waitForReplica(api, timeoutMs) {
  return api.waitFor(`(() => {
    const sync = document.querySelector("#sync-state")?.textContent || "";
    return window.atlas?.schema?.ready === true && sync === "replica live";
  })()`, { timeoutMs, retryOnError: true });
}

async function waitForReplicaEquality(api, timeoutMs) {
  return api.waitFor(`(async () => {
    if (!window.atlas?.schema?.ready) return false;
    const mine = window.atlas.doc.describe();
    const response = await fetch("/atlas/describe", { headers: { accept: "application/json" } });
    if (!response.ok) return false;
    const payload = await response.json();
    return payload.ok && mine === payload.text ? { equal: true, text: mine } : false;
  })()`, { timeoutMs, retryOnError: true });
}

function assertRealMaskReceipt(accepted, expected, key, label) {
  assert(accepted?.label === label, `${label} did not retain its user-entered label`);
  assert(accepted?.trustedPointCount === 1, `${label} was not selected by exactly one trusted pointer input`);
  assert(accepted?.sourceId === expected.source_id, `${label} used the wrong generated source id`);
  const receipt = accepted?.receipt;
  assert(receipt?.segmenter === "real-mobilesam-vit-t", `${label} has no real MobileSAM receipt`);
  assert(receipt?.backendRequested === "webgpu", `${label} did not request WebGPU`);
  assert(receipt?.alternateProviderConfigured === false, `${label} configured an alternate provider`);
  assert(receipt?.adapterBoundToRuntime === true, `${label} did not bind the measured adapter to the runtime`);
  assert(receipt?.deviceAcquired === true, `${label} did not acquire a WebGPU device`);
  assert(receipt?.executionLocation === "browser-worker", `${label} did not execute in the browser Worker`);
  assert(receipt?.sourceSha256 === expected.source_sha256, `${label} used the wrong source digest`);
  assert(Array.isArray(receipt?.sourceSize)
    && receipt.sourceSize[0] === expected.source_size[0]
    && receipt.sourceSize[1] === expected.source_size[1], `${label} used the wrong source dimensions`);
  assert(Number.isFinite(receipt?.encodeMs) && receipt.encodeMs > 0, `${label} has no real encoder time`);
  assert(Number.isFinite(receipt?.decodeMs) && receipt.decodeMs > 0, `${label} has no real decoder time`);
  assert(Number.isFinite(receipt?.sourceMaskAreaPixels) && receipt.sourceMaskAreaPixels > 0,
    `${label} produced an empty source mask`);
  const point = receipt?.sourcePoints?.[0];
  const box = expected.point_boxes?.[key];
  assert(Array.isArray(point) && point.length === 2 && Array.isArray(box) && box.length === 4,
    `${label} has no verifiable source point or semantic region`);
  assert(point[0] >= box[0] && point[1] >= box[1] && point[0] <= box[2] && point[1] <= box[3],
    `${label} trusted click landed outside its declared infographic region`);
  assert(receipt?.fallbackUsed === false, `${label} used a fallback`);
  assert(receipt?.mockUsed === false, `${label} used a mock`);
}

async function acceptMask(api, expected, key, label, point, expectedCount, screenshotName) {
  await api.trustedFill(".segment-picker-label input", label);
  const trustedInput = await api.trustedCanvasClick(
    ".segment-picker-canvas",
    point[0],
    point[1],
    expected.source_size,
  );
  await api.waitFor(`(() => {
    const picker = window.atlas?.segment.picker();
    return picker?.state === "ready" && picker.hasSelection === true;
  })()`, { timeoutMs: expected.mask_timeout_ms, retryOnError: true });
  if (screenshotName) await api.screenshot(screenshotName);
  await api.trustedClick(".segment-picker-create");
  const accepted = await api.waitFor(`(() => {
    const picker = window.atlas?.segment.picker();
    const segments = window.atlas?.segment.snapshot() || [];
    return segments.length === ${expectedCount}
      && picker?.lastAccepted?.label === ${JSON.stringify(label)}
      ? picker.lastAccepted
      : false;
  })()`, { timeoutMs: 20_000, retryOnError: true });
  assertRealMaskReceipt(accepted, expected, key, label);
  return { accepted, trustedInput };
}

function evidenceMotion(ids) {
  const track = (label, targetId, delayMs, x, y) => ({
    label,
    target_ids: [targetId],
    duration_ms: 700,
    delay_ms: delayMs,
    loop: false,
    alternate: false,
    curve: [0.25, 0.1, 0.25, 1],
    keyframes: [
      { at: 0, x, y, opacity: 0 },
      { at: 0.65, x: 0, y: 0, opacity: 0.65 },
      { at: 1, x: 0, y: 0, opacity: 0 },
    ],
  });
  return {
    schema: "atlas-segment-motion-v1",
    label: "Observe, segment, act, verify once",
    child_compositing: "overlay",
    tracks: [
      track("Observe the real surface", ids.observe, 0, 0, -8),
      track("Segment the evidence", ids.segment, 750, 8, 0),
      track("Act on stable targets", ids.act, 1500, 0, 8),
      track("Verify the result", ids.verify, 2250, -8, 0),
    ],
  };
}

function evidenceFlow(ids) {
  const beat = (id, title, intent, cue, targetId, detail, next, cueSpan = null) => ({
    id,
    title,
    intent,
    cue,
    evidence: [{ target_id: targetId, detail }],
    actions: [{
      kind: "point",
      target_id: targetId,
      ...(cueSpan ? { cue_span: cueSpan } : {}),
    }],
    advance: {
      mode: "continue",
      prompt: "Continue when this stage is grounded in the visible evidence.",
      transitions: [{ id: `to-${next}`, label: "Continue", next, target_ids: [], phrases: [] }],
    },
  });
  const observe = beat(
    "observe",
    "Observe the real surface",
    "Begin with evidence from the actual system instead of an inferred state.",
    "Observe means capture the real surface and its current context before deciding what it contains.",
    ids.observe,
    "The Observe segment is the first visible stage.",
    "segment",
    [19, 63],
  );
  observe.actions.unshift({ kind: "replay-motion", target_id: ids.root });
  return {
    schema: "atlas-explanation-flow-v1",
    id: "evidence-loop-v1",
    title: "Evidence-first agent loop",
    goal: "Move from real observation to a verified result without treating an unmeasured assumption as proof.",
    start: "observe",
    beats: [
      observe,
      beat(
        "segment",
        "Segment the relevant evidence",
        "Turn a large visual surface into stable, addressable parts.",
        "Segmentation isolates the evidence that matters and gives each accepted part a stable target id.",
        ids.segment,
        "The Segment stage is a distinct materialized part, not a label guessed from prose.",
        "act",
      ),
      beat(
        "act",
        "Act on stable targets",
        "Bind the intended change to exact objects rather than page coordinates.",
        "Act means issue a bounded operation against the stable ids produced by observation and segmentation.",
        ids.act,
        "The Act stage consumes stable targets from the earlier stages.",
        "verify",
      ),
      beat(
        "verify",
        "Verify the real result",
        "Close the loop with measured browser and backend evidence.",
        "Verify means inspect the resulting surface and shared state. A stored instruction alone is not proof that the browser rendered it.",
        ids.verify,
        "The Verify stage is last because it checks the effect that actually occurred.",
        "complete",
      ),
      {
        id: "complete",
        title: "One loop, four evidence boundaries",
        intent: "Leave the repeatable operating sequence visible.",
        cue: "The repeatable flow is Observe, Segment, Act, Verify. Preparation may use specialist models, but playback and verification refer only to stable Atlas objects.",
        evidence: [{
          target_id: ids.root,
          detail: "The complete infographic remains the durable overview of all four stages.",
        }],
        actions: [{ kind: "reveal", target_id: ids.root }],
        advance: { mode: "terminal", prompt: "", transitions: [] },
      },
    ],
  };
}

async function createEvidenceLoop(api, input) {
  const expected = input.expected;
  await waitForReplica(api, 30_000);
  const initial = await pageState(api);
  assert(initial.segments.length === 0, "isolated evidence-loop run did not start empty");
  await api.screenshot("00-empty-board.png");

  await api.trustedClick(".segment-picker-launcher");
  await api.waitFor(`(() => {
    const picker = window.atlas?.segment.picker();
    return picker?.open === true
      && picker.sourceId === ${JSON.stringify(expected.source_id)}
      && picker.state === "ready";
  })()`, { timeoutMs: expected.mask_timeout_ms, retryOnError: true });
  await api.screenshot("00-picker-ready.png");

  const created = {};
  created.root = await acceptMask(api, expected, "root", "Evidence Loop", expected.points.root, 1, "01-root-mask-ready.png");
  const rootId = created.root.accepted.stableObjectId;
  assert(rootId, "the human root selection did not mint a stable Atlas id");

  await api.waitFor(`(() => {
    const button = document.querySelector(".segment-add-part-button");
    const rect = button?.getBoundingClientRect();
    return window.atlas?.selection?.id === ${JSON.stringify(rootId)}
      && rect?.width > 0 && rect?.height > 0;
  })()`, { timeoutMs: 10_000, retryOnError: true });
  await api.trustedClick(".segment-add-part-button");
  await api.waitFor(`(() => {
    const picker = window.atlas?.segment.picker();
    return picker?.open === true && picker.seriesParentId === ${JSON.stringify(rootId)} && picker.state === "ready";
  })()`, { timeoutMs: 20_000, retryOnError: true });

  created.observe = await acceptMask(api, expected, "observe", "Observe", expected.points.observe, 2, "02-observe-mask-ready.png");
  created.segment = await acceptMask(api, expected, "segment", "Segment", expected.points.segment, 3, null);
  created.act = await acceptMask(api, expected, "act", "Act", expected.points.act, 4, null);
  created.verify = await acceptMask(api, expected, "verify", "Verify", expected.points.verify, 5, "03-all-stage-masks-ready.png");
  await api.trustedClick(".segment-picker-finish");
  await api.trustedClick(".zoom-fit");
  await api.sleep(600);
  await api.screenshot("04-segmented-infographic.png");

  const segments = (await pageState(api)).segments;
  const byLabel = Object.fromEntries(segments.map((segment) => [segment.label, segment]));
  const ids = {
    root: byLabel["Evidence Loop"]?.id,
    observe: byLabel.Observe?.id,
    segment: byLabel.Segment?.id,
    act: byLabel.Act?.id,
    verify: byLabel.Verify?.id,
  };
  assert(Object.values(ids).every(Boolean), "the five infographic segments were not all present");
  assert(new Set(Object.values(ids)).size === 5, "the infographic segments did not receive five stable ids");
  for (const [key, id] of Object.entries(ids)) {
    const shape = segments.find((candidate) => candidate.id === id);
    assert(shape?.segment_status === "materialized", `${key} is not materialized`);
    assert(shape?.created_by === "human", `${key} is not attributed to the human`);
    assert(shape?.segment_source_sha256 === expected.source_sha256, `${key} used the wrong source digest`);
    if (key !== "root") assert(shape?.segment_parent_id === ids.root, `${key} is not attached to the loop root`);
  }

  const mcp = await openMcp(input.base_url, input.mcp_token);
  const preMotionRead = await mcp("atlas_read", {});
  for (const label of ["Evidence Loop", "Observe", "Segment", "Act", "Verify"]) {
    assert(preMotionRead.text.includes(label), `backend atlas_read did not contain ${label}`);
  }

  const motion = evidenceMotion(ids);
  const motionResult = await mcp("atlas_segment_motion", { owner_id: ids.root, motion });
  assert(motionResult.text.includes("saved positions were unchanged"), "backend motion action omitted its geometry invariant");
  await api.waitFor(`(() => {
    const root = window.atlas?.segment.snapshot().find((shape) => shape.id === ${JSON.stringify(ids.root)});
    return root?.segment_motion?.label === "Observe, segment, act, verify once";
  })()`, { timeoutMs: 20_000, retryOnError: true });

  const learnerUrl = new URL("/", input.base_url);
  learnerUrl.searchParams.set("experiment", "evidence-loop-live");
  learnerUrl.searchParams.set("segment-recipe", input.expected.source_id);
  learnerUrl.searchParams.set("atlas-mode", "learn");
  await api.navigate(learnerUrl.href);
  await waitForReplica(api, 30_000);
  const learnerSurface = await api.evaluate(`(() => {
    const visible = (selector) => {
      const element = document.querySelector(selector);
      if (!element) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== "none" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
    };
    return {
      mode: document.body.dataset.atlasMode,
      authorSurfacesVisible: [".workspace-dock", ".draw-tools", ".segment-tree-panel", ".segment-flap-panel", ".segment-picker"].filter(visible),
    };
  })()`);
  assert(learnerSurface.mode === "learn", "the second-topic lesson did not enter learner mode");
  assert(learnerSurface.authorSurfacesVisible.length === 0,
    `the second-topic lesson exposed author surfaces: ${learnerSurface.authorSurfacesVisible.join(", ")}`);
  const resourcesBeforeFlow = await api.evaluate(`(() => ({
    resources: performance.getEntriesByType("resource").map((entry) => entry.name),
    inflight: window.atlas.segment.inflight(),
  }))()`);
  await api.evaluate(`window.atlas.explanation.setClockMode("speech")`);
  const flow = evidenceFlow(ids);
  const flowResult = await mcp("atlas_explanation_define", { flow });
  assert(flowResult.text.includes("revision=1"), "evidence explanation did not begin at revision 1");
  const speechBlocked = await api.waitFor(`(() => {
    const snapshot = window.atlas?.explanation?.snapshot?.();
    const panel = window.atlas?.explanation?.panel?.();
    const actions = window.atlas?.explanation?.actionLog?.() || [];
    const replay = actions.find((entry) => entry.beatId === "observe" && entry.kind === "replay-motion");
    const point = actions.find((entry) => entry.beatId === "observe" && entry.kind === "point");
    return snapshot?.state?.revision === 1
      && panel?.beatId === "observe"
      && replay?.result === "replayed-4"
      && !point
      ? { snapshot, panel, replay, clock: window.atlas.explanation.clock() }
      : false;
  })()`, { timeoutMs: 20_000, retryOnError: true });
  assert(speechBlocked.clock.mode === "speech", "the cue-span test did not use the speech clock");
  await api.evaluate(`window.atlas.explanation.cueProgress({
    flowId: "evidence-loop-v1",
    revision: 1,
    spokenChars: 64,
    status: "playing",
    alignment: "character"
  })`);
  const speechReleased = await api.waitFor(`(() => {
    const action = (window.atlas?.explanation?.actionLog?.() || [])
      .find((entry) => entry.beatId === "observe" && entry.kind === "point");
    return action?.clock === "speech" && action?.result === "highlighted" ? action : false;
  })()`, { timeoutMs: 5_000, retryOnError: true });
  await api.evaluate(`window.atlas.explanation.setClockMode("event")`);

  const beforeInvalid = await api.evaluate(`JSON.stringify(window.atlas.segment.snapshot()
    .find((shape) => shape.id === ${JSON.stringify(ids.root)}).segment_motion)`);
  const invalidMotion = structuredClone(motion);
  invalidMotion.tracks[1].target_ids = [ids.observe, "not-a-real-segment"];
  const negative = await mcp(
    "atlas_segment_motion",
    { owner_id: ids.root, motion: invalidMotion },
    { allowError: true },
  );
  assert(negative.failed === true, "deliberately invalid cross-object motion unexpectedly succeeded");
  const afterInvalid = await api.evaluate(`JSON.stringify(window.atlas.segment.snapshot()
    .find((shape) => shape.id === ${JSON.stringify(ids.root)}).segment_motion)`);
  assert(afterInvalid === beforeInvalid, "invalid motion changed the accepted motion program");
  await api.evaluate(`(() => {
    const view = document.querySelector("#atlas-view");
    if (!(view instanceof HTMLElement)) throw new Error("Atlas view is unavailable");
    view.focus({ preventScroll: true });
    return document.activeElement === view;
  })()`);
  const clearSelection = await api.trustedKey("Escape", "Escape");
  await api.waitFor("window.atlas?.selection === null", { timeoutMs: 5_000, retryOnError: true });

  await api.trustedClick(".explanation-control");
  const paused = await api.waitFor(`(() => {
    const flow = window.atlas?.explanation?.snapshot?.();
    return flow?.state?.status === "paused" && flow.state.revision === 2 ? flow.state : false;
  })()`, { timeoutMs: 5_000, retryOnError: true });
  const staleAdvance = await mcp(
    "atlas_explanation_advance",
    {
      flow_id: "evidence-loop-v1",
      expected_revision: 1,
      transition_id: "to-segment",
      response: "Continue",
      selected_target_ids: [],
    },
    { allowError: true },
  );
  assert(staleAdvance.failed === true, "a stale explanation transition unexpectedly succeeded");
  const afterStale = await api.evaluate("window.atlas.explanation.snapshot().state");
  assert(afterStale.status === "paused" && afterStale.revision === 2 && afterStale.current_beat === "observe",
    "the stale explanation transition changed the cursor");
  await api.trustedClick(".explanation-control");
  const resumed = await api.waitFor(`(() => {
    const flow = window.atlas?.explanation?.snapshot?.();
    return flow?.state?.status === "active" && flow.state.revision === 3 ? flow.state : false;
  })()`, { timeoutMs: 5_000, retryOnError: true });
  const actionsAfterResume = await api.evaluate("window.atlas.explanation.actionLog()");
  assert(actionsAfterResume.filter((entry) => (
    entry.beatId === "observe" && entry.kind === "replay-motion"
  )).length === 1, "pause or resume replayed the current beat motion");

  const transitionLatenciesMs = [];
  for (const [beatId, expectedNext, expectedRevision] of [
    ["observe", "segment", 4],
    ["segment", "act", 5],
    ["act", "verify", 6],
    ["verify", "complete", 7],
  ]) {
    await api.waitFor(`(() => {
      const panel = window.atlas?.explanation?.panel?.();
      const button = document.querySelector(".explanation-primary");
      const rect = button?.getBoundingClientRect();
      return panel?.beatId === ${JSON.stringify(beatId)}
        && panel.status === "active"
        && rect?.width > 0 && rect?.height > 0;
    })()`, { timeoutMs: 5_000, retryOnError: true });
    const started = await api.evaluate("performance.now()");
    await api.trustedClick(".explanation-primary");
    const completed = await api.waitFor(`(() => {
      const panel = window.atlas?.explanation?.panel?.();
      return panel?.beatId === ${JSON.stringify(expectedNext)}
        && panel.revision === ${expectedRevision}
        ? performance.now()
        : false;
    })()`, { timeoutMs: 5_000, retryOnError: true });
    transitionLatenciesMs.push({ from: beatId, to: expectedNext, ms: completed - started });
  }
  const completedFlow = await api.evaluate("window.atlas.explanation.snapshot()");
  assert(completedFlow.state.status === "completed" && completedFlow.state.revision === 7,
    "the generic explanation did not complete through its browser controls");

  const replayed = await api.evaluate(`window.atlas.segment.replay(${JSON.stringify(ids.root)})`);
  assert(replayed === 4, `the evidence flow replay targeted ${replayed} segments instead of four`);
  const samples = [];
  for (let index = 0; index < 14; index += 1) {
    samples.push(await api.evaluate(`window.atlas.segment.rendered().map((segment) => ({
      id: segment.id,
      transform: segment.computedTransform,
      visibility: segment.computedVisibility,
      animations: segment.animations,
    }))`));
    if (index === 0) await api.screenshot("05-animation-frame-a.png");
    if (index === 7) await api.screenshot("06-animation-frame-b.png");
    await api.sleep(250);
  }
  for (const id of [ids.observe, ids.segment, ids.act, ids.verify]) {
    const observations = samples.map((sample) => sample.find((entry) => entry.id === id));
    assert(observations.every(Boolean), `animated segment ${id} disappeared from a sample`);
    assert(observations.some((entry) => entry.animations.some((animation) => animation.playState === "running")),
      `segment ${id} never reported its one-shot browser animation running`);
    assert(new Set(observations.map((entry) => entry.transform)).size >= 2,
      `segment ${id} did not visibly change transform across sampled frames`);
    assert(observations.at(-1).visibility === "hidden",
      `segment ${id} did not hide its overlay after the one-shot animation`);
  }

  const equality = await waitForReplicaEquality(api, 20_000);
  const finalRead = await mcp("atlas_read", {});
  assert(finalRead.text.includes("Observe, segment, act, verify once"), "backend read-back omitted the stored motion program");
  assert(finalRead.text.includes("[complete] \"One loop, four evidence boundaries\""),
    "backend read-back omitted the completed generic explanation");
  const finalState = await pageState(api);
  assert(finalState.errors.length === 0, `browser segment errors were reported: ${JSON.stringify(finalState.errors)}`);
  const resourcesAfterFlow = await api.evaluate(`(() => ({
    resources: performance.getEntriesByType("resource").map((entry) => entry.name),
    inflight: window.atlas.segment.inflight(),
    inputs: window.atlas.explanation.inputEvents(),
    actions: window.atlas.explanation.actionLog(),
  }))()`);
  const newResources = resourcesAfterFlow.resources.filter((name) => !resourcesBeforeFlow.resources.includes(name));
  assert(Object.keys(resourcesBeforeFlow.inflight).length === 0
    && Object.keys(resourcesAfterFlow.inflight).length === 0,
  "an explanation transition left specialist model work in flight");
  assert(newResources.every((name) => !/onnx|mobile[_-]?sam|\.wasm(?:$|\?)/i.test(name)),
    `flow transitions loaded specialist model resources: ${JSON.stringify(newResources)}`);
  assert(resourcesAfterFlow.inputs.filter((entry) => entry.type === "control").every((entry) => entry.trusted === true),
    "pause or resume was not driven by trusted browser input");
  assert(resourcesAfterFlow.inputs.filter((entry) => entry.type === "transition").length === 4
    && resourcesAfterFlow.inputs.filter((entry) => entry.type === "transition").every((entry) => entry.trusted === true),
  "the four explanation transitions were not driven by trusted browser inputs");
  return {
    status: "real-human-segmentation-and-generic-explanation-flow",
    source: {
      id: expected.source_id,
      sha256: expected.source_sha256,
      size: expected.source_size,
    },
    ids,
    human_inputs: Object.fromEntries(Object.entries(created).map(([key, value]) => [key, value.trustedInput])),
    selection_clear_input: clearSelection,
    model_receipts: Object.fromEntries(Object.entries(created).map(([key, value]) => [key, value.accepted.receipt])),
    backend: {
      pre_motion_read_contains_all_labels: true,
      motion_result: motionResult.text,
      invalid_motion_refused: true,
      invalid_motion_error: negative.text,
      invalid_motion_atomic: afterInvalid === beforeInvalid,
      stale_explanation_advance_refused: true,
      stale_explanation_advance_error: staleAdvance.text,
      stale_explanation_advance_atomic: afterStale.status === "paused" && afterStale.revision === 2,
      final_read_contains_motion: true,
    },
    browser: {
      learner_surface: learnerSurface,
      animation_sample_count: samples.length,
      one_shot_animation_ids: [ids.observe, ids.segment, ids.act, ids.verify],
      host_browser_equal: equality.equal,
      rendered_count: finalState.rendered.length,
      segment_errors: finalState.errors,
      paused,
      resumed,
      control_did_not_replay_beat_actions: true,
      completed_flow: completedFlow,
      transition_latencies_ms: transitionLatenciesMs,
      speech_clock_blocked_action: speechBlocked,
      speech_clock_released_action: speechReleased,
      no_specialist_model_resource_load_on_transition: newResources.every((name) => (
        !/onnx|mobile[_-]?sam|\.wasm(?:$|\?)/i.test(name)
      )),
      new_resources_during_flow: newResources,
      trusted_input_events: resourcesAfterFlow.inputs,
      explanation_actions: resourcesAfterFlow.actions,
    },
  };
}

async function verifyPersistence(api, input) {
  await waitForReplica(api, 30_000);
  const labels = ["Evidence Loop", "Observe", "Segment", "Act", "Verify"];
  const state = await api.waitFor(`(() => {
    const segments = window.atlas?.segment.snapshot() || [];
    const labels = new Set(segments.map((segment) => segment.label));
    const explanation = window.atlas?.explanation?.snapshot?.();
    return segments.length === 5 && ${JSON.stringify(labels)}.every((label) => labels.has(label))
      && explanation?.definition?.id === "evidence-loop-v1"
      && explanation.state?.status === "completed"
      && explanation.state?.revision === 7
      ? { segments, rendered: window.atlas.segment.rendered(), errors: window.atlas.segment.errors(), explanation }
      : false;
  })()`, { timeoutMs: 30_000, retryOnError: true });
  const root = state.segments.find((segment) => segment.label === "Evidence Loop");
  assert(root?.segment_motion?.label === "Observe, segment, act, verify once", "motion program did not survive host restart");
  assert(root.segment_motion.tracks?.length === 4
    && root.segment_motion.tracks.every((track) => track.loop === false),
  "the persisted explanation motion is not the four-stage one-shot program");
  assert(state.segments.every((segment) => segment.segment_status === "materialized"),
    "a segment was not materialized after host restart");
  assert(state.segments.every((segment) => segment.segment_source_sha256 === input.expected.source_sha256),
    "a segment source digest changed after host restart");
  assert(state.rendered.length === 5, "five segments were not rendered after host restart");
  assert(state.errors.length === 0, "the browser reported a segment error after host restart");
  await api.trustedClick(".zoom-fit");
  await api.sleep(700);
  await api.screenshot("00-persisted-animation.png");
  const equality = await waitForReplicaEquality(api, 20_000);
  const mcp = await openMcp(input.base_url, input.mcp_token);
  const read = await mcp("atlas_read", {});
  for (const label of [...labels, "Observe, segment, act, verify once", "One loop, four evidence boundaries"]) {
    assert(read.text.includes(label), `persisted backend read-back omitted ${label}`);
  }
  return {
    status: "persisted-after-real-host-restart",
    stable_ids: Object.fromEntries(state.segments.map((segment) => [segment.label, segment.id])),
    stored_motion_label: root.segment_motion.label,
    one_shot_track_count: root.segment_motion.tracks.length,
    explanation_cursor: state.explanation.state,
    host_browser_equal: equality.equal,
    segment_errors: state.errors,
  };
}

export async function run(api, input) {
  if (input.mode === "create") return createEvidenceLoop(api, input);
  if (input.mode === "persistence") return verifyPersistence(api, input);
  throw new Error(`unknown evidence-loop driver mode ${input.mode}`);
}
