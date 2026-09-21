import assert from "node:assert/strict";
import {
  atlasMaterialization,
  atlasProposal,
  decodeMaskRuns,
  nextNumberedPartLabel,
  pointFromCanvas,
  togglePromptPoint,
} from "../../static/extensions/atlas/segment-picker.js";

assert.equal(nextNumberedPartLabel("Claw 1"), "Claw 2");
assert.equal(nextNumberedPartLabel("front claw 09"), "front claw 10");
assert.equal(nextNumberedPartLabel("Claw"), "Claw 2");
assert.equal(nextNumberedPartLabel(""), "Part 1");

const rle = Buffer.from(new Uint8Array(new Uint32Array([2, 3, 1]).buffer)).toString("base64");
assert.deepEqual([...decodeMaskRuns(rle, 6)], [0, 0, 1, 1, 1, 0]);
assert.throws(() => decodeMaskRuns(rle, 7), /covers 6 pixels, expected 7/);

const result = {
  model_id: "mobilesam-vit-t",
  encoder_sha256: "a".repeat(64),
  decoder_sha256: "b".repeat(64),
  predicted_iou: 0.91,
  mask_encoding: "mask-alpha-rle-u32le-base64-v1",
  mask_width: 120,
  mask_height: 220,
  mask_runs: "AAAAAA==",
  mask_source_box: [544, 437, 664, 657],
  receipt: { backendRequested: "webgpu", fallbackUsed: false, mockUsed: false },
};

assert.deepEqual(
  pointFromCanvas(
    { clientX: 210, clientY: 120 },
    { getBoundingClientRect: () => ({ left: 10, top: 20, width: 400, height: 200 }) },
    [1536, 1024],
  ),
  [768, 512],
);

assert.deepEqual(
  pointFromCanvas(
    { clientX: 210, clientY: 210 },
    { getBoundingClientRect: () => ({ left: 10, top: 10, width: 400, height: 400 }) },
    [1536, 1024],
  ),
  [768, 512],
);
assert.throws(
  () => pointFromCanvas(
    { clientX: 210, clientY: 20 },
    { getBoundingClientRect: () => ({ left: 10, top: 10, width: 400, height: 400 }) },
    [1536, 1024],
  ),
  /inside the source image/,
);

assert.deepEqual(atlasProposal(" Left wing ", result, { x: 100, y: 200, w: 320 }, "owl-1"), {
  label: "Left wing",
  tags: [],
  ocr: [],
  prompt_box: [544, 437, 664, 657],
  x: 100,
  y: 200,
  w: 320,
  animation: "none",
  parent_id: "owl-1",
});

const firstPoint = togglePromptPoint([], [700, 500], "positive");
assert.equal(firstPoint.removed, false);
assert.equal(firstPoint.points.length, 1);
assert.deepEqual(firstPoint.points[0], {
  coordinates: [700, 500],
  label: "positive",
  trusted: true,
});
const removedPoint = togglePromptPoint(firstPoint.points, [710, 506], "negative");
assert.equal(removedPoint.removed, true);
assert.deepEqual(removedPoint.points, []);
const distantPoint = togglePromptPoint(firstPoint.points, [760, 500], "negative");
assert.equal(distantPoint.removed, false);
assert.equal(distantPoint.points.length, 2);

const materialization = atlasMaterialization(result);
assert.equal(materialization.generation, 1);
assert.deepEqual(materialization.parts, []);
assert.notEqual(materialization.receipt, result.receipt);
materialization.receipt.fallbackUsed = true;
assert.equal(result.receipt.fallbackUsed, false);

assert.throws(
  () => atlasProposal("", result, { x: 0, y: 0, w: 320 }),
  /Name the object/,
);
assert.throws(
  () => atlasMaterialization({ ...result, mask_runs: undefined }),
  /missing mask_runs/,
);

console.log("segment picker contracts: PASS");
