import {
  DEFAULT_SEGMENT_SOURCE_ID,
  SEGMENT_SOURCES,
  segmentSource,
  segmentSourceByIdentity,
} from "./generated/segmentation-sources.js";

const PROMPT_HIT_RADIUS = 18;

export function nextNumberedPartLabel(label) {
  const clean = String(label || "").trim();
  const numbered = clean.match(/^(.*\S)\s+(\d+)$/);
  if (!numbered) return clean ? `${clean} 2` : "Part 1";
  return `${numbered[1]} ${Number(numbered[2]) + 1}`;
}

export function decodeMaskRuns(encoded, expected) {
  const binary = atob(encoded);
  if (binary.length % 4 !== 0) throw new Error("segment mask RLE is not u32 aligned");
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  const view = new DataView(bytes.buffer);
  const out = new Uint8Array(expected);
  let cursor = 0;
  let value = 0;
  for (let offset = 0; offset < bytes.length; offset += 4) {
    const run = view.getUint32(offset, true);
    if (value) out.fill(1, cursor, cursor + run);
    cursor += run;
    value = value ? 0 : 1;
  }
  if (cursor !== expected) throw new Error(`segment mask covers ${cursor} pixels, expected ${expected}`);
  return out;
}

export function togglePromptPoint(points, coordinates, label, radius = PROMPT_HIT_RADIUS) {
  const nearest = points
    .map((point, index) => ({
      index,
      distance: Math.hypot(
        point.coordinates[0] - coordinates[0],
        point.coordinates[1] - coordinates[1],
      ),
    }))
    .sort((left, right) => left.distance - right.distance)[0];
  if (nearest && nearest.distance <= radius) {
    return {
      points: points.filter((_, index) => index !== nearest.index),
      removed: true,
    };
  }
  return {
    points: [...points, { coordinates: [...coordinates], label, trusted: true }],
    removed: false,
  };
}

export function pointFromCanvas(event, canvas, sourceSize) {
  const bounds = canvas.getBoundingClientRect();
  if (!bounds.width || !bounds.height) throw new Error("The image is not visible yet.");
  const scale = Math.min(bounds.width / sourceSize[0], bounds.height / sourceSize[1]);
  const imageWidth = sourceSize[0] * scale;
  const imageHeight = sourceSize[1] * scale;
  const imageLeft = bounds.left + (bounds.width - imageWidth) / 2;
  const imageTop = bounds.top + (bounds.height - imageHeight) / 2;
  if (
    event.clientX < imageLeft
    || event.clientX > imageLeft + imageWidth
    || event.clientY < imageTop
    || event.clientY > imageTop + imageHeight
  ) {
    throw new Error("Click inside the source image.");
  }
  const x = Math.round((event.clientX - imageLeft) / scale);
  const y = Math.round((event.clientY - imageTop) / scale);
  return [
    Math.max(0, Math.min(sourceSize[0] - 1, x)),
    Math.max(0, Math.min(sourceSize[1] - 1, y)),
  ];
}

export function atlasProposal(label, result, placement, parentId = "") {
  const cleanLabel = String(label || "").trim();
  if (!cleanLabel) throw new Error("Name the object before creating it.");
  if (!Array.isArray(result?.mask_source_box) || result.mask_source_box.length !== 4) {
    throw new Error("The selected mask has no usable bounds.");
  }
  if (![placement?.x, placement?.y, placement?.w].every(Number.isFinite)) {
    throw new Error("The Atlas placement is incomplete.");
  }
  return {
    label: cleanLabel,
    tags: [],
    ocr: [],
    prompt_box: [...result.mask_source_box],
    x: placement.x,
    y: placement.y,
    w: placement.w,
    animation: "none",
    parent_id: String(parentId || "").trim(),
  };
}

export function atlasMaterialization(result) {
  const required = [
    "model_id",
    "encoder_sha256",
    "decoder_sha256",
    "predicted_iou",
    "mask_encoding",
    "mask_width",
    "mask_height",
    "mask_runs",
    "mask_source_box",
    "receipt",
  ];
  for (const field of required) {
    if (result?.[field] === undefined || result[field] === null) {
      throw new Error(`The selected mask is missing ${field}.`);
    }
  }
  return {
    generation: 1,
    model_id: result.model_id,
    encoder_sha256: result.encoder_sha256,
    decoder_sha256: result.decoder_sha256,
    predicted_iou: result.predicted_iou,
    mask_encoding: result.mask_encoding,
    mask_width: result.mask_width,
    mask_height: result.mask_height,
    mask_runs: result.mask_runs,
    mask_source_box: [...result.mask_source_box],
    parts: [],
    receipt: structuredClone(result.receipt),
  };
}

