// Loads the fixture's real wasm module (through wasm-bindgen's Node glue),
// calls every export over the shared cases, asserts the ABI v2 export
// contract, and writes each raw envelope out for the native equivalence test.
// Usage: node check.mjs <dir containing samepage_capability_fixture.js> <results.json>
import { createRequire } from "node:module";
import { resolve, dirname } from "node:path";
import { writeFileSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = createRequire(import.meta.url)(resolve(process.argv[2], "samepage_capability_fixture.js"));
const resultsPath = process.argv[3];
const ABI = "samepage.capability.v2";

// The export contract: exactly these nine, and no second planning export.
const EXPORTS = ["abi", "descriptor", "project", "plan", "validate", "cascade", "digest", "changes", "legend"];
const found = EXPORTS.filter((name) => typeof pkg[name] === "function");
assert.deepEqual(found, EXPORTS, `missing exports: ${EXPORTS.filter((n) => !found.includes(n))}`);
assert.equal(typeof pkg.plan_batch, "undefined", "ABI v2 has one planning export, not two");
const extra = Object.keys(pkg).filter((name) => typeof pkg[name] === "function" && !EXPORTS.includes(name));
assert.deepEqual(extra, [], `unexpected exports: ${extra}`);

const ok = (text) => {
  const envelope = JSON.parse(text);
  assert.equal(envelope.abi, ABI);
  assert.ok(!("error" in envelope), `unexpected error: ${text}`);
  return envelope.ok;
};
const err = (text) => {
  const envelope = JSON.parse(text);
  assert.equal(envelope.abi, ABI);
  assert.ok("error" in envelope, `expected an error: ${text}`);
  return envelope.error;
};

const cases = JSON.parse(readFileSync(resolve(here, "cases.json"), "utf8"));
const view = JSON.stringify(cases.view);
assert.equal(pkg.abi(), ABI);

const results = cases.calls.map((c) => {
  switch (c.export) {
    case "descriptor": return pkg.descriptor();
    case "project": return pkg.project(view);
    case "digest": return pkg.digest(view);
    case "legend": return pkg.legend(view);
    case "plan": return pkg.plan("raw" in c ? c.raw : JSON.stringify(c.call), view);
    case "validate": return pkg.validate(JSON.stringify(c.delta), view);
    case "cascade": return pkg.cascade(JSON.stringify(c.deleted), view);
    case "changes": return pkg.changes("{}", "{}", "agent");
    default: throw new Error(`unknown export ${c.export}`);
  }
});
const at = (index) => results[index];

const descriptor = ok(at(0));
assert.equal(descriptor.id, "pins");
assert.deepEqual(descriptor.kinds[0].keys, ["label", "x", "y", "note"]);
assert.deepEqual(descriptor.actions[0].aliases, ["fixture_pin"]);

assert.deepEqual(ok(at(1)), { pins: [{ id: "p1", label: "Start" }] });

// Planning answers with a Batch: one self-owned entry by default.
const add = ok(at(4));
assert.equal(add.base_revision, "r1");
assert.equal(add.entries.length, 1);
assert.equal(add.entries[0].capability, "pins");
assert.deepEqual(add.entries[0].plan.ops, [
  { op: "create", ref: "pin", kind: "pin", props: { label: "Queue", x: 0.0 } },
]);
assert.equal(add.entries[0].plan.readback, "Added pin Queue.");
assert.deepEqual(ok(at(5)).entries[0].plan.ops, [{ op: "set", target: { id: "p1" }, props: { x: 42.0 } }]);

// An override carries ordered entries for two owners, with a reference that
// the second entry resolves from the first.
const scaffold = ok(at(6));
assert.deepEqual(scaffold.entries.map((e) => e.capability), ["notes", "pins"]);
assert.deepEqual(scaffold.entries[0].plan.ops, [
  { op: "create", ref: "note", kind: "note", props: { text: "Queue" } },
]);
assert.deepEqual(scaffold.entries[1].plan.ops[0].props.note, { $ref: "note" });
assert.deepEqual(scaffold.entries.map((e) => e.plan.readback), [
  "Wrote a note about Queue.",
  "Pinned Queue to it.",
]);
assert.equal(err(at(7)).code, "invalid_args");

assert.equal(err(at(8)).code, "not_found");
assert.equal(err(at(9)).code, "invalid_args");
assert.equal(err(at(14)).code, "bad_input");
assert.equal(err(pkg.project("[]")).code, "bad_input");

const badView = JSON.stringify({ revision: "r2", records: [{ id: "p9", kind: "pin", props: { x: 3 } }] });
const delta = JSON.stringify(cases.calls[15].delta);
assert.equal(err(pkg.validate(delta, badView)).code, "invalid_pin");
assert.equal(ok(at(15)), null);

assert.deepEqual(ok(at(16)), { ops: [], readback: "" });

// Numbers computed inside the module: finite passes, NaN and both infinities
// must come back as error envelopes, never as ok with the value nulled.
assert.equal(ok(at(10)).entries[0].plan.ops[0].props.x, 3);
for (const [index, label] of [[11, "NaN"], [12, "+Infinity"], [13, "-Infinity"]]) {
  assert.equal(err(at(index)).code, "non_finite_output", `plan ${label}: ${at(index)}`);
  assert.ok(!at(index).includes("null"), `plan ${label} leaked a null: ${at(index)}`);
}
assert.equal(err(at(18)).code, "non_finite_output");
assert.ok(!at(18).includes("null"));
assert.equal(ok(at(17)).ops[0].props.x, 4);
assert.deepEqual(ok(at(2)), { pins: [{ id: "p1", label: "Start" }] });
assert.deepEqual(ok(at(19)), []);
assert.equal(ok(at(3))[0].symbol, "•");

writeFileSync(resultsPath, JSON.stringify(results));
console.log(`fixture wasm: all 9 exports answered as expected over ${results.length} calls`);
