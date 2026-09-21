import {
  segmentSource,
  segmentSourceByIdentity,
} from "./generated/segmentation-sources.js";

const MANIFEST = Object.freeze({
  modelId: "mobilesam-vit-t",
  model: "MobileSAM ViT-T",
  modelVersion: "source-f706ad9",
  sourceCommit: "f706ad9c4eb7f219c00d9050e46328518ffb65d2",
  sourceCheckpointSha256: "6dbb90523a35330fedd7f1d3dfc66f995213d81b29a5ca8108dbcdd4e37d6c2f",
  runtimeModuleUrl: "/ort/ort.webgpu.min.mjs",
  encoder: {
    url: "/model-assets/mobilesam-vit-t-encoder.onnx",
    sha256: "d80de6055095e7ba5551e6c15aaa8981b05dca77d6ae77ee82e51e2746a87e3d",
    bytes: 27996238,
    inputs: ["images"],
    outputs: ["image_embeddings"],
  },
  decoder: {
    url: "/model-assets/mobilesam-vit-t-decoder.onnx",
    sha256: "a21b65b6e1b75e2c6265b36835747a0ab9169ec1ed725139a78ce90297f95126",
    bytes: 16496934,
    inputs: [
      "image_embeddings",
      "point_coords",
      "point_labels",
      "mask_input",
      "has_mask_input",
      "orig_im_size",
    ],
    outputs: ["masks", "iou_predictions", "low_res_masks"],
  },
});

let loaded = null;

self.addEventListener("message", ({ data }) => {
  if (data?.type === "segment") {
    materialize(data)
      .then((result) => self.postMessage({
        type: "segment-result",
        requestId: data.requestId,
        id: data.id,
        result,
      }))
      .catch((error) => postError("segment-error", data, error));
    return;
  }
  if (data?.type === "prepare-picker") {
    preparePicker(data)
      .catch((error) => postError("picker-error", data, error));
    return;
  }
  if (data?.type === "resolve-picker") {
    resolvePicker(data)
      .catch((error) => postError("picker-error", data, error));
  }
});

function postError(type, data, error) {
  self.postMessage({
    type,
    requestId: data.requestId,
    id: data.id,
    error: error instanceof Error ? error.message : String(error),
  });
}

async function loadModel() {
  if (loaded) return loaded;
  if (!self.navigator?.gpu) throw new Error("WebGPU is unavailable in the Atlas browser worker.");
  const adapter = await self.navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new Error("WebGPU did not return an adapter to the Atlas browser worker.");
  const adapterInfo = adapter.info ? serializableAdapterInfo(adapter.info) : { available: true };
  const [encoderAsset, decoderAsset] = await Promise.all([
    fetchChecked(MANIFEST.encoder, "MobileSAM encoder"),
    fetchChecked(MANIFEST.decoder, "MobileSAM decoder"),
  ]);
  const runtime = await import(MANIFEST.runtimeModuleUrl);
  runtime.env.wasm.wasmPaths = "/ort/";
  runtime.env.wasm.numThreads = 1;
  runtime.env.wasm.proxy = false;
  runtime.env.webgpu.adapter = adapter;
  runtime.env.webgpu.powerPreference = "high-performance";

  const encoderStartedAt = performance.now();
  const encoderSession = await runtime.InferenceSession.create(encoderAsset.buffer, {
    executionProviders: ["webgpu"],
    graphOptimizationLevel: "all",
  });
  const encoderSessionCreateMs = performance.now() - encoderStartedAt;
  assertNames(encoderSession, MANIFEST.encoder, "encoder");

  const decoderStartedAt = performance.now();
  const decoderSession = await runtime.InferenceSession.create(decoderAsset.buffer, {
    executionProviders: ["webgpu"],
    graphOptimizationLevel: "all",
  });
  const decoderSessionCreateMs = performance.now() - decoderStartedAt;
  assertNames(decoderSession, MANIFEST.decoder, "decoder");

  loaded = {
    runtime,
    encoderSession,
    decoderSession,
    pickers: new Map(),
    loadReceipt: {
      modelId: MANIFEST.modelId,
      model: MANIFEST.model,
      modelVersion: MANIFEST.modelVersion,
      sourceCommit: MANIFEST.sourceCommit,
      sourceCheckpointSha256: MANIFEST.sourceCheckpointSha256,
      encoderSha256: encoderAsset.sha256,
      encoderBytes: encoderAsset.buffer.byteLength,
      decoderSha256: decoderAsset.sha256,
      decoderBytes: decoderAsset.buffer.byteLength,
      backendRequested: "webgpu",
      alternateProviderConfigured: false,
      adapterBoundToRuntime: runtime.env.webgpu.adapter === adapter,
      deviceAcquired: Boolean(runtime.env.webgpu.device),
      adapterInfo,
      encoderInputNames: [...encoderSession.inputNames],
      encoderOutputNames: [...encoderSession.outputNames],
      decoderInputNames: [...decoderSession.inputNames],
      decoderOutputNames: [...decoderSession.outputNames],
      encoderFetchMs: encoderAsset.fetchMs,
      decoderFetchMs: decoderAsset.fetchMs,
      encoderSessionCreateMs,
      decoderSessionCreateMs,
    },
  };
  return loaded;
}

