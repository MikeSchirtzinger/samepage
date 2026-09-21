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
        clientInfo: { name: "atlas-epsilon-lesson-proof", version: "1" },
      },
    }),
  });
  const sessionId = response.headers.get("mcp-session-id");
  const body = parseMcpBody(await response.text());
  if (!response.ok || body.error || !sessionId) {
    throw new Error(`MCP initialize failed: HTTP ${response.status}, ${JSON.stringify(body.error || body)}`);
  }
  let requestId = 1;
  return async (name, args) => {
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
    if (failed) throw new Error(`MCP ${name} failed: ${text || JSON.stringify(result.error || result)}`);
    return text;
  };
}

async function waitForReplica(api, timeoutMs = 30_000) {
  return api.waitFor(`(() => {
    const sync = document.querySelector("#sync-state")?.textContent || "";
    return window.atlas?.schema?.ready === true && sync === "replica live";
  })()`, { timeoutMs, retryOnError: true });
}

async function waitForReplicaEquality(api, timeoutMs = 20_000) {
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
  assert(receipt?.sourceSize?.[0] === expected.source_size[0]
    && receipt?.sourceSize?.[1] === expected.source_size[1], `${label} used the wrong source dimensions`);
  assert(Number.isFinite(receipt?.encodeMs) && receipt.encodeMs > 0, `${label} has no real encoder time`);
  assert(Number.isFinite(receipt?.decodeMs) && receipt.decodeMs > 0, `${label} has no real decoder time`);
  assert(Number.isFinite(receipt?.sourceMaskAreaPixels) && receipt.sourceMaskAreaPixels > 0,
    `${label} produced an empty source mask`);
  const semanticAreaRanges = {
    root: [500_000, 1_550_000],
    subtract: [180_000, 320_000],
    measure: [150_000, 320_000],
    inside: [150_000, 320_000],
    l2: [250, 80_000],
    linf: [250, 80_000],
  };
  const [minimumArea, maximumArea] = semanticAreaRanges[key];
  assert(receipt.sourceMaskAreaPixels >= minimumArea && receipt.sourceMaskAreaPixels <= maximumArea,
    `${label} mask area ${receipt.sourceMaskAreaPixels} did not isolate its intended infographic region`);
  const point = receipt?.sourcePoints?.[0];
  const box = expected.point_boxes?.[key];
  assert(Array.isArray(point) && Array.isArray(box), `${label} has no verifiable source point or semantic region`);
  assert(point[0] >= box[0] && point[1] >= box[1] && point[0] <= box[2] && point[1] <= box[3],
    `${label} trusted click landed outside its declared infographic region`);
  assert(receipt?.fallbackUsed === false, `${label} used a fallback`);
  assert(receipt?.mockUsed === false, `${label} used a mock`);
}

