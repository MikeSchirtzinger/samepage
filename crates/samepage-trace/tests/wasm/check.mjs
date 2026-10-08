import { createRequire } from "node:module";
import { resolve } from "node:path";
import { readFileSync,writeFileSync } from "node:fs";
import assert from "node:assert/strict";
const cap=createRequire(import.meta.url)(resolve(process.argv[2],"samepage_trace.js"));
const {view,calls}=JSON.parse(readFileSync(process.argv[3],"utf8"));const v=JSON.stringify(view);
assert.equal(cap.abi(),"samepage.capability.v2");
const results=calls.map(c=>{switch(c.export){
case "descriptor":return cap.descriptor();case "project":return cap.project(v);case "digest":return cap.digest(v);case "legend":return cap.legend(v);
case "plan":return cap.plan(typeof c.call==="string"?c.call:JSON.stringify(c.call),v);
case "validate":return cap.validate(JSON.stringify(c.delta),v);case "cascade":return cap.cascade(JSON.stringify(c.deleted),v);
case "changes":return cap.changes(JSON.stringify(c.before),JSON.stringify(c.after),"agent");
case "trace_operation":return cap.trace_operation(JSON.stringify(c.input));default:throw Error(c.export);}});
writeFileSync(process.argv[4],JSON.stringify(results,null,2));console.log(`samepage-trace wasm: ${results.length} calls answered`);
