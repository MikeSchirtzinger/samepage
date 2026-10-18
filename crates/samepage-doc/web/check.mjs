// Loads the real doc.wasm (through wasm-bindgen's Node glue) and exercises the
// published facade. Usage: node check.mjs <pkg dir> [<file to write a frame to>]
// The optional frame file lets the native host admit a frame produced by the
// wasm replica (see tests/wasm_frame.rs). This proves the facade mechanics,
// not any product capability.
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { writeFileSync } from "node:fs";
import assert from "node:assert/strict";

const doc = createRequire(import.meta.url)(resolve(process.argv[2], "samepage_doc_web.js"));

assert.equal(doc.abi(), "samepage.document.v1");
assert.equal(doc.layout_version(), 1);

const descriptors = JSON.stringify([{
  abi: "samepage.capability.v2", id: "cards", version: "0.1.0",
  kinds: [{ kind: "card", keys: ["label", "x", "y", "note"], reason: "cards" }],
  props: [], reads: [], depends_on: [], actions: [], visual_actions: [], triggers: [],
  artifact: { type: "wasm" }, schema: 1,
}]);

// One replica per page.
const a = new doc.Replica();
assert.throws(() => new doc.Replica(), /a replica already exists/);
a.set_composition(descriptors);
assert.throws(() => a.apply("cards", "{nope"), /^Error: bad_input:/);

const hello = a.hello();
assert.ok(hello instanceof Uint8Array && hello.length > 0);
const applied = JSON.parse(a.apply("cards", JSON.stringify({
  ops: [{ op: "create", ref: "card", kind: "card", props: { label: "From wasm", x: 3 } }],
  readback: "",
})));
const id = applied.ids.card;
assert.match(id, new RegExp(`^${BigInt(a.client_id()).toString(16).padStart(16, "0")}-00000000$`));
assert.equal(applied.revision, a.revision());
assert.equal(a.take_dirty(), true);
assert.equal(a.take_dirty(), false);
assert.deepEqual(JSON.parse(a.kinds()), ["card"]);
const view = JSON.parse(a.view(JSON.stringify(["card"]), JSON.stringify([id])));
assert.equal(view.records[0].props.label, "From wasm");
assert.equal(view.records[0].props.touched_by, undefined, "the browser never stamps");
// Plans cannot write authorship.
assert.throws(() => a.apply("cards", JSON.stringify({
  ops: [{ op: "set", target: { id }, props: { touched_by: "me" } }], readback: "",
})), /^Error: plan:/);

const outbound = a.take_outbound();
assert.equal(outbound.length, 1);
const frame = outbound[0];
const revision = a.revision();
if (process.argv[3]) writeFileSync(process.argv[3], Buffer.from(frame));

// Refusal is truthful and sticky until disposal.
a.mark_refused("samepage.refused: test");
assert.equal(a.refused(), "samepage.refused: test");
assert.throws(() => a.receive(frame), /^Error: refused:/);
a.dispose();

// A fresh replica after disposal converges from the frame alone.
const b = new doc.Replica();
assert.deepEqual(b.receive(frame), []);
assert.equal(b.revision(), revision);
assert.equal(JSON.parse(b.snapshot()).records[0].id, id);
// A truncated frame is refused with the sync code.
assert.throws(() => b.receive(frame.slice(0, frame.length - 2)), /^Error: sync:/);
b.free();
const c = new doc.Replica();
c.free();

console.log("doc.wasm: facade answered as published");
