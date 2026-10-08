// Calls the real samepage-cement wasm artifact with every case in cases.json and
// writes the raw envelopes, so the native test can compare them one by one.
// Usage: node check.mjs <pkg dir> <cases.json> <results.json>
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { readFileSync, writeFileSync } from "node:fs";
import assert from "node:assert/strict";

const map = createRequire(import.meta.url)(resolve(process.argv[2], "samepage_cement.js"));
const { view, calls } = JSON.parse(readFileSync(process.argv[3], "utf8"));
const viewJson = JSON.stringify(view);
assert.equal(map.abi(), "samepage.capability.v2");

const results = calls.map((c) => {
  const viewJson = JSON.stringify(c.view || view);
  switch (c.export) {
    case "descriptor": return map.descriptor();
    case "obligations_with_facts": return map.obligations_with_facts(viewJson,JSON.stringify(c.args),JSON.stringify(c.facts));
    case "bundle_with_facts": return map.bundle_with_facts(viewJson,JSON.stringify(c.args),JSON.stringify(c.facts));
    case "project": return map.project(viewJson);
    case "digest": return map.digest(viewJson);
    case "legend": return map.legend(viewJson);
    case "plan": return map.plan(typeof c.call === "string" ? c.call : JSON.stringify(c.call), viewJson);
    case "validate": return map.validate(JSON.stringify(c.delta), viewJson);
    case "cascade": return map.cascade(JSON.stringify(c.deleted), viewJson);
    case "changes": {
      const after = c.after === "project" ? JSON.parse(map.project(viewJson)).ok : c.after;
      return map.changes(JSON.stringify(c.before), JSON.stringify(after), "agent");
    }
    default: throw new Error(`unknown export ${c.export}`);
  }
});
writeFileSync(process.argv[4], JSON.stringify(results, null, 1));
console.log(`samepage-cement wasm: ${results.length} calls answered`);