async function materialize(data) {
  const startedAt = performance.now();
  const segment = data.segment;
  if (!segment || segment.segment_status !== "proposed") {
    throw new Error("The worker requires one proposed Atlas segment.");
  }
  if (!Number.isInteger(segment.segment_generation) || segment.segment_generation < 1) {
    throw new Error("The Atlas segment has no valid generation.");
  }
  const source = segmentSourceByIdentity(
    segment.segment_source_sha256,
    [segment.segment_source_width, segment.segment_source_height],
  );
  validateBox(segment.segment_prompt_box, "prompt box", source.size);
  const occlusion = Array.isArray(segment.segment_occlusion_box)
    && segment.segment_occlusion_box.length === 4
    ? segment.segment_occlusion_box
    : null;
  if (occlusion) validateBox(occlusion, "occlusion box", source.size);
  const wingPrompts = segment.segment_wing_prompts;
  const hasWingPrompts = wingPrompts
    && typeof wingPrompts === "object"
    && !Array.isArray(wingPrompts)
    && Object.keys(wingPrompts).length > 0;
  const wingEntries = hasWingPrompts
    ? [
      ["left-wing", wingPrompts["left-wing"]],
      ["right-wing", wingPrompts["right-wing"]],
    ]
    : [];
  for (const [role, box] of wingEntries) validateBox(box, `${role} prompt box`, source.size);

  const model = await loadModel();
  const sourceStartedAt = performance.now();
  const sourceResponse = await fetch(source.url, { cache: "no-store" });
  if (!sourceResponse.ok) throw new Error(`Infographic fetch failed with status ${sourceResponse.status}.`);
  const sourceBuffer = await sourceResponse.arrayBuffer();
  const sourceSha256 = await sha256(sourceBuffer);
  if (sourceSha256 !== source.sha256) {
    throw new Error(`Infographic digest mismatch. Expected ${source.sha256}, received ${sourceSha256}.`);
  }
  const bitmap = await createImageBitmap(new Blob([sourceBuffer], { type: "image/png" }));
  if (bitmap.width !== source.size[0] || bitmap.height !== source.size[1]) {
    bitmap.close();
    throw new Error(`Infographic dimensions are ${bitmap.width}x${bitmap.height}, expected ${source.size.join("x")}.`);
  }
  const sourceFetchMs = performance.now() - sourceStartedAt;
  const prepared = preprocess(bitmap, occlusion);
  bitmap.close();

  const encoderInput = new model.runtime.Tensor("float32", prepared.tensor, [1, 3, 1024, 1024]);
  const encodeStartedAt = performance.now();
  const encoderOutputs = await model.encoderSession.run({ images: encoderInput });
  const embeddingTensor = encoderOutputs.image_embeddings;
  const embeddingValues = Float32Array.from(await tensorValues(embeddingTensor));
  const encodeMs = performance.now() - encodeStartedAt;
  const encoderOutputShape = [...embeddingTensor.dims];
  encoderInput.dispose?.();
  for (const tensor of Object.values(encoderOutputs)) tensor.dispose?.();
  if (!arrayEqual(encoderOutputShape, [1, 256, 64, 64])) {
    throw new Error(`Unexpected MobileSAM encoder output shape: ${JSON.stringify(encoderOutputShape)}.`);
  }

  const body = await decodePrompt(model, embeddingValues, prepared, segment.segment_prompt_box);
  const parts = [];
  for (const [role, promptBox] of wingEntries) {
    const decoded = await decodePrompt(model, embeddingValues, prepared, promptBox);
    parts.push({
      role,
      prompt_box: [...promptBox],
      predicted_iou: decoded.selected.predictedIoU,
      mask_encoding: "mask-alpha-rle-u32le-base64-v1",
      mask_width: decoded.compact.width,
      mask_height: decoded.compact.height,
      mask_runs: decoded.maskRuns,
      mask_source_box: decoded.selected.sourceBox,
      receipt: {
        ...model.loadReceipt,
        role,
        executionLocation: "browser-worker",
        segmenter: "real-mobilesam-vit-t",
        sourceUrl: source.url,
        sourceSha256,
        generation: segment.segment_generation,
        stableObjectId: data.id,
        promptBox: [...promptBox],
        decoderContract: "mobile-sam-multimask-highest-predicted-iou",
        selectedMaskIndex: decoded.selected.index,
        predictedIoU: decoded.selected.predictedIoU,
        predictedIoUContract: "raw-regression-output-not-probability",
        predictedIoURawInUnitInterval:
          decoded.selected.predictedIoU >= 0 && decoded.selected.predictedIoU <= 1,
        sourceMaskAreaPixels: decoded.selected.area,
        maskSourceBox: decoded.selected.sourceBox,
        materializedMaskSize: [decoded.compact.width, decoded.compact.height],
        decodeMs: decoded.decodeMs,
        fallbackUsed: false,
        mockUsed: false,
      },
    });
  }
  const receipt = {
    ...model.loadReceipt,
    executionLocation: "browser-worker",
    segmenter: "real-mobilesam-vit-t",
    sourceUrl: source.url,
    sourceSha256,
    sourceSize: [...source.size],
    sourceFetchMs,
    generation: segment.segment_generation,
    stableObjectId: data.id,
    promptBox: [...segment.segment_prompt_box],
    occlusionBox: occlusion ? [...occlusion] : null,
    occlusionApplied: Boolean(occlusion),
    preprocessing: "sam-resize-longest-1024-rgb-imagenet-pad",
    decoderContract: "mobile-sam-multimask-highest-predicted-iou",
    maskThreshold: 0,
    encoderInputShape: [1, 3, 1024, 1024],
    encoderOutputShape,
    decoderOutputShapes: body.decoderOutputShapes,
    selectedMaskIndex: body.selected.index,
    predictedIoU: body.selected.predictedIoU,
    predictedIoUContract: "raw-regression-output-not-probability",
    predictedIoURawInUnitInterval: body.selected.predictedIoU >= 0 && body.selected.predictedIoU <= 1,
    sourceMaskAreaPixels: body.selected.area,
    maskSourceBox: body.selected.sourceBox,
    materializedMaskSize: [body.compact.width, body.compact.height],
    partCount: parts.length,
    partRoles: parts.map((part) => part.role),
    partPromptBoxes: parts.map((part) => part.prompt_box),
    partDecodeMs: parts.map((part) => part.receipt.decodeMs),
    preprocessMs: prepared.preprocessMs,
    encodeMs,
    decodeMs: body.decodeMs,
    totalMs: performance.now() - startedAt,
    fallbackUsed: false,
    mockUsed: false,
  };
  return {
    generation: segment.segment_generation,
    model_id: MANIFEST.modelId,
    encoder_sha256: MANIFEST.encoder.sha256,
    decoder_sha256: MANIFEST.decoder.sha256,
    predicted_iou: body.selected.predictedIoU,
    mask_encoding: "mask-alpha-rle-u32le-base64-v1",
    mask_width: body.compact.width,
    mask_height: body.compact.height,
    mask_runs: body.maskRuns,
    mask_source_box: body.selected.sourceBox,
    parts,
    receipt,
  };
}