async function acceptMask(api, expected, key, label, expectedCount, screenshotName = null) {
  await api.trustedFill(".segment-picker-label input", label);
  const point = expected.points[key];
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

async function createLessonSegments(api, input) {
  const expected = input.expected;
  await waitForReplica(api);
  const initial = await api.evaluate("window.atlas.segment.snapshot()");
  assert(initial.length === 0, "isolated epsilon lesson did not start empty");
  await api.screenshot("00-empty-board.png");

  await api.trustedClick(".segment-picker-launcher");
  await api.waitFor(`(() => {
    const picker = window.atlas?.segment.picker();
    return picker?.open === true
      && picker.sourceId === ${JSON.stringify(expected.source_id)}
      && picker.state === "ready";
  })()`, { timeoutMs: expected.mask_timeout_ms, retryOnError: true });
  await api.screenshot("01-picker-ready.png");

  const created = {};
  created.root = await acceptMask(api, expected, "root", "Worked example infographic", 1, "02-infographic-mask.png");
  const rootId = created.root.accepted.stableObjectId;
  assert(rootId, "the human infographic selection did not mint a stable Atlas id");
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

  created.subtract = await acceptMask(api, expected, "subtract", "1 SUBTRACT panel", 2, "03-subtract-panel-mask.png");
  created.measure = await acceptMask(api, expected, "measure", "2 MEASURE panel", 3, "04-measure-panel-mask.png");
  created.inside = await acceptMask(api, expected, "inside", "YES -> Inside", 4, "05-compare-panel-mask.png");
  created.l2 = await acceptMask(api, expected, "l2", "L2: circle", 5, "06-l2-circle-mask.png");
  created.linf = await acceptMask(api, expected, "linf", "L-infinity: square", 6, "07-linfinity-square-mask.png");
  await api.trustedClick(".segment-picker-finish");
  await api.trustedClick(".zoom-fit");
  await api.sleep(800);
  await api.screenshot("08-segmented-lesson.png");

  const segments = await api.evaluate("window.atlas.segment.snapshot()");
  const byLabel = Object.fromEntries(segments.map((segment) => [segment.label, segment]));
  const ids = {
    root: byLabel["Worked example infographic"]?.id,
    subtract: byLabel["1 SUBTRACT panel"]?.id,
    measure: byLabel["2 MEASURE panel"]?.id,
    inside: byLabel["YES -> Inside"]?.id,
    l2: byLabel["L2: circle"]?.id,
    linf: byLabel["L-infinity: square"]?.id,
  };
  assert(Object.values(ids).every(Boolean), "the six lesson segments were not all present");
  assert(new Set(Object.values(ids)).size === 6, "the lesson segments did not receive six stable ids");
  for (const [key, id] of Object.entries(ids)) {
    const shape = segments.find((candidate) => candidate.id === id);
    assert(shape?.segment_status === "materialized", `${key} is not materialized`);
    assert(shape?.created_by === "human", `${key} is not attributed to the human`);
    assert(shape?.segment_source_sha256 === expected.source_sha256, `${key} used the wrong source digest`);
    if (key !== "root") assert(shape?.segment_parent_id === ids.root, `${key} is not attached to the neighborhood root`);
  }
  const initialRendering = await api.waitFor(`(() => {
    const rendered = window.atlas?.segment?.rendered?.() || [];
    const root = rendered.find((segment) => segment.id === ${JSON.stringify(ids.root)});
    const children = rendered.filter((segment) => segment.id !== ${JSON.stringify(ids.root)});
    return root?.canvas?.sourcePreserved === "true"
      && root.canvas.hierarchyPartsSubtracted === "0"
      && children.length === 5
      && children.every((segment) => segment.canvas?.redundantSourceOverlay === "true")
      ? { root: root.canvas, children: children.map((segment) => segment.canvas) }
      : false;
  })()`, { timeoutMs: 10_000, retryOnError: true });
  const equality = await waitForReplicaEquality(api);
  return {
    ids,
    human_inputs: Object.fromEntries(Object.entries(created).map(([key, value]) => [key, value.trustedInput])),
    model_receipts: Object.fromEntries(Object.entries(created).map(([key, value]) => [key, value.accepted.receipt])),
    initial_source_preserving_render: initialRendering,
    host_browser_equal: equality.equal,
  };
}

async function conversationSnapshot(api) {
  return api.evaluate(`(() => {
    const conversation = document.querySelector("#conversation");
    return {
      state: conversation?.state || null,
      status: conversation?.ui?.status?.textContent || "",
      streaming: conversation?.streaming?.size || 0,
      messages: conversation?.messages?.() || [],
      attached: conversation?.attachedAgents || [],
    };
  })()`);
}

async function sendHumanTurn(api, text, screenshotName) {
  const learnerMode = await api.evaluate("document.body.dataset.atlasMode === 'learn'");
  const before = await conversationSnapshot(api);
  let fill;
  if (learnerMode) {
    await api.waitFor(`(() => {
      const input = document.querySelector(".explanation-response-input");
      const rect = input?.getBoundingClientRect();
      return rect?.width > 0 && rect?.height > 0;
    })()`, { timeoutMs: 10_000, retryOnError: true });
    await api.trustedFill(".explanation-response-input", text);
    fill = await api.trustedClick('.explanation-response-form button[type="submit"]');
  } else {
    await api.trustedClick('[data-dock-tab="chat"]');
    await api.trustedShadowFill("#conversation", "#input", text);
    fill = await api.trustedShadowClick("#conversation", ".send");
  }
  const complete = await api.waitFor(`(() => {
    const conversation = document.querySelector("#conversation");
    const messages = conversation?.messages?.() || [];
    const last = messages.at(-1);
    return messages.length >= ${before.messages.length + 2}
      && last?.by !== "you"
      && last?.text?.trim().length > 0
      && conversation.streaming.size === 0
      && conversation.state !== "thinking"
      ? { state: conversation.state, status: conversation.ui.status.textContent, messages }
      : false;
  })()`, { timeoutMs: 420_000, retryOnError: true });
  if (screenshotName) await api.screenshot(screenshotName);
  return { input: text, trusted_send: fill, ...complete, response: complete.messages.at(-1).text };
}

function includesEvery(text, fragments, label) {
  const canonical = (value) => value.toLowerCase().replace(/\s+/g, "");
  const canonicalText = canonical(text);
  for (const fragment of fragments) {
    assert(canonicalText.includes(canonical(fragment)), `${label} omitted ${fragment}`);
  }
}

function assertCleanLearnerResponse(text, label) {
  assert(
    !/reading done|let me|now (?:storing|reading)|motion confirmed|layout is clean/i.test(text),
    `${label} exposed internal tool narration`,
  );
  assert(!text.includes("**") && !text.includes("`"),
    `${label} used Markdown markers that the plain-text chat would show literally`);
}

function epsilonLessonFlow(ids) {
  const point = (targetId, cueSpan = null) => ({
    kind: "point",
    target_id: targetId,
    ...(cueSpan ? { cue_span: cueSpan } : {}),
  });
  const evidence = (targetId, detail) => ({ target_id: targetId, detail });
  return {
    schema: "atlas-explanation-flow-v1",
    id: "epsilon-hypersphere-v1",
    title: "Epsilon neighborhoods",
    goal: "Classify points by subtracting the center, measuring with the named norm, and comparing with epsilon.",
    start: "worked-example",
    beats: [
      {
        id: "worked-example",
        title: "Follow one point through the rule",
        intent: "Make each intermediate value visible before asking for a classification.",
        cue: "This picture is a two-dimensional slice of an epsilon neighborhood. First q-c=(1.5,0.5). Next the L2 norm is sqrt(1.5^2+0.5^2)=sqrt(2.5), approximately 1.58. Finally 1.58 is less than or equal to epsilon=2, so q is Inside. Equality is included, so boundary points belong too.",
        evidence: [
          evidence(ids.subtract, "Subtract produces the displacement (1.5,0.5)."),
          evidence(ids.measure, "The named L2 norm turns the displacement into the distance 1.58."),
          evidence(ids.inside, "The less-than-or-equal comparison produces the classification Inside."),
        ],
        actions: [
          point(ids.subtract, [56, 82]),
          point(ids.measure, [88, 173]),
          point(ids.inside, [180, 255]),
        ],
        advance: {
          mode: "agent",
          prompt: "Does q belong? Choose the visible result.",
          transitions: [
            {
              id: "inside",
              label: "Inside",
              control: "button",
              next: "transfer",
              target_ids: [ids.inside],
              phrases: ["inside", "yes", "belongs", "1.58 is less than or equal to 2"],
            },
            {
              id: "beyond",
              label: "Beyond",
              control: "button",
              next: "worked-correction",
              target_ids: [],
              phrases: ["beyond", "no", "does not belong"],
            },
          ],
        },
      },
      {
        id: "worked-correction",
        title: "Use the decisive comparison",
        intent: "Repair the classification from the visible numerical evidence.",
        cue: "The measured distance is 1.58 and epsilon is 2. Because 1.58 is less than or equal to 2, the visible result is Inside. The boundary is included by the equality case.",
        evidence: [evidence(ids.inside, "The compare panel contains the decision and the equality rule.")],
        actions: [point(ids.inside)],
        advance: {
          mode: "continue",
          prompt: "Use this comparison, then continue.",
          transitions: [{ id: "continue", label: "Continue", next: "transfer", target_ids: [], phrases: [] }],
        },
      },
      {
        id: "transfer",
        title: "Classify a point not shown",
        intent: "Transfer the same three operations instead of recalling the pictured answer.",
        cue: "Now use c=(1,1), epsilon=2, r=(3,2.5), and L2. Subtract to get r-c, measure its L2 norm, then compare that distance with 2.",
        evidence: [
          evidence(ids.subtract, "Reuse the subtraction operation, not the old numbers."),
          evidence(ids.measure, "Reuse the named norm to produce one distance."),
          evidence(ids.inside, "Reuse the same threshold comparison to decide Inside or Beyond."),
        ],
        actions: [point(ids.subtract), point(ids.measure), point(ids.inside)],
        advance: {
          mode: "agent",
          prompt: "Is r Inside or Beyond? Show the three steps.",
          transitions: [
            {
              id: "correct-transfer",
              label: "Beyond with the complete calculation",
              next: "norm-contrast",
              target_ids: [],
              phrases: ["(2,1.5)", "sqrt(6.25)", "2.5", "greater than 2", "beyond"],
            },
            {
              id: "needs-transfer-work",
              label: "Incomplete or incorrect transfer",
              next: "transfer-support",
              target_ids: [],
              phrases: ["inside", "beyond without calculation", "not sure"],
            },
          ],
        },
      },
      {
        id: "transfer-support",
        title: "Expose the missing intermediate values",
        intent: "Show exactly where the transfer decision comes from.",
        cue: "r-c=(2,1.5). Its L2 norm is sqrt(2^2+1.5^2)=sqrt(6.25)=2.5. Since 2.5 is greater than 2, r is Beyond.",
        evidence: [evidence(ids.measure, "The distance 2.5 is the value compared with epsilon.")],
        actions: [point(ids.measure)],
        advance: {
          mode: "continue",
          prompt: "Continue when the three produced values are clear.",
          transitions: [{ id: "continue", label: "Continue", next: "norm-contrast", target_ids: [], phrases: [] }],
        },
      },
      {
        id: "norm-contrast",
        title: "Change the norm, not the rule",
        intent: "Separate the invariant comparison from the norm-dependent shape and distance.",
        cue: "In two dimensions, equal L2 distance draws a circle. L-infinity uses the largest coordinate change and draws an axis-aligned square. The subtract, measure, compare rule stays the same.",
        evidence: [
          evidence(ids.l2, "L2 measures radial distance and produces circular level sets."),
          evidence(ids.linf, "L-infinity measures the largest coordinate change and produces square level sets."),
        ],
        actions: [point(ids.l2), point(ids.linf)],
        advance: {
          mode: "agent",
          prompt: "For c=(1,1), epsilon=1, q=(1.8,1.8): q is Beyond under L2. Under L-infinity, is it Inside or Beyond? Explain with the distance.",
          transitions: [
            {
              id: "correct-contrast",
              label: "Inside under L-infinity with distance 0.8",
              next: "teach-back",
              target_ids: [ids.linf],
              phrases: ["inside", "max(0.8,0.8)", "0.8", "less than or equal to 1"],
            },
            {
              id: "needs-contrast-work",
              label: "Incorrect norm contrast",
              next: "contrast-support",
              target_ids: [ids.l2],
              phrases: ["beyond", "1.13", "not sure"],
            },
          ],
        },
      },
      {
        id: "contrast-support",
        title: "Measure with each named norm",
        intent: "Repair confusion between L2 and L-infinity distance.",
        cue: "The displacement is (0.8,0.8). Its L2 distance is about 1.13, so it is Beyond for epsilon 1. Its L-infinity distance is max(0.8,0.8)=0.8, so it is Inside.",
        evidence: [
          evidence(ids.l2, "The circle excludes this point because 1.13 is greater than 1."),
          evidence(ids.linf, "The square includes it because 0.8 is less than or equal to 1."),
        ],
        actions: [point(ids.l2), point(ids.linf)],
        advance: {
          mode: "continue",
          prompt: "Continue when the two distances are distinct.",
          transitions: [{ id: "continue", label: "Continue", next: "teach-back", target_ids: [], phrases: [] }],
        },
      },
      {
        id: "teach-back",
        title: "State the reusable rule",
        intent: "Check that the learner can generalize beyond the two-dimensional slice.",
        cue: "The same inequality works in any number of dimensions. Hypersphere often names the boundary; ball or neighborhood names the filled set, including its boundary when the comparison uses less than or equal.",
        evidence: [evidence(ids.root, "The infographic is one two-dimensional worked slice of the dimension-independent rule.")],
        actions: [{ kind: "reveal", target_id: ids.root }],
        advance: {
          mode: "agent",
          prompt: "Teach the subtract, measure, compare rule back in your own words.",
          transitions: [
            {
              id: "complete",
              label: "Complete rule with boundary inclusion",
              next: "complete",
              target_ids: [],
              phrases: ["subtract", "measure", "compare", "at most epsilon", "boundary"],
            },
            {
              id: "needs-teach-back-work",
              label: "Teach-back is missing a step",
              next: "teach-back-support",
              target_ids: [],
              phrases: ["not sure", "distance only", "inside only"],
            },
          ],
        },
      },
      {
        id: "teach-back-support",
        title: "Name all three produced facts",
        intent: "Make the reusable procedure explicit after an incomplete summary.",
        cue: "Subtract produces a displacement. The named norm measures that displacement to produce one distance. Comparing that distance with epsilon produces Inside or Beyond, and equality belongs to Inside.",
        evidence: [evidence(ids.root, "The full worked example binds all three operations into one decision.")],
        actions: [{ kind: "reveal", target_id: ids.root }],
        advance: {
          mode: "continue",
          prompt: "Continue to finish the lesson.",
          transitions: [{ id: "continue", label: "Finish", next: "complete", target_ids: [], phrases: [] }],
        },
      },
      {
        id: "complete",
        title: "Lesson complete",
        intent: "Leave the reusable procedure and its boundary convention visible.",
        cue: "You can now classify a candidate in any named norm: subtract the center, measure the displacement, and compare the distance with epsilon. Less than or equal means the boundary is included.",
        evidence: [evidence(ids.inside, "The visible Inside result preserves the equality case.")],
        actions: [point(ids.inside)],
        advance: { mode: "terminal", prompt: "", transitions: [] },
      },
    ],
  };
}

async function prepareLesson(api, input, setup) {
  const mcp = await openMcp(input.base_url, input.mcp_token);
  const flow = epsilonLessonFlow(setup.ids);
  const flowResult = await mcp("atlas_explanation_define", { flow });
  assert(flowResult.includes("revision=1"), "explanation definition did not begin at revision 1");
  const browser = await api.waitFor(`(() => {
    const flow = window.atlas?.explanation?.snapshot?.();
    const panel = window.atlas?.explanation?.panel?.();
    return flow?.definition?.id === "epsilon-hypersphere-v1"
      && flow.state?.current_beat === "worked-example"
      && flow.state?.revision === 1
      && panel?.beatId === "worked-example"
      && panel?.status === "active"
      ? { flow, panel, actionLog: window.atlas.explanation.actionLog() }
      : false;
  })()`, { timeoutMs: 30_000, retryOnError: true });
  const fittedForExplanation = await api.evaluate(`(() => {
    const button = document.querySelector(".zoom-fit");
    if (!(button instanceof HTMLButtonElement)) throw new Error("Fit control is unavailable");
    button.click();
    const flowPanel = document.querySelector(".atlas-explanation-flow");
    const root = window.atlas.segment.snapshot()
      .find((segment) => segment.id === ${JSON.stringify(setup.ids.root)});
    const visual = [...document.querySelectorAll("g.shape")]
      .find((element) => element.dataset.id === root.id);
    if (!(flowPanel instanceof HTMLElement) || !(visual instanceof SVGGElement)) {
      throw new Error("Fit proof could not resolve the lesson panel and infographic");
    }
    const panel = flowPanel.getBoundingClientRect();
    const infographic = visual.getBoundingClientRect();
    const overlapWidth = Math.max(0, Math.min(panel.right, infographic.right)
      - Math.max(panel.left, infographic.left));
    const overlapHeight = Math.max(0, Math.min(panel.bottom, infographic.bottom)
      - Math.max(panel.top, infographic.top));
    return {
      ...window.atlas.camera,
      panel: { left: panel.left, top: panel.top, right: panel.right, bottom: panel.bottom },
      infographic: {
        left: infographic.left,
        top: infographic.top,
        right: infographic.right,
        bottom: infographic.bottom,
      },
      overlapArea: overlapWidth * overlapHeight,
    };
  })()`);
  assert(fittedForExplanation.overlapArea === 0,
    `explanation-aware Fit put ${fittedForExplanation.overlapArea}px² of the infographic under the panel`);
  await clearVisualChrome(api);
  await api.evaluate(`(() => {
    const view = document.querySelector("#atlas-view");
    if (!(view instanceof HTMLElement)) throw new Error("Atlas view is unavailable");
    view.focus({ preventScroll: true });
    return document.activeElement === view;
  })()`);
  await api.trustedKey("Escape", "Escape");
  await api.waitFor("window.atlas?.selection === null", { timeoutMs: 5_000, retryOnError: true });
  return {
    flow,
    flow_result: flowResult,
    explanation_aware_camera: fittedForExplanation,
    browser,
  };
}

async function clearVisualChrome(api) {
  const helpOpen = await api.evaluate("Boolean(document.querySelector('details.canvas-help[open] > summary'))");
  if (helpOpen) await api.trustedClick("details.canvas-help > summary");
  const changesOpen = await api.evaluate(`(() => {
    const button = document.querySelector(".atlas-changes-summary[aria-expanded='true']");
    const rect = button?.getBoundingClientRect();
    return Boolean(button && rect?.width > 0 && rect?.height > 0);
  })()`);
  if (changesOpen) await api.trustedClick(".atlas-changes-summary");
}

async function setObjectTreeCollapsed(api, collapsed) {
  const current = await api.evaluate("document.querySelector('.segment-tree-panel')?.classList.contains('collapsed') === true");
  if (current !== collapsed) await api.trustedClick(".segment-tree-collapse");
  await api.waitFor(`document.querySelector('.segment-tree-panel')?.classList.contains('collapsed') === ${JSON.stringify(collapsed)}`,
    { timeoutMs: 5_000, retryOnError: true });
}

async function waitForDirectAnswer(api, before, transitionId, nextBeat) {
  const attention = await api.waitFor(`(() => {
    const targets = [...document.querySelectorAll(".explanation-target-attention")];
    const running = targets.flatMap((target) => target.getAnimations())
      .some((animation) => animation.playState === "running");
    return targets.length > 0 && running
      ? { targetCount: targets.length, running }
      : false;
  })()`, { timeoutMs: 1_500, retryOnError: true });
  const complete = await api.waitFor(`(() => {
    const conversation = document.querySelector("#conversation");
    const messages = conversation?.messages?.() || [];
    const input = window.atlas?.explanation?.inputEvents?.().find((entry) => (
      entry.transitionId === ${JSON.stringify(transitionId)}
        && entry.trusted === true
        && entry.hostCommitted === true
        && entry.agentAccepted === true
    ));
    const flow = window.atlas?.explanation?.snapshot?.();
    const last = messages.at(-1);
    return input
      && flow?.state?.current_beat === ${JSON.stringify(nextBeat)}
      && messages.length >= ${before.messages.length + 2}
      && last?.by !== "you"
      && last?.text?.trim().length > 0
      && conversation.streaming.size === 0
      && conversation.state !== "thinking"
      ? { messages, input, flow }
      : false;
  })()`, { timeoutMs: 420_000, retryOnError: true });
  const settled = await api.waitFor(`(() => {
    const targets = [...document.querySelectorAll(".explanation-target-attention")];
    return targets.length === 0 ? { attentionClassCount: 0 } : false;
  })()`, { timeoutMs: 3_000, retryOnError: true });
  return {
    ...complete,
    response: complete.messages.at(-1).text,
    attention,
    settled,
  };
}

async function teachLesson(api, input, setup) {
  const provider = await api.waitFor(`fetch("/provider")
    .then((response) => response.json())
    .then((value) => value.ready === true && value.current === "claude" ? value : false)`, {
    timeoutMs: 60_000,
    retryOnError: true,
  });
  assert(provider?.ready === true, `managed provider is not ready: ${JSON.stringify(provider)}`);
  assert(provider?.current === "claude",
    `managed provider is not Claude: ${JSON.stringify(provider)}`);

  const start = await sendHumanTurn(
    api,
    "Start the Epsilon hypersphere lesson. Teach me with the page, then quiz me.",
    "05-agent-first-question.png",
  );
  assertCleanLearnerResponse(start.response, "first lesson turn");
  includesEvery(start.response, ["(1.5,0.5)", "1.58", "2", "Inside"], "first lesson turn");
  assert(/does\s+`?q`?\s+belong/i.test(start.response), "first lesson turn omitted the guided belonging check");

  const lessonState = await api.waitFor(`(() => {
    const text = window.atlas?.doc?.describe?.() || "";
    const root = window.atlas?.segment?.snapshot?.().find((shape) => shape.label === "Worked example infographic");
    const flow = window.atlas?.explanation?.snapshot?.();
    const actions = window.atlas?.explanation?.actionLog?.() || [];
    const direct = [...document.querySelectorAll(".explanation-direct-answer")]
      .map((button) => button.textContent.trim());
    return text.includes("YES -> Inside")
      && !root?.segment_motion
      && flow?.state?.current_beat === "worked-example"
      && flow.state.revision === 1
      && flow.definition.beats[0].advance.transitions.every((transition) => transition.control === "button")
      && direct.includes("Inside")
      && direct.includes("Beyond")
      && ![...document.querySelectorAll("button")].some((button) => button.textContent.trim() === "Answer in chat")
      && actions.filter((action) => action.beatId === "worked-example" && action.kind === "point").length === 3
      && !actions.some((action) => action.kind === "replay-motion")
      ? { text, flow, actions, direct }
      : false;
  })()`, { timeoutMs: 60_000, retryOnError: true });
  const learnerUrl = new URL("/", input.base_url);
  learnerUrl.searchParams.set("experiment", "epsilon-hypersphere-live");
  learnerUrl.searchParams.set("segment-recipe", input.expected.source_id);
  learnerUrl.searchParams.set("atlas-mode", "learn");
  await api.navigate(learnerUrl.href);
  await waitForReplica(api);
  const learnerSurface = await api.waitFor(`(() => {
    const visible = (selector) => {
      const element = document.querySelector(selector);
      if (!element) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== "none" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
    };
    const view = document.querySelector("#atlas-view")?.getBoundingClientRect();
    const panel = document.querySelector(".atlas-explanation-flow")?.getBoundingClientRect();
    const overlapWidth = view && panel ? Math.max(0, Math.min(view.right, panel.right) - Math.max(view.left, panel.left)) : 0;
    const overlapHeight = view && panel ? Math.max(0, Math.min(view.bottom, panel.bottom) - Math.max(view.top, panel.top)) : 0;
    const flow = window.atlas?.explanation?.snapshot?.();
    return document.body.dataset.atlasMode === "learn"
      && flow?.state?.current_beat === "worked-example"
      && visible(".atlas-explanation-flow")
      ? {
          authorSurfacesVisible: [".workspace-dock", ".draw-tools", ".segment-tree-panel", ".segment-flap-panel", ".segment-picker"].filter(visible),
          panelViewOverlapArea: overlapWidth * overlapHeight,
          panelParent: document.querySelector(".atlas-explanation-flow")?.parentElement?.className || null,
        }
      : false;
  })()`, { timeoutMs: 30_000, retryOnError: true });
  assert(learnerSurface.authorSurfacesVisible.length === 0,
    `learner mode exposed author surfaces: ${learnerSurface.authorSurfacesVisible.join(", ")}`);
  assert(learnerSurface.panelViewOverlapArea === 0,
    `lesson panel overlapped the visual by ${learnerSurface.panelViewOverlapArea} pixels`);
  await api.sleep(800);
  await api.screenshot("06-agent-visual-scaffold.png");
  const directBefore = await conversationSnapshot(api);
  const cameraBefore = await api.evaluate("({ ...window.atlas.camera })");
  const directClick = await api.trustedClick(
    `.explanation-target-button[data-target-id="${setup.ids.inside}"]`,
  );
  const directAnswer = await waitForDirectAnswer(api, directBefore, "inside", "transfer");
  assertCleanLearnerResponse(directAnswer.response, "direct Inside feedback");
  includesEvery(directAnswer.response, ["Inside", "(3,2.5)"], "direct Inside feedback");
  const cameraAfter = await api.evaluate("({ ...window.atlas.camera })");
  assert(["x", "y", "scale"].every((key) => Math.abs(cameraAfter[key] - cameraBefore[key]) < 0.001),
    `answer click moved the camera: ${JSON.stringify({ cameraBefore, cameraAfter })}`);
  assert(directAnswer.flow.state.revision === 2, "direct answer did not produce revision 2");
  assert(directAnswer.flow.state.last_transition === "inside", "direct answer chose the wrong branch");
  assert(directAnswer.flow.state.last_response === "Inside", "direct answer stored a hidden replacement response");
  assert(directAnswer.flow.state.last_selected_target_ids?.includes(setup.ids.inside),
    "direct visual answer omitted the selected evidence target");
  assert(directAnswer.input.source === "evidence-target", "direct answer lost its visual input source");
  assert(!/please answer|one quiz label|answer with (?:one|the word)|must say/i.test(directAnswer.response),
    "the agent asked the learner to repeat a committed button answer");
  await api.screenshot("07-one-click-inside-answer.png");
  await api.evaluate(`(() => {
    const view = document.querySelector("#atlas-view");
    if (!(view instanceof HTMLElement)) throw new Error("Atlas view is unavailable");
    view.focus({ preventScroll: true });
    return document.activeElement === view;
  })()`);
  await api.trustedKey("Escape", "Escape");
  await api.waitFor("window.atlas?.selection === null", { timeoutMs: 5_000, retryOnError: true });

  const transfer = await sendHumanTurn(
    api,
    "Beyond. r-c=(2,1.5), its L2 distance is sqrt(6.25)=2.5, and 2.5 is greater than 2.",
    "13-transfer-quiz-feedback.png",
  );
  assertCleanLearnerResponse(transfer.response, "transfer quiz feedback");
  includesEvery(transfer.response, ["1.8", "Inside or Beyond"], "transfer quiz feedback");
  assert(/L[- ]?infinity|L∞/i.test(transfer.response),
    "transfer feedback did not name the next problem's L-infinity norm");
  await api.waitFor(`(() => {
    const flow = window.atlas?.explanation?.snapshot?.();
    return flow?.state?.current_beat === "norm-contrast"
      && flow.state.revision === 3
      && flow.state.last_transition === "correct-transfer";
  })()`, { timeoutMs: 20_000, retryOnError: true });

  const contrast = await sendHumanTurn(
    api,
    "Inside, because max(0.8,0.8)=0.8 and 0.8 is less than or equal to 1.",
    "14-contrast-feedback.png",
  );
  assertCleanLearnerResponse(contrast.response, "norm contrast feedback");
  await api.waitFor(`(() => {
    const flow = window.atlas?.explanation?.snapshot?.();
    return flow?.state?.current_beat === "teach-back"
      && flow.state.revision === 4
      && flow.state.last_transition === "correct-contrast"
      ? flow
      : false;
  })()`, { timeoutMs: 30_000, retryOnError: true });
  assert(/explain|rule|without looking|own words/i.test(contrast.response),
    "the agent did not ask for a final rule explanation");

  const teachBack = await sendHumanTurn(
    api,
    "Subtract the center from the point, measure that difference with the chosen norm, and compare it to epsilon. At most epsilon is Inside, including the boundary.",
    "15-completed-teach-back.png",
  );
  assertCleanLearnerResponse(teachBack.response, "teach-back feedback");
  const completedFlow = await api.waitFor(`(() => {
    const flow = window.atlas?.explanation?.snapshot?.();
    return flow?.state?.current_beat === "complete"
      && flow.state.status === "completed"
      && flow.state.revision === 5
      && flow.state.last_transition === "complete"
      ? flow
      : false;
  })()`, { timeoutMs: 20_000, retryOnError: true });

  const equality = await waitForReplicaEquality(api);
  const mcp = await openMcp(input.base_url, input.mcp_token);
  const backendRead = await mcp("atlas_read", {});
  for (const label of [
    "Worked example infographic",
    "1 SUBTRACT panel",
    "2 MEASURE panel",
    "YES -> Inside",
    "L2: circle",
    "L-infinity: square",
  ]) {
    assert(backendRead.includes(label), `backend atlas_read omitted ${label}`);
  }
  assert(backendRead.includes("[complete] \"Lesson complete\""),
    "backend atlas_read omitted the completed explanation cursor");
  const finalState = await api.evaluate(`(() => ({
    segments: window.atlas.segment.snapshot(),
    rendered: window.atlas.segment.rendered(),
    errors: window.atlas.segment.errors(),
    atlas: window.atlas.doc.describe(),
    explanation: window.atlas.explanation.snapshot(),
    explanationActions: window.atlas.explanation.actionLog(),
  }))()`);
  assert(finalState.errors.length === 0, `browser segment errors were reported: ${JSON.stringify(finalState.errors)}`);
  return {
    provider: {
      selected: provider.current,
      ready: provider.ready,
      attached_agents: provider.agents,
    },
    turns: { start, direct_answer: directAnswer, transfer, contrast, teach_back: teachBack },
    direct_answer: {
      trusted_click: directClick,
      learner_surface: learnerSurface,
      camera_before: cameraBefore,
      camera_after: cameraAfter,
      input: directAnswer.input,
      attention: directAnswer.attention,
      attention_settled: directAnswer.settled,
    },
    lesson_state: { flow: lessonState.flow, atlas_text: lessonState.text },
    completed_flow: completedFlow,
    browser: {
      host_browser_equal: equality.equal,
      decorative_motion_present: false,
      one_shot_attention_target_count: directAnswer.attention.targetCount,
      rendered_count: finalState.rendered.length,
      segment_errors: finalState.errors,
      explanation_action_count: finalState.explanationActions.length,
    },
    backend_atlas_read: backendRead,
  };
}

async function setupOnly(api, input) {
  const setup = await createLessonSegments(api, input);
  const prepared = await prepareLesson(api, input, setup);
  return { status: "epsilon-lesson-flow-ready", setup, prepared };
}

async function createAndTeach(api, input) {
  const setup = await createLessonSegments(api, input);
  const prepared = await prepareLesson(api, input, setup);
  const teaching = await teachLesson(api, input, setup);
  return { status: "real-managed-agent-epsilon-lesson", setup, prepared, teaching };
}

async function verifyPersistence(api, input) {
  await waitForReplica(api);
  const state = await api.waitFor(`(() => {
    const segments = window.atlas?.segment?.snapshot?.() || [];
    const rendered = window.atlas?.segment?.rendered?.() || [];
    const atlas = window.atlas?.doc?.describe?.() || "";
    const explanation = window.atlas?.explanation?.snapshot?.();
    const root = segments.find((segment) => segment.label === "Worked example infographic");
    const labels = new Set(segments.map((segment) => segment.label));
    const expected = ${JSON.stringify([
      "Worked example infographic",
      "1 SUBTRACT panel",
      "2 MEASURE panel",
      "YES -> Inside",
      "L2: circle",
      "L-infinity: square",
    ])};
    return segments.length === 6
      && expected.every((label) => labels.has(label))
      && !root?.segment_motion
      && rendered.length === 6
      && atlas.includes("YES -> Inside")
      && explanation?.definition?.id === "epsilon-hypersphere-v1"
      && explanation.state?.current_beat === "complete"
      && explanation.state?.status === "completed"
      && explanation.state?.revision === 5
      ? { segments, rendered, atlas, explanation }
      : false;
  })()`, { timeoutMs: 60_000, retryOnError: true });
  const provider = await api.waitFor(`fetch("/provider")
    .then((response) => response.json())
    .then((value) => value.ready === true && value.current === "claude" ? value : false)`, {
    timeoutMs: 60_000,
    retryOnError: true,
  });
  await api.trustedClick(".zoom-fit");
  await api.sleep(800);
  await api.screenshot("00-persisted-lesson.png");
  const equality = await waitForReplicaEquality(api);
  const mcp = await openMcp(input.base_url, input.mcp_token);
  const backendRead = await mcp("atlas_read", {});
  assert(backendRead.includes("YES -> Inside"), "persisted backend read omitted the compare segment");
  assert(backendRead.includes("[complete] \"Lesson complete\""),
    "persisted backend read omitted the completed lesson cursor");
  const reloadConversation = await conversationSnapshot(api);
  return {
    status: "epsilon-lesson-visual-state-persisted-after-host-restart",
    stable_ids: Object.fromEntries(state.segments.map((segment) => [segment.label, segment.id])),
    decorative_motion_present: false,
    explanation_cursor: state.explanation.state,
    live_transcript_proved_before_restart: true,
    transcript_persistence_claimed: false,
    reload_message_count: reloadConversation.messages.length,
    provider: { selected: provider.current, ready: provider.ready },
    host_browser_equal: equality.equal,
  };
}

export async function run(api, input) {
  if (input.mode === "setup") return setupOnly(api, input);
  if (input.mode === "create-and-teach") return createAndTeach(api, input);
  if (input.mode === "persistence") return verifyPersistence(api, input);
  throw new Error(`unknown epsilon lesson driver mode ${input.mode}`);
}
