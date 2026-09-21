import test from "node:test";
import assert from "node:assert/strict";
import { focusGraph, linkedDetailViews } from "../static/explore-graph.js";

const ids = values => [...values].sort();
test("a component opens its descendants and immediate interfaces, excluding siblings and distant dependencies", () => {
  const page = { nodes: [
    { id: "system" }, { id: "runtime", parent: "system" },
    { id: "planner", parent: "runtime" }, { id: "router", parent: "runtime" },
    { id: "policy", parent: "system" }, { id: "tool", parent: "system" },
    { id: "unrelated", parent: "system" },
  ], edges: [{ from: "planner", to: "router" }, { from: "router", to: "policy" }, { from: "policy", to: "tool" }] };
  const before = JSON.stringify(page);
  assert.deepEqual(ids(focusGraph(page, "runtime").visible), ["planner", "policy", "router", "runtime"]);
  assert.deepEqual(ids(focusGraph(page, "router").visible), ["planner", "policy", "router"]);
  assert.equal(JSON.stringify(page), before, "navigation must not edit shared subjects, edges, or layout");
});

test("missing objects and dangling relationships cannot create detail", () => {
  assert.equal(focusGraph({ nodes: [], edges: [] }, "gone"), null);
  const result = focusGraph({ nodes: [{ id: "a" }], edges: [{ from: "a", to: "missing" }] }, "a");
  assert.deepEqual([...result.visible], ["a"]);
});

test("ancestor back edges do not expose the ancestor frame or its other children", () => {
  const page = { nodes: [{ id: "root" }, { id: "one", parent: "root" }, { id: "two", parent: "root" }], edges: [{ from: "one", to: "root" }] };
  assert.deepEqual([...focusGraph(page, "one").visible], ["one"]);
});

test("an unscoped tour is not advertised as separate detail for every mentioned card", () => {
  const flow = (id, about, scene_ids = []) => ({ definition: { id, about, scene_ids } });
  const page = { explanations: [flow("tour", ["router", "policy"]), flow("permissions", ["router", "policy"], ["owned-detail"]), flow("router-only", ["router"]), flow("other", ["store"], ["store-detail"])] };
  assert.deepEqual(linkedDetailViews(page, "router").map(item => item.definition.id), ["permissions", "router-only"]);
  assert.deepEqual(linkedDetailViews(page, "missing"), []);
});