async function preparePicker(data) {
  const startedAt = performance.now();
  const model = await loadModel();
  const source = segmentSource(data.sourceId);
  const existing = model.pickers.get(source.id);
  if (existing) {
    self.postMessage({
      type: "picker-ready",
      requestId: data.requestId,
      sourceId: source.id,
      receipt: { ...existing.receipt, embeddingReused: true },
    });
    return;
  }

  const sourceStartedAt = performance.now();
  const sourceResponse = await fetch(source.url, { cache: "no-store" });
  if (!sourceResponse.ok) throw new Error(`Image fetch failed with status ${sourceResponse.status}.`);
  const sourceBuffer = await sourceResponse.arrayBuffer();
  const sourceSha256 = await sha256(sourceBuffer);
  if (sourceSha256 !== source.sha256) {
    throw new Error(`Image digest mismatch. Expected ${source.sha256}, received ${sourceSha256}.`);
  }
  const bitmap = await createImageBitmap(new Blob([sourceBuffer], { type: "image/png" }));
  if (bitmap.width !== source.size[0] || bitmap.height !== source.size[1]) {
    const actual = [bitmap.width, bitmap.height];
    bitmap.close();
    throw new Error(`Image dimensions are ${actual.join("x")}, expected ${source.size.join("x")}.`);
  }
  const sourceFetchMs = performance.now() - sourceStartedAt;
  const prepared = preprocess(bitmap, null);
  bitmap.close();

  const encoderInput = new model.runtime.Tensor("float32", prepared.tensor, [1, 3, 1024, 1024]);
  const encodeStartedAt = performance.now();
  const encoderOutputs = await model.encoderSession.run({ images: encoderInput });
  const embeddingTensor = encoderOutputs.image_embeddings;
  const embedding = Float32Array.from(await tensorValues(embeddingTensor));
  const encodeMs = performance.now() - encodeStartedAt;
  const encoderOutputShape = [...embeddingTensor.dims];
  encoderInput.dispose?.();
  for (const tensor of Object.values(encoderOutputs)) tensor.dispose?.();
  if (!arrayEqual(encoderOutputShape, [1, 256, 64, 64])) {
    throw new Error(`Unexpected MobileSAM encoder output shape: ${JSON.stringify(encoderOutputShape)}.`);
  }

  const receipt = {
    ...model.loadReceipt,
    executionLocation: "browser-worker",
    sourceUrl: source.url,
    sourceSha256,
    sourceSize: [...source.size],
    sourceFetchMs,
    preprocessMs: prepared.preprocessMs,
    encodeMs,
    encoderOutputShape,
    embeddingReused: false,
    totalMs: performance.now() - startedAt,
    fallbackUsed: false,
    mockUsed: false,
  };
  model.pickers.set(source.id, { embedding, prepared, receipt });
  self.postMessage({ type: "picker-ready", requestId: data.requestId, sourceId: source.id, receipt });
}