export function createSegmentPicker({
  view,
  toolbar,
  worker,
  placement,
  accept,
  finish,
  report,
  maxDepth = 3,
  initialSourceId = DEFAULT_SEGMENT_SOURCE_ID,
}) {
  let currentSource = segmentSource(initialSourceId);
  const launcher = document.createElement("button");
  launcher.type = "button";
  launcher.className = "tool segment-picker-launcher";
  launcher.textContent = "Add mask";
  launcher.title = "Select another object or part from the image";
  launcher.setAttribute("aria-label", "Add another object or part mask from the image");
  toolbar.append(launcher);

  const panel = document.createElement("section");
  panel.className = "segment-picker";
  panel.hidden = true;
  panel.setAttribute("role", "dialog");
  panel.setAttribute("aria-labelledby", "segment-picker-title");

  const header = document.createElement("header");
  const headingGroup = document.createElement("div");
  const eyebrow = document.createElement("span");
  eyebrow.className = "segment-picker-eyebrow";
  eyebrow.textContent = "IMAGE TO OBJECT";
  const heading = document.createElement("h2");
  heading.id = "segment-picker-title";
  heading.textContent = "Click the object you want";
  const explanation = document.createElement("p");
  explanation.textContent = "The mask appears after one click. Click a point again to undo it, then refine only if the edge needs help.";
  headingGroup.append(eyebrow, heading, explanation);
  const close = document.createElement("button");
  close.type = "button";
  close.className = "segment-picker-close";
  close.textContent = "×";
  close.setAttribute("aria-label", "Close image object picker");
  header.append(headingGroup, close);

  const canvasWrap = document.createElement("div");
  canvasWrap.className = "segment-picker-canvas-wrap";
  const canvas = document.createElement("canvas");
  canvas.className = "segment-picker-canvas";
  canvas.width = currentSource.size[0];
  canvas.height = currentSource.size[1];
  canvas.setAttribute("aria-label", "Source image. Click an object to create its mask.");
  const canvasStatus = document.createElement("div");
  canvasStatus.className = "segment-picker-canvas-status";
  canvasStatus.setAttribute("role", "status");
  canvasStatus.textContent = "Preparing object selection";
  canvasWrap.append(canvas, canvasStatus);

  const controls = document.createElement("div");
  controls.className = "segment-picker-controls";
  const sourceLabel = document.createElement("label");
  sourceLabel.className = "segment-picker-source";
  const sourceText = document.createElement("span");
  sourceText.textContent = "Infographic";
  const sourceSelect = document.createElement("select");
  sourceSelect.setAttribute("aria-label", "Infographic source");
  for (const source of Object.values(SEGMENT_SOURCES)) {
    const option = document.createElement("option");
    option.value = source.id;
    option.textContent = source.targetRef.split("-")
      .map((word, index) => index ? word : word[0].toUpperCase() + word.slice(1))
      .join(" ");
    sourceSelect.append(option);
  }
  sourceSelect.value = currentSource.id;
  sourceLabel.append(sourceText, sourceSelect);
  const pointModes = document.createElement("div");
  pointModes.className = "segment-picker-modes";
  pointModes.setAttribute("role", "group");
  pointModes.setAttribute("aria-label", "Mask refinement mode");
  const include = document.createElement("button");
  include.type = "button";
  include.className = "segment-picker-mode active";
  include.textContent = "Add to mask";
  include.setAttribute("aria-pressed", "true");
  const exclude = document.createElement("button");
  exclude.type = "button";
  exclude.className = "segment-picker-mode";
  exclude.textContent = "Remove from mask";
  exclude.setAttribute("aria-pressed", "false");
  pointModes.append(include, exclude);

  const reset = document.createElement("button");
  reset.type = "button";
  reset.className = "quiet-button segment-picker-reset";
  reset.textContent = "Start over";
  reset.disabled = true;
  const undo = document.createElement("button");
  undo.type = "button";
  undo.className = "quiet-button segment-picker-undo";
  undo.textContent = "Undo point";
  undo.disabled = true;
  const resetActions = document.createElement("div");
  resetActions.className = "segment-picker-reset-actions";
  resetActions.append(undo, reset);
  const seriesSummary = document.createElement("div");
  seriesSummary.className = "segment-picker-series-summary";
  seriesSummary.hidden = true;
  controls.append(sourceLabel, pointModes, seriesSummary, resetActions);

  const footer = document.createElement("footer");
  const label = document.createElement("label");
  label.className = "segment-picker-label";
  const labelText = document.createElement("span");
  labelText.textContent = "Object name";
  const labelInput = document.createElement("input");
  labelInput.type = "text";
  labelInput.maxLength = 160;
  labelInput.value = "Object";
  labelInput.autocomplete = "off";
  label.append(labelText, labelInput);
  const parentLabel = document.createElement("label");
  parentLabel.className = "segment-picker-label segment-picker-parent";
  const parentText = document.createElement("span");
  parentText.textContent = "Belongs to";
  const parentSelect = document.createElement("select");
  parentSelect.setAttribute("aria-label", "Parent object for this mask");
  parentLabel.append(parentText, parentSelect);
  const create = document.createElement("button");
  create.type = "button";
  create.className = "segment-picker-create";
  create.textContent = "Create object";
  create.disabled = true;
  const finishParts = document.createElement("button");
  finishParts.type = "button";
  finishParts.className = "quiet-button segment-picker-finish";
  finishParts.textContent = "Finish parts";
  finishParts.hidden = true;
  footer.append(parentLabel, label, finishParts, create);
  panel.append(header, canvasWrap, controls, footer);
  view.append(panel);

  const context = canvas.getContext("2d");
  if (!context) throw new Error("The image object picker has no canvas context.");
  const image = new Image();
  image.decoding = "async";
  image.src = currentSource.url;

  let state = "idle";
  let mode = "positive";
  let points = [];
  let selection = null;
  let previewMask = null;
  let requestNumber = 0;
  let currentRequest = null;
  let openRequested = false;
  let lastAccepted = null;
  let prepared = false;
  let segments = [];
  let seriesParentId = "";
  let seriesAccepted = 0;
  const existingMaskCache = new Map();

  const segmentUsesCurrentSource = (segment) =>
    segment.segment_source_sha256 === currentSource.sha256
      && segment.segment_source_width === currentSource.size[0]
      && segment.segment_source_height === currentSource.size[1];

  const setStatus = (text, busy = false) => {
    canvasStatus.textContent = text;
    canvasStatus.classList.toggle("busy", busy);
  };

  const draw = () => {
    if (!image.complete || !image.naturalWidth) return;
    context.clearRect(0, 0, canvas.width, canvas.height);
    context.drawImage(image, 0, 0, canvas.width, canvas.height);
    const visibleMasks = seriesParentId
      ? segments.filter((segment) => segmentUsesCurrentSource(segment)
          && segment.segment_parent_id === seriesParentId)
      : segments.filter((segment) => segmentUsesCurrentSource(segment)
          && !segment.segment_parent_id);
    for (const segment of visibleMasks) {
      const width = Number(segment.segment_mask_width);
      const height = Number(segment.segment_mask_height);
      const box = segment.segment_mask_source_box;
      if (segment.segment_status !== "materialized"
          || !segment.segment_mask_runs
          || !Number.isFinite(width)
          || !Number.isFinite(height)
          || !Array.isArray(box)
          || box.length !== 4) continue;
      const cacheKey = `${segment.id}:${segment.segment_generation}`;
      let overlay = existingMaskCache.get(cacheKey);
      if (!overlay) {
        const alpha = decodeMaskRuns(segment.segment_mask_runs, width * height);
        overlay = document.createElement("canvas");
        overlay.width = width;
        overlay.height = height;
        const overlayContext = overlay.getContext("2d");
        if (!overlayContext) throw new Error("accepted mask overlay has no canvas context");
        const pixels = overlayContext.createImageData(width, height);
        for (let index = 0; index < alpha.length; index += 1) {
          if (!alpha[index]) continue;
          const offset = index * 4;
          pixels.data[offset] = 246;
          pixels.data[offset + 1] = 173;
          pixels.data[offset + 2] = 85;
          pixels.data[offset + 3] = seriesParentId ? 112 : 54;
        }
        overlayContext.putImageData(pixels, 0, 0);
        existingMaskCache.set(cacheKey, overlay);
      }
      context.drawImage(overlay, box[0], box[1], box[2] - box[0], box[3] - box[1]);
    }
    if (previewMask?.length === canvas.width * canvas.height) {
      const pixels = context.createImageData(canvas.width, canvas.height);
      for (let index = 0; index < previewMask.length; index += 1) {
        if (!previewMask[index]) continue;
        const offset = index * 4;
        pixels.data[offset] = 45;
        pixels.data[offset + 1] = 212;
        pixels.data[offset + 2] = 191;
        pixels.data[offset + 3] = 118;
      }
      const overlay = document.createElement("canvas");
      overlay.width = canvas.width;
      overlay.height = canvas.height;
      overlay.getContext("2d")?.putImageData(pixels, 0, 0);
      context.drawImage(overlay, 0, 0);
    }
    for (const point of points) {
      context.beginPath();
      context.arc(point.coordinates[0], point.coordinates[1], 12, 0, Math.PI * 2);
      context.fillStyle = point.label === "positive" ? "#21d4bf" : "#ff7187";
      context.fill();
      context.lineWidth = 4;
      context.strokeStyle = "#07101d";
      context.stroke();
    }
  };

  const prepare = () => {
    if (state !== "idle" && state !== "error") return;
    state = "preparing";
    setStatus("Preparing object selection", true);
    currentRequest = `picker-prepare-${++requestNumber}`;
    worker.postMessage({
      type: "prepare-picker",
      requestId: currentRequest,
      sourceId: currentSource.id,
    });
  };

  const resolve = () => {
    if (!prepared || !points.some((point) => point.label === "positive")) return;
    state = "resolving";
    selection = null;
    previewMask = null;
    create.disabled = true;
    setStatus("Finding the object", true);
    draw();
    currentRequest = `picker-resolve-${++requestNumber}`;
    worker.postMessage({
      type: "resolve-picker",
      requestId: currentRequest,
      sourceId: currentSource.id,
      points: points.map((point) => ({
        coordinates: [...point.coordinates],
        label: point.label,
      })),
    });
  };

  const updatePointControls = () => {
    reset.disabled = points.length === 0;
    undo.disabled = points.length === 0;
  };

  const clearSelection = (status = "Click any object in the image") => {
    if (prepared) currentRequest = `picker-stale-${++requestNumber}`;
    selection = null;
    previewMask = null;
    create.disabled = true;
    if (prepared) state = "ready";
    updatePointControls();
    setStatus(status, !prepared);
    draw();
  };

  const refreshParents = (preferred = null) => {
    const prior = preferred ?? parentSelect.value;
    parentSelect.replaceChildren();
    const root = document.createElement("option");
    root.value = "";
    root.textContent = "New object";
    parentSelect.append(root);
    const appendParentOptions = (segment, depth) => {
      if (depth < maxDepth) {
        const option = document.createElement("option");
        option.value = segment.id;
        option.textContent = `${"  ".repeat(depth)}${depth > 0 ? "↳ " : ""}${segment.label || `Object ${segment.id}`}`;
        parentSelect.append(option);
      }
      for (const child of segments.filter((candidate) => candidate.segment_parent_id === segment.id)) {
        appendParentOptions(child, depth + 1);
      }
    };
    for (const segment of segments.filter((candidate) =>
      segmentUsesCurrentSource(candidate) && !candidate.segment_parent_id)) {
      appendParentOptions(segment, 0);
    }
    if ([...parentSelect.options].some((option) => option.value === prior)) {
      parentSelect.value = prior;
    }
    const isPart = Boolean(parentSelect.value);
    seriesParentId = isPart ? parentSelect.value : "";
    parentText.textContent = isPart ? "Child of" : "Belongs to";
    labelText.textContent = isPart ? "Part name" : "Object name";
    create.textContent = isPart ? "Add part" : "Create object";
    finishParts.hidden = !isPart;
    seriesSummary.hidden = !isPart;
    panel.classList.toggle("segment-picker-series", isPart);
    sourceSelect.disabled = isPart;
    if (isPart) {
      const parent = segments.find((segment) => segment.id === seriesParentId);
      const count = segments.filter((segment) => segment.segment_parent_id === seriesParentId).length;
      heading.textContent = `Add parts to ${parent?.label || "this object"}`;
      explanation.textContent = "Accepted child masks stay highlighted. Click, confirm, and repeat without leaving this image.";
      seriesSummary.textContent = `${count} part${count === 1 ? "" : "s"} already added`;
    } else {
      heading.textContent = "Click the object you want";
      explanation.textContent = "The mask appears after one click. Click a point again to undo it, then refine only if the edge needs help.";
      seriesSummary.textContent = "";
    }
  };

  const show = (parentId = "") => {
    seriesAccepted = 0;
    const parent = typeof parentId === "string"
      ? segments.find((segment) => segment.id === parentId && segment.form === "segment")
      : null;
    if (parent) {
      const parentSource = segmentSourceByIdentity(
        parent.segment_source_sha256,
        [parent.segment_source_width, parent.segment_source_height],
      );
      if (parentSource.id !== currentSource.id) selectSource(parentSource.id, false);
    }
    refreshParents(typeof parentId === "string" ? parentId : "");
    const existingParts = parentSelect.value
      ? segments.filter((segment) => segment.segment_parent_id === parentSelect.value).length
      : 0;
    labelInput.value = parentSelect.value ? `Part ${existingParts + 1}` : "Object";
    panel.hidden = false;
    launcher.setAttribute("aria-pressed", "true");
    openRequested = true;
    prepare();
    requestAnimationFrame(draw);
  };

  const hide = () => {
    const completedParent = seriesAccepted > 0 ? seriesParentId : "";
    panel.hidden = true;
    launcher.setAttribute("aria-pressed", "false");
    openRequested = false;
    seriesParentId = "";
    seriesAccepted = 0;
    if (completedParent) finish?.(completedParent);
  };

  const setMode = (next) => {
    mode = next;
    include.classList.toggle("active", mode === "positive");
    exclude.classList.toggle("active", mode === "negative");
    include.setAttribute("aria-pressed", String(mode === "positive"));
    exclude.setAttribute("aria-pressed", String(mode === "negative"));
  };

  const selectSource = (id, prepareWhenOpen = true) => {
    const next = segmentSource(id);
    if (next.id === currentSource.id) return;
    currentSource = next;
    sourceSelect.value = currentSource.id;
    canvas.width = currentSource.size[0];
    canvas.height = currentSource.size[1];
    image.src = currentSource.url;
    prepared = false;
    state = "idle";
    points = [];
    selection = null;
    previewMask = null;
    currentRequest = `picker-stale-${++requestNumber}`;
    existingMaskCache.clear();
    updatePointControls();
    refreshParents("");
    setStatus("Preparing object selection", true);
    if (prepareWhenOpen && !panel.hidden) prepare();
  };

  launcher.addEventListener("click", () => show(""));
  close.addEventListener("click", hide);
  finishParts.addEventListener("click", hide);
  labelInput.addEventListener("focus", () => labelInput.select());
  parentSelect.addEventListener("change", () => {
    refreshParents(parentSelect.value);
    const count = segments.filter((segment) => segment.segment_parent_id === parentSelect.value).length;
    labelInput.value = parentSelect.value ? `Part ${count + 1}` : "Object";
    draw();
  });
  sourceSelect.addEventListener("change", () => selectSource(sourceSelect.value));
  include.addEventListener("click", () => setMode("positive"));
  exclude.addEventListener("click", () => setMode("negative"));
  reset.addEventListener("click", () => {
    points = [];
    setMode("positive");
    clearSelection("Click any object in the image");
  });
  undo.addEventListener("click", () => {
    if (!points.length) return;
    points = points.slice(0, -1);
    if (!points.some((point) => point.label === "positive")) {
      clearSelection("Point removed. Add a point inside the object.");
    } else {
      updatePointControls();
      resolve();
    }
  });

  canvas.addEventListener("click", (event) => {
    if (event.button !== 0 || !event.isTrusted) return;
    const coordinates = pointFromCanvas(event, canvas, currentSource.size);
    const labelForPoint = event.altKey ? "negative" : mode;
    // The second click in a double-click may only remove. This keeps the first
    // click from removing an existing point and the second silently adding it
    // straight back.
    if (event.detail > 1) {
      const toggled = togglePromptPoint(points, coordinates, labelForPoint);
      if (!toggled.removed) return;
      points = toggled.points;
      if (!points.some((point) => point.label === "positive")) {
        clearSelection("Point removed. Add a point inside the object.");
      } else {
        updatePointControls();
        resolve();
      }
      return;
    }
    if (labelForPoint === "negative" && !points.some((point) => point.label === "positive")) {
      setStatus("Add one point inside the object first");
      return;
    }
    const toggled = togglePromptPoint(points, coordinates, labelForPoint);
    points = toggled.points;
    updatePointControls();
    if (toggled.removed && !points.some((point) => point.label === "positive")) {
      clearSelection("Point removed. Add a point inside the object.");
      return;
    }
    draw();
    if (!prepared) {
      setStatus("The model is almost ready", true);
      return;
    }
    resolve();
  });

  create.addEventListener("click", async () => {
    if (!selection || state !== "ready") return;
    state = "creating";
    create.disabled = true;
    setStatus("Creating Atlas object", true);
    try {
      const id = await accept({
        proposal: atlasProposal(
          labelInput.value,
          selection,
          placement(selection, parentSelect.value),
          parentSelect.value,
        ),
        materialization: atlasMaterialization(selection),
        points: points.map((point) => ({ ...point, coordinates: [...point.coordinates] })),
      });
      lastAccepted = {
        stableObjectId: id,
        label: labelInput.value.trim(),
        pointCount: points.length,
        trustedPointCount: points.filter((point) => point.trusted).length,
        modelId: selection.model_id,
        encoderSha256: selection.encoder_sha256,
        decoderSha256: selection.decoder_sha256,
        predictedIou: selection.predicted_iou,
        maskSourceBox: [...selection.mask_source_box],
        sourceId: currentSource.id,
        parentId: parentSelect.value || null,
        receipt: structuredClone(selection.receipt),
      };
      report(parentSelect.value
        ? `Created ${labelInput.value.trim()} as a mask part of ${parentSelect.selectedOptions[0]?.textContent || "the object"}.`
        : `Created ${labelInput.value.trim()} as Atlas object ${id}. Drag it to move it.`);
      const acceptedLabel = labelInput.value.trim();
      const acceptedParent = parentSelect.value;
      points = [];
      selection = null;
      previewMask = null;
      state = "ready";
      create.disabled = true;
      updatePointControls();
      if (acceptedParent) {
        seriesAccepted += 1;
        refreshParents(acceptedParent);
        labelInput.value = nextNumberedPartLabel(acceptedLabel);
        labelInput.focus({ preventScroll: true });
        labelInput.select();
        setStatus(`${acceptedLabel} added. Click the next part.`);
        draw();
      } else {
        hide();
      }
    } catch (error) {
      state = "ready";
      create.disabled = false;
      setStatus(error instanceof Error ? error.message : String(error));
    }
  });

  image.addEventListener("load", draw);
  image.addEventListener("error", () => {
    state = "error";
    setStatus("The source image could not be loaded");
  }, { once: true });

  return {
    show,
    hide,
    handleMessage(data) {
      if (data?.requestId !== currentRequest) return false;
      if (data.type === "picker-error") {
        state = "error";
        setStatus(data.error || "Object selection failed");
        return true;
      }
      if (data.type === "picker-ready") {
        prepared = true;
        state = "ready";
        setStatus(points.length ? "Finding the object" : "Click any object in the image", points.length > 0);
        if (points.length) resolve();
        return true;
      }
      if (data.type === "picker-result") {
        state = "ready";
        selection = data.result;
        previewMask = new Uint8Array(data.previewMask);
        create.disabled = false;
        setStatus("Mask ready. Refine it or create the object.");
        draw();
        return true;
      }
      return false;
    },
    handleWorkerError(message) {
      if (!openRequested) return;
      state = "error";
      setStatus(message || "The object selection worker stopped");
    },
    refresh(atlas) {
      segments = atlas.shapes.filter((shape) => shape.form === "segment");
      if (!panel.hidden) {
        refreshParents(parentSelect.value);
        draw();
      }
    },
    receipt() {
      return {
        state,
        pointCount: points.length,
        trustedPointCount: points.filter((point) => point.trusted).length,
        hasSelection: Boolean(selection),
        open: !panel.hidden,
        sourceId: currentSource.id,
        sourceSha256: currentSource.sha256,
        seriesParentId: seriesParentId || null,
        seriesAccepted,
        visibleAcceptedMasks: seriesParentId
          ? segments.filter((segment) => segmentUsesCurrentSource(segment)
              && segment.segment_parent_id === seriesParentId).length
          : segments.filter((segment) => segmentUsesCurrentSource(segment)
              && !segment.segment_parent_id).length,
        lastAccepted: structuredClone(lastAccepted),
      };
    },
    dispose() {
      panel.remove();
      launcher.remove();
    },
  };
}
