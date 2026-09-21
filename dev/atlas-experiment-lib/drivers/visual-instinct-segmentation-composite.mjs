function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function positiveTimes(values) {
  return Array.isArray(values)
    && values.length > 0
    && values.every((value) => Number.isFinite(value) && value > 0);
}

async function compositeReceipt(api) {
  return api.evaluate("window.__visualInstinctLab.segmentationCompositeReceipt()");
}

async function waitForControl(api, value, previousSerial) {
  await api.waitFor(
    `(() => {
      const receipt = window.__visualInstinctLab?.segmentationCompositeReceipt?.();
      return receipt?.control?.value === ${value}
        && receipt?.frame?.serial > ${previousSerial}
        && typeof receipt?.frame?.sha256 === "string";
    })()`,
    { timeoutMs: 20_000 },
  );
  return compositeReceipt(api);
}

export async function run(api, input) {
  await api.screenshot("00-loaded.png");
  const available = await api.evaluate(
    "typeof window.__visualInstinctLab?.runSegmentationComposite === 'function'",
  );
  assert(available, "the segmentation composite entry point is unavailable");

  await api.evaluate(
    "document.querySelector('#run-segmentation-composite').scrollIntoView({ block: 'center' })",
  );
  await api.screenshot("01-composite-control-visible.png");
  await api.trustedClick("#run-segmentation-composite");
  await api.waitFor(
    "['ready', 'error'].includes(window.__visualInstinctLab?.segmentationCompositeReceipt?.()?.status)",
    { timeoutMs: input.composite_timeout_ms },
  );
  const neutral = await compositeReceipt(api);
  assert(neutral.status === "ready", `the segmentation composite failed: ${neutral.error}`);
  assert(neutral.modelId === input.expected.model_id, "the composite used the wrong model");
  assert(neutral.contractSha256 === input.expected.contract_sha256, "the contract digest is wrong");
  assert(neutral.recipeId === input.expected.recipe_id, "the part recipe is wrong");
  assert(neutral.sourceSha256 === input.expected.source_sha256, "the source image digest is wrong");
  assert(neutral.segmentation.backendRequested === "webgpu", "segmentation did not request WebGPU");
  assert(neutral.segmentation.alternateProviderConfigured === false,
    "segmentation configured an alternate provider");
  assert(neutral.segmentation.executionLocation === "browser-worker",
    "segmentation did not execute in the browser Worker");
  assert(neutral.segmentation.adapterBoundToRuntime === true,
    "the segmentation adapter was not bound to ONNX Runtime");
  assert(neutral.segmentation.deviceAcquired === true,
    "ONNX Runtime did not acquire a WebGPU device");
  assert(positiveTimes(neutral.segmentation.encodeMs), "the image encode was not measured");
  assert(neutral.segmentation.postprocessPositiveControl.graphParityIoU >= input.minimum_graph_parity_iou,
    "Worker postprocessing did not match the graph positive control");
  assert(neutral.parts.length === 3, "the composite did not resolve body plus two wings");
  assert(neutral.parts.every((part) => positiveTimes(part.decodeMs)),
    "one or more part resolves were not measured");
  assert(neutral.parts.every((part) => part.decodeMs.every(
    (duration) => duration <= input.maximum_part_resolve_ms,
  )), "one or more part resolves exceeded the interaction latency budget");
  assert(neutral.parts.every((part) => part.maskSummary.areaPixels > 0),
    "one or more part masks were empty");
  assert(neutral.parts.every((part) => part.workerPostprocess === input.expected.worker_postprocess),
    "one or more masks bypassed the declared Worker postprocess");
  assert(neutral.compositor.executionProvider === "webgpu", "the compositor did not use WebGPU");
  assert(neutral.compositor.alternateProviderConfigured === false,
    "the compositor configured a fallback renderer");
  assert(neutral.compositor.deviceAcquired === true, "the compositor did not acquire a WebGPU device");
  assert(neutral.compositor.shaderSha256 === input.expected.shader_sha256,
    "the compositor shader digest is wrong");
  assert(neutral.compositor.drawCalls === 3, "the compositor did not draw exactly three scene objects");
  assert(neutral.compositor.sourcePositiveControlVisiblePixels >= input.minimum_changed_pixels,
    "the WebGPU source-texture positive control did not render visible pixels");
  assert(neutral.control.id === input.expected.control_id, "the motion control id is wrong");
  assert(JSON.stringify(neutral.control.unitsPerValue) === JSON.stringify(input.expected.motion_units),
    "the motion units drifted from the generated recipe");
  assert(neutral.frame.sha256 === neutral.neutralSha256, "the initial frame is not neutral");
  assert(neutral.frame.renderMs <= input.maximum_composite_frame_ms,
    "the neutral composite frame exceeded the interaction latency budget");
  await api.screenshot("02-neutral-composite.png");

  const maximumInput = await api.trustedRange("#segmentation-flap-control", 1, 10);
  assert(maximumInput.events.some((event) => event.isTrusted === true),
    "the maximum control did not receive trusted input");
  const maximum = await waitForControl(api, 1, neutral.frame.serial);
  await api.screenshot("03-maximum-flap.png");
  assert(maximum.frame.sha256 !== neutral.frame.sha256, "maximum motion did not change rendered pixels");
  assert(maximum.frame.changedPixelsFromNeutral >= input.minimum_changed_pixels,
    "maximum motion changed too few pixels");
  assert(maximum.frame.renderMs <= input.maximum_composite_frame_ms,
    "the maximum composite frame exceeded the interaction latency budget");
  assert(maximum.bodyInvariantSha256 === neutral.bodyInvariantSha256,
    "the static body layer changed during wing motion");

  const minimumInput = await api.trustedRange("#segmentation-flap-control", 0, 10);
  assert(minimumInput.events.some((event) => event.isTrusted === true),
    "the minimum control did not receive trusted input");
  const minimum = await waitForControl(api, -1, maximum.frame.serial);
  await api.screenshot("04-minimum-flap.png");
  assert(minimum.frame.sha256 !== neutral.frame.sha256, "minimum motion did not change rendered pixels");
  assert(minimum.frame.sha256 !== maximum.frame.sha256,
    "opposite control extremes rendered the same pixels");
  assert(minimum.frame.changedPixelsFromNeutral >= input.minimum_changed_pixels,
    "minimum motion changed too few pixels");
  assert(minimum.frame.renderMs <= input.maximum_composite_frame_ms,
    "the minimum composite frame exceeded the interaction latency budget");
  assert(minimum.bodyInvariantSha256 === neutral.bodyInvariantSha256,
    "the static body layer changed at minimum motion");

  const returnInput = await api.trustedRange("#segmentation-flap-control", 0.5, 10);
  assert(returnInput.events.some((event) => event.isTrusted === true),
    "the neutral return did not receive trusted input");
  const returned = await waitForControl(api, 0, minimum.frame.serial);
  assert(returned.frame.sha256 === neutral.frame.sha256,
    "returning to neutral did not reproduce the original GPU frame");
  assert(returned.frame.renderMs <= input.maximum_composite_frame_ms,
    "the returned neutral frame exceeded the interaction latency budget");
  assert(returned.bodyInvariantSha256 === neutral.bodyInvariantSha256,
    "the body layer was not invariant across the control cycle");
  await api.screenshot("05-neutral-return.png");

  return {
    status: "real-mobilesam-webgpu-segmentation-composite",
    neutral,
    maximum: maximum.frame,
    minimum: minimum.frame,
    returned: returned.frame,
    trusted_inputs: {
      maximum: maximumInput,
      minimum: minimumInput,
      returned: returnInput,
    },
  };
}