async function resolvePicker(data) {
  const model = await loadModel();
  const source = segmentSource(data.sourceId);
  const picker = model.pickers.get(source.id);
  if (!picker) throw new Error("Object selection is not prepared yet.");
  if (!Array.isArray(data.points) || data.points.length === 0 || data.points.length > 16) {
    throw new Error("Object selection needs 1 to 16 prompt points.");
  }
  const points = data.points.map((point) => {
    if (!Array.isArray(point?.coordinates)
        || point.coordinates.length !== 2
        || !point.coordinates.every(Number.isInteger)
        || point.coordinates[0] < 0
        || point.coordinates[0] >= source.size[0]
        || point.coordinates[1] < 0
        || point.coordinates[1] >= source.size[1]) {
      throw new Error("Every prompt point must be an integer inside the image.");
    }
    if (point.label !== "positive" && point.label !== "negative") {
      throw new Error("Every prompt point must add to or remove from the mask.");
    }
    return { coordinates: [...point.coordinates], label: point.label };
  });
  if (!points.some((point) => point.label === "positive")) {
    throw new Error("Object selection needs one point inside the object.");
  }

  const decoded = await decodePointPrompt(model, picker.embedding, picker.prepared, points);
  const receipt = {
    ...picker.receipt,
    executionLocation: "browser-worker",
    segmenter: "real-mobilesam-vit-t",
    sourceUrl: source.url,
    sourceSha256: source.sha256,
    sourceSize: [...source.size],
    sourcePoints: points.map((point) => point.coordinates),
    pointLabels: points.map((point) => point.label),
    preprocessing: "sam-resize-longest-1024-rgb-imagenet-pad",
    decoderContract: "mobile-sam-multimask-highest-predicted-iou",
    selectedMaskIndex: decoded.selected.index,
    predictedIoU: decoded.selected.predictedIoU,
    predictedIoUContract: "raw-regression-output-not-probability",
    predictedIoURawInUnitInterval:
      decoded.selected.predictedIoU >= 0 && decoded.selected.predictedIoU <= 1,
    sourceMaskAreaPixels: decoded.selected.area,
    maskSourceBox: decoded.selected.sourceBox,
    materializedMaskSize: [decoded.compact.width, decoded.compact.height],
    decodeMs: decoded.decodeMs,
    fallbackUsed: false,
    mockUsed: false,
  };
  const result = {
    generation: 1,
    model_id: MANIFEST.modelId,
    encoder_sha256: MANIFEST.encoder.sha256,
    decoder_sha256: MANIFEST.decoder.sha256,
    predicted_iou: decoded.selected.predictedIoU,
    mask_encoding: "mask-alpha-rle-u32le-base64-v1",
    mask_width: decoded.compact.width,
    mask_height: decoded.compact.height,
    mask_runs: decoded.maskRuns,
    mask_source_box: decoded.selected.sourceBox,
    parts: [],
    receipt,
  };
  self.postMessage({
    type: "picker-result",
    requestId: data.requestId,
    sourceId: source.id,
    result,
    previewMask: decoded.selected.mask.buffer,
  }, [decoded.selected.mask.buffer]);
}

