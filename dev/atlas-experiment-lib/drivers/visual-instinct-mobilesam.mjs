function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function positiveTimes(values) {
  return Array.isArray(values)
    && values.length === 3
    && values.every((value) => Number.isFinite(value) && value > 0);
}

function canonicalValue(value) {
  if (Array.isArray(value)) return value.map(canonicalValue);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonicalValue(value[key])]));
}

function sameJsonValue(left, right) {
  return JSON.stringify(canonicalValue(left)) === JSON.stringify(canonicalValue(right));
}

export async function run(api, input) {
  const expected = input.expected;
  await api.screenshot("00-loaded.png");
  const contractReceipt = await api.evaluate(`(async () => {
    const host = await fetch("/segmentation-recipes.json", { cache: "no-store" }).then((response) => {
      if (!response.ok) throw new Error("segmentation recipe fetch failed with " + response.status);
      return response.json();
    });
    const generated = await import("/generated/segmentation-contracts.js");
    const browser = {
      schemaVersion: 1,
      contractSha256: generated.SEGMENTATION_CONTRACT_SHA256,
      recipes: Object.values(generated.SEGMENTATION_PART_RECIPES),
    };
    const canonical = (value) => {
      if (Array.isArray(value)) return value.map(canonical);
      if (!value || typeof value !== "object") return value;
      return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])]));
    };
    if (JSON.stringify(canonical(host)) !== JSON.stringify(canonical(browser))) {
      throw new Error("Rust and browser segmentation recipes are not equal.");
    }
    const owl = generated.segmentationPartRecipe("mechanical-owl-wing-flap-v1");
    return {
      contract_sha256: generated.SEGMENTATION_CONTRACT_SHA256,
      host_browser_equal: true,
      owl_recipe: {
        id: owl.id,
        body_box: owl.body.prompt.coordinates,
        part_roles: owl.parts.map((part) => part.role),
        part_boxes: owl.parts.map((part) => part.prompt.coordinates),
        control_id: owl.control?.id ?? null,
        motion_units: owl.parts.map((part) => part.motion?.unitsPerValue ?? null),
      },
    };
  })()`);
  assert(contractReceipt.contract_sha256 === expected.contract_sha256, "browser contract digest is wrong");
  assert(contractReceipt.host_browser_equal === true, "Rust and browser part recipes differ");
  assert(sameJsonValue(contractReceipt.owl_recipe, expected.owl_recipe),
    "mechanical owl part recipe is wrong");
  await api.waitFor(`(async () => {
    const lab = window.__visualInstinctLab;
    if (!lab) return false;
    try {
      await lab.selectModel(${JSON.stringify(input.model_id)});
    } catch {
      return false;
    }
    const receipt = lab?.modelReceipt();
    if (receipt?.selectedId === ${JSON.stringify(input.model_id)} && receipt.status === "ready") return true;
    if (receipt?.selectedId === ${JSON.stringify(input.model_id)}
      && (receipt.status === "assets-missing" || receipt.status === "error")) {
      throw new Error(receipt.error ?? receipt.status);
    }
    return false;
  })()`, { timeoutMs: input.manifest_timeout_ms });

  const receipt = await api.evaluate(
    `window.__visualInstinctLab.runSegmentationProof(${JSON.stringify(input.model_id)})`,
  );
  assert(receipt.segmenter === `real-${input.model_id}`, "real segmenter receipt is missing");
  assert(receipt.backendRequested === "webgpu", "WebGPU was not the requested backend");
  assert(receipt.alternateProviderConfigured === false, "an alternate execution provider was configured");
  assert(receipt.adapterBoundToRuntime === true, "the measured adapter was not bound to the runtime");
  assert(receipt.deviceAcquired === true, "the WebGPU execution provider did not acquire a device");
  assert(receipt.executionLocation === "browser-worker", "segmentation did not run in the browser worker");
  assert(receipt.contractSha256 === expected.contract_sha256, "generated segmentation contract digest is wrong");
  assert(receipt.encoderSha256 === expected.encoder_sha256, "encoder digest receipt is wrong");
  assert(receipt.decoderSha256 === expected.decoder_sha256, "decoder digest receipt is wrong");
  assert(receipt.artifactBytes === expected.artifact_bytes, "segmentation artifact size is wrong");
  assert(receipt.fixtureSha256 === expected.fixture_sha256, "frame 1 fixture digest is wrong");
  assert(receipt.occlusionFixtureSha256 === expected.occlusion_fixture_sha256, "occlusion fixture digest is wrong");
  assert(JSON.stringify(receipt.encoderInputShape) === JSON.stringify(expected.encoder_input_shape), "encoder input shape is wrong");
  assert(JSON.stringify(receipt.encoderOutputShape) === JSON.stringify(expected.encoder_output_shape), "encoder output shape is wrong");
  assert(positiveTimes(receipt.frame1EncodeMs), "three real frame 1 encodes are required");
  assert(positiveTimes(receipt.frame2EncodeMs), "three real frame 2 encodes are required");
  assert(positiveTimes(receipt.pointPrompt?.decodeMs), "three point-prompt decodes are required");
  assert(positiveTimes(receipt.boxPrompt?.decodeMs), "three box-prompt decodes are required");
  assert(receipt.pointPrompt.maskSummary.areaPixels > 0, "point prompt returned an empty mask");
  assert(receipt.boxPrompt.maskSummary.areaPixels > 0, "box prompt returned an empty mask");
  assert(receipt.maskPostprocessing === expected.worker_postprocess,
    "the segmentation receipt used the wrong Worker postprocess");
  assert(receipt.pointPrompt.workerPostprocess === expected.worker_postprocess,
    "the point prompt bypassed the declared Worker postprocess");
  assert(receipt.boxPrompt.workerPostprocess === expected.worker_postprocess,
    "the box prompt bypassed the declared Worker postprocess");
  assert(receipt.pointPrompt.graphParityIoU >= input.minimum_graph_parity_iou,
    "point-prompt Worker postprocessing did not match the graph output");
  assert(receipt.boxPrompt.graphParityIoU >= input.minimum_graph_parity_iou,
    "box-prompt Worker postprocessing did not match the graph output");
  assert(receipt.promptTypesSupported?.point === true, "point prompt support was not proven");
  assert(receipt.promptTypesSupported?.box === true, "box prompt support was not proven");
  assert(receipt.identityTest?.withoutStructuredHint?.modelIdentityOutputAvailable === false,
    "pixel-only arm incorrectly reported an identity output");
  assert(receipt.identityTest?.withoutStructuredHint?.sameObjectIdProven === false,
    "pixel-only arm incorrectly proved stable object identity");
  assert(receipt.identityTest?.withStructuredHint?.structuredStateHintProvided === true,
    "structured identity hint was not provided");
  assert(receipt.identityTest?.withStructuredHint?.identityPreserved === true,
    "structured state did not preserve the object id");
  assert(receipt.memoryAfterLoad?.available === true, "browser memory measurement is missing");
  assert(receipt.memoryAfterLoad?.crossOriginIsolated === true, "browser memory proof was not cross-origin isolated");
  assert(receipt.memoryAfterLoad?.includesPageAndDedicatedWorkerJavaScriptAndWebAssembly === true,
    "browser memory proof omitted the dedicated Worker");
  await api.screenshot("01-proof-complete.png");

  return {
    status: "real-mobilesam-webgpu-measured",
    generated_contract: contractReceipt,
    model_id: receipt.modelId,
    backend_requested: receipt.backendRequested,
    alternate_provider_configured: receipt.alternateProviderConfigured,
    execution_location: receipt.executionLocation,
    contract_sha256: receipt.contractSha256,
    mask_source_output: receipt.maskSourceOutput,
    mask_postprocessing: receipt.maskPostprocessing,
    encoder_sha256: receipt.encoderSha256,
    decoder_sha256: receipt.decoderSha256,
    artifact_bytes: receipt.artifactBytes,
    frame_1_encode_ms: receipt.frame1EncodeMs,
    frame_2_encode_ms: receipt.frame2EncodeMs,
    point_decode_ms: receipt.pointPrompt.decodeMs,
    box_decode_ms: receipt.boxPrompt.decodeMs,
    identity: receipt.identityTest,
    memory_after_load: receipt.memoryAfterLoad,
  };
}
