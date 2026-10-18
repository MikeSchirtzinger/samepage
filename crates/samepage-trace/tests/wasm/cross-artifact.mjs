// The independently built doc and map binaries report through one host hook.
// Usage: node cross-artifact.mjs <build-capabilities output>
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';
const root=path.resolve(process.argv[2]);
async function load(id,pkg) {
 const cap=await import(pathToFileURL(path.join(root,id,pkg+'.js')));
 await cap.default({module_or_path:fs.readFileSync(path.join(root,id,pkg+'_bg.wasm'))});
 return cap;
}
const trace=await load('trace','samepage_trace');
globalThis.__samepageTrace=raw=>{const result=JSON.parse(trace.trace_operation(JSON.stringify({operation:'record',record:JSON.parse(raw)})));assert(!result.error,JSON.stringify(result));};
const doc=await load('doc','samepage_doc_web');const map=await load('map','samepage_map');
const replica=new doc.Replica();replica.set_composition(JSON.stringify([JSON.parse(map.descriptor()).ok]));
const batch=JSON.parse(map.plan(JSON.stringify({action:'card_place',args:{label:'cross-artifact',x:0,y:0}}),replica.snapshot())).ok;
replica.apply_batch(JSON.stringify(batch));
const snapshot=JSON.parse(trace.trace_operation(JSON.stringify({operation:'snapshot'}))).ok;
const opens=snapshot.records.filter(r=>r.kind==='span_open');
assert(opens.some(r=>r.capability==='doc'));assert(opens.some(r=>r.capability==='map'));
assert.equal(new Set(opens.map(r=>r.span_id)).size,opens.length);
for(const cap of ['doc','map']) assert(opens.some(r=>r.capability===cap&&r.parent_span_id&&opens.some(p=>p.span_id===r.parent_span_id&&p.capability===cap)));
assert(!JSON.stringify(snapshot).includes('negative-control'));
console.log(JSON.stringify({result:'PASS',test:'cross_artifact_capture_keeps_doc_and_map_ids_and_parents_distinct',snapshot},null,2));replica.free();