async function decodePrompt(model, embeddingValues, prepared, promptBox) {
  const [left, top, right, bottom] = promptBox;
  const tensors = {
    image_embeddings: new model.runtime.Tensor("float32", embeddingValues, [1, 256, 64, 64]),
    point_coords: new model.runtime.Tensor("float32", new Float32Array([
      left * prepared.promptScaleX,
      top * prepared.promptScaleY,
      right * prepared.promptScaleX,
      bottom * prepared.promptScaleY,
    ]), [1, 2, 2]),
    point_labels: new model.runtime.Tensor("float32", new Float32Array([2, 3]), [1, 2]),
    mask_input: new model.runtime.Tensor("float32", new Float32Array(256 * 256), [1, 1, 256, 256]),
    has_mask_input: new model.runtime.Tensor("float32", new Float32Array([0]), [1]),
    orig_im_size: new model.runtime.Tensor(
      "float32",
      new Float32Array([prepared.sourceHeight, prepared.sourceWidth]),
      [2],
    ),
  };
  const decodeStartedAt = performance.now();
  const outputs = await model.decoderSession.run(tensors);
  const maskValues = Float32Array.from(await tensorValues(outputs.masks));
  const scoreValues = Float32Array.from(await tensorValues(outputs.iou_predictions));
  const maskShape = [...outputs.masks.dims];
  const decoderOutputShapes = model.decoderSession.outputNames.map((name) => [...outputs[name].dims]);
  const decodeMs = performance.now() - decodeStartedAt;
  for (const tensor of Object.values(tensors)) tensor.dispose?.();
  for (const tensor of Object.values(outputs)) tensor.dispose?.();
  const selected = selectMask(maskValues, scoreValues, maskShape);
  const compact = compactMask(selected.mask, selected.width, selected.height, selected.sourceBox);
  return {
    selected,
    compact,
    maskRuns: encodeRuns(compact.mask),
    decoderOutputShapes,
    decodeMs,
  };
}

async function decodePointPrompt(model, embeddingValues, prepared, points) {
  const coordinates = points.flatMap((point) => [
    point.coordinates[0] * prepared.promptScaleX,
    point.coordinates[1] * prepared.promptScaleY,
  ]);
  const labels = points.map((point) => point.label === "positive" ? 1 : 0);
  const tensors = {
    image_embeddings: new model.runtime.Tensor("float32", embeddingValues, [1, 256, 64, 64]),
    point_coords: new model.runtime.Tensor(
      "float32",
      new Float32Array(coordinates),
      [1, points.length, 2],
    ),
    point_labels: new model.runtime.Tensor(
      "float32",
      new Float32Array(labels),
      [1, labels.length],
    ),
    mask_input: new model.runtime.Tensor("float32", new Float32Array(256 * 256), [1, 1, 256, 256]),
    has_mask_input: new model.runtime.Tensor("float32", new Float32Array([0]), [1]),
    orig_im_size: new model.runtime.Tensor(
      "float32",
      new Float32Array([prepared.sourceHeight, prepared.sourceWidth]),
      [2],
    ),
  };
  const decodeStartedAt = performance.now();
  const outputs = await model.decoderSession.run(tensors);
  const maskValues = Float32Array.from(await tensorValues(outputs.masks));
  const scoreValues = Float32Array.from(await tensorValues(outputs.iou_predictions));
  const maskShape = [...outputs.masks.dims];
  const decoderOutputShapes = model.decoderSession.outputNames.map((name) => [...outputs[name].dims]);
  const decodeMs = performance.now() - decodeStartedAt;
  for (const tensor of Object.values(tensors)) tensor.dispose?.();
  for (const tensor of Object.values(outputs)) tensor.dispose?.();
  const selected = selectMask(maskValues, scoreValues, maskShape);
  const compact = compactMask(selected.mask, selected.width, selected.height, selected.sourceBox);
  return {
    selected,
    compact,
    maskRuns: encodeRuns(compact.mask),
    decoderOutputShapes,
    decodeMs,
  };
}

