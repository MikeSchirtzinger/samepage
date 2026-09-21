import test from "node:test";
import assert from "node:assert/strict";
import { sourceReferences } from "../static/source-view.js";

test("code links include only this node's live evidence and retain distinct ranges", () => {
  const node = { id: "component", path: "src/main.rs", lines: "1-4" };
  const references = sourceReferences(node, [
    { about: "component", path: "src/main.rs", lines: "1-4" },
    { about: "component", path: "src/main.rs", lines: "20-40" },
    { about: "component", path: "src/old.rs", withdrawn: true },
    { about: "another", path: "src/unrelated.rs" },
  ]);
  assert.deepEqual(references.map(({path, lines}) => ({path, lines})), [
    { path: "src/main.rs", lines: "1-4" },
    { path: "src/main.rs", lines: "20-40" },
  ]);
});

test("a card without its own path can open a live claim's source", () => {
  assert.equal(sourceReferences({id: "component"}, [{about: "component", path: "src/lib.rs"}])[0].path, "src/lib.rs");
  assert.deepEqual(sourceReferences({id: "unlinked"}), []);
});
