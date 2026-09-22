// Loads the fixture's real wasm module (through wasm-bindgen's Node glue) and
// calls every export. Usage: node check.mjs <dir containing samepage_capability_fixture.js>
import { createRequire } from "node:module";
import { resolve } from "node:path";
import assert from "node:assert/strict";

const pkg = createRequire(import.meta.url)(resolve(process.argv[2], "samepage_capability_fixture.js"));
const ABI = "samepage.capability.v1";
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

const view = JSON.stringify({
  revision: "r1",
  records: [
    { id: "p1", kind: "pin", props: { label: "Start", x: 1 } },
    { id: "u1", kind: "unknown-kind", props: { anything: true } },
  ],
});

assert.equal(pkg.abi(), ABI);

const descriptor = ok(pkg.descriptor());
assert.equal(descriptor.id, "pins");
assert.deepEqual(descriptor.kinds[0].keys, ["label", "x", "y"]);
assert.deepEqual(descriptor.actions[0].aliases, ["fixture_pin"]);

assert.deepEqual(ok(pkg.project(view)), { pins: [{ id: "p1", label: "Start" }] });

const add = ok(pkg.plan(JSON.stringify({ action: "pin_add", args: { label: "Queue" } }), view));
assert.deepEqual(add.ops, [{ op: "create", ref: "pin", kind: "pin", props: { label: "Queue", x: 0.0 } }]);
assert.equal(add.readback, "Added pin Queue.");

const move = ok(pkg.plan(JSON.stringify({ action: "pin_move", args: { id: "p1", x: 42 } }), view));
assert.deepEqual(move.ops, [{ op: "set", target: { id: "p1" }, props: { x: 42.0 } }]);

assert.equal(err(pkg.plan(JSON.stringify({ action: "pin_move", args: { id: "nope", x: 1 } }), view)).code, "not_found");
assert.equal(err(pkg.plan(JSON.stringify({ action: "pin_add", args: {} }), view)).code, "invalid_args");
assert.equal(err(pkg.plan("{broken", view)).code, "bad_input");
assert.equal(err(pkg.project("[]")).code, "bad_input");

const badView = JSON.stringify({ revision: "r2", records: [{ id: "p9", kind: "pin", props: { x: 3 } }] });
const delta = JSON.stringify({ created: ["p9"], modified: {}, deleted: [], rekinded: [] });
assert.equal(err(pkg.validate(delta, badView)).code, "invalid_pin");
assert.equal(ok(pkg.validate(delta, view)), null);

assert.deepEqual(ok(pkg.cascade(JSON.stringify(["p1"]), view)), { ops: [], readback: "" });

// Numbers computed inside the module: finite passes, NaN and both infinities
// must come back as error envelopes, never as ok with the value nulled.
const moved = ok(pkg.plan(JSON.stringify({ action: "pin_move", args: { id: "p1", x: 9, divide_by: 3 } }), view));
assert.equal(moved.ops[0].props.x, 3);
for (const [x, label] of [[0, "NaN"], [1, "+Infinity"], [-1, "-Infinity"]]) {
  const planText = pkg.plan(JSON.stringify({ action: "pin_move", args: { id: "p1", x, divide_by: 0 } }), view);
  assert.equal(err(planText).code, "non_finite_output", `plan ${label}: ${planText}`);
  assert.ok(!planText.includes("null"), `plan ${label} leaked a null: ${planText}`);
  const cascadeText = pkg.cascade(JSON.stringify([`ratio:${x}:0`]), view);
  assert.equal(err(cascadeText).code, "non_finite_output", `cascade ${label}: ${cascadeText}`);
  assert.ok(!cascadeText.includes("null"), `cascade ${label} leaked a null: ${cascadeText}`);
}
assert.equal(ok(pkg.cascade(JSON.stringify(["ratio:8:2"]), view)).ops[0].props.x, 4);
assert.deepEqual(ok(pkg.digest(view)), { pins: [{ id: "p1", label: "Start" }] });
assert.deepEqual(ok(pkg.changes("{}", "{}", "agent")), []);
assert.equal(ok(pkg.legend(view))[0].symbol, "•");

console.log("fixture wasm: all 9 exports answered as expected");