function preprocess(bitmap, occlusion) {
  const startedAt = performance.now();
  const source = new OffscreenCanvas(bitmap.width, bitmap.height);
  const sourceContext = source.getContext("2d", { willReadFrequently: true });
  if (!sourceContext) throw new Error("OffscreenCanvas source context is unavailable.");
  sourceContext.drawImage(bitmap, 0, 0);
  if (occlusion) {
    sourceContext.fillStyle = "rgb(22, 31, 50)";
    sourceContext.fillRect(
      occlusion[0],
      occlusion[1],
      occlusion[2] - occlusion[0],
      occlusion[3] - occlusion[1],
    );
  }

  const size = 1024;
  const canvas = new OffscreenCanvas(size, size);
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) throw new Error("OffscreenCanvas preprocessing context is unavailable.");
  context.imageSmoothingEnabled = true;
  context.imageSmoothingQuality = "high";
  const scale = size / Math.max(bitmap.width, bitmap.height);
  const drawWidth = Math.round(bitmap.width * scale);
  const drawHeight = Math.round(bitmap.height * scale);
  context.drawImage(source, 0, 0, drawWidth, drawHeight);
  const pixels = context.getImageData(0, 0, drawWidth, drawHeight).data;
  const plane = size * size;
  const tensor = new Float32Array(plane * 3);
  const means = [123.675, 116.28, 103.53];
  const deviations = [58.395, 57.12, 57.375];
  for (let y = 0; y < drawHeight; y += 1) {
    for (let x = 0; x < drawWidth; x += 1) {
      const sourceIndex = ((y * drawWidth) + x) * 4;
      const destination = (y * size) + x;
      tensor[destination] = (pixels[sourceIndex] - means[0]) / deviations[0];
      tensor[plane + destination] = (pixels[sourceIndex + 1] - means[1]) / deviations[1];
      tensor[(plane * 2) + destination] = (pixels[sourceIndex + 2] - means[2]) / deviations[2];
    }
  }
  return {
    tensor,
    sourceWidth: bitmap.width,
    sourceHeight: bitmap.height,
    promptScaleX: drawWidth / bitmap.width,
    promptScaleY: drawHeight / bitmap.height,
    preprocessMs: performance.now() - startedAt,
  };
}

function selectMask(values, scores, shape) {
  if (shape.length !== 4 || shape[0] !== 1) {
    throw new Error(`Unexpected MobileSAM mask shape: ${JSON.stringify(shape)}.`);
  }
  let index = 0;
  for (let candidate = 1; candidate < scores.length; candidate += 1) {
    if (scores[candidate] > scores[index]) index = candidate;
  }
  const height = shape[2];
  const width = shape[3];
  const plane = width * height;
  const offset = index * plane;
  const mask = new Uint8Array(plane);
  let area = 0;
  let minX = width;
  let minY = height;
  let maxX = -1;
  let maxY = -1;
  for (let pixel = 0; pixel < plane; pixel += 1) {
    if (values[offset + pixel] <= 0) continue;
    mask[pixel] = 1;
    area += 1;
    const x = pixel % width;
    const y = Math.floor(pixel / width);
    minX = Math.min(minX, x);
    minY = Math.min(minY, y);
    maxX = Math.max(maxX, x);
    maxY = Math.max(maxY, y);
  }
  if (area === 0) throw new Error("MobileSAM returned an empty mask for the oracle box.");
  const predictedIoU = Number(scores[index]);
  if (!Number.isFinite(predictedIoU)) {
    throw new Error(`MobileSAM returned non-finite predicted IoU ${predictedIoU}.`);
  }
  return {
    index,
    predictedIoU,
    mask,
    width,
    height,
    area,
    sourceBox: [minX, minY, maxX + 1, maxY + 1],
  };
}

function compactMask(mask, sourceWidth, sourceHeight, box) {
  const [left, top, right, bottom] = box;
  const cropWidth = right - left;
  const cropHeight = bottom - top;
  const scale = Math.min(1, 320 / Math.max(cropWidth, cropHeight));
  const width = Math.max(1, Math.round(cropWidth * scale));
  const height = Math.max(1, Math.round(cropHeight * scale));
  const compact = new Uint8Array(width * height);
  for (let y = 0; y < height; y += 1) {
    const sourceY = Math.min(bottom - 1, top + Math.floor(((y + 0.5) * cropHeight) / height));
    for (let x = 0; x < width; x += 1) {
      const sourceX = Math.min(right - 1, left + Math.floor(((x + 0.5) * cropWidth) / width));
      compact[(y * width) + x] = mask[(sourceY * sourceWidth) + sourceX];
    }
  }
  if (!compact.some(Boolean)) throw new Error("The compact MobileSAM mask is empty.");
  return { mask: compact, width, height };
}

function encodeRuns(mask) {
  const runs = [];
  let value = 0;
  let run = 0;
  for (const pixel of mask) {
    const binary = pixel ? 1 : 0;
    if (binary === value) {
      run += 1;
    } else {
      runs.push(run);
      run = 1;
      value = binary;
    }
  }
  runs.push(run);
  const bytes = new Uint8Array(runs.length * 4);
  const view = new DataView(bytes.buffer);
  runs.forEach((count, index) => view.setUint32(index * 4, count, true));
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}

async function fetchChecked(graph, label) {
  const startedAt = performance.now();
  const response = await fetch(graph.url, { cache: "no-store" });
  if (!response.ok) throw new Error(`${label} fetch failed with status ${response.status}.`);
  const buffer = await response.arrayBuffer();
  const digest = await sha256(buffer);
  if (digest !== graph.sha256) {
    throw new Error(`${label} digest mismatch. Expected ${graph.sha256}, received ${digest}.`);
  }
  if (buffer.byteLength !== graph.bytes) {
    throw new Error(`${label} byte length mismatch. Expected ${graph.bytes}, received ${buffer.byteLength}.`);
  }
  return { buffer, sha256: digest, fetchMs: performance.now() - startedAt };
}

function assertNames(session, graph, label) {
  if (!arrayEqual(session.inputNames, graph.inputs)) {
    throw new Error(`Unexpected ${label} inputs: ${JSON.stringify(session.inputNames)}.`);
  }
  if (!arrayEqual(session.outputNames, graph.outputs)) {
    throw new Error(`Unexpected ${label} outputs: ${JSON.stringify(session.outputNames)}.`);
  }
}

function validateBox(box, label, sourceSize) {
  if (!Array.isArray(box) || box.length !== 4 || !box.every(Number.isFinite)) {
    throw new Error(`The ${label} must contain four finite source-pixel numbers.`);
  }
  if (box[0] < 0 || box[1] < 0 || box[2] <= box[0] || box[3] <= box[1]
      || box[2] > sourceSize[0] || box[3] > sourceSize[1]) {
    throw new Error(`The ${label} must be [left, top, right, bottom] inside the infographic.`);
  }
}

async function tensorValues(tensor) {
  try {
    if (tensor.data) return tensor.data;
  } catch {
    // WebGPU-resident output values are downloaded through getData().
  }
  return tensor.getData();
}

async function sha256(buffer) {
  const digest = await crypto.subtle.digest("SHA-256", buffer);
  return Array.from(new Uint8Array(digest), (value) => value.toString(16).padStart(2, "0")).join("");
}

function serializableAdapterInfo(info) {
  const out = {};
  for (const key of ["vendor", "architecture", "device", "description"]) {
    if (info[key]) out[key] = String(info[key]);
  }
  return Object.keys(out).length ? out : { available: true };
}

function arrayEqual(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}
