import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { readFileSync, writeFileSync } from 'node:fs';
import assert from 'node:assert/strict';
const wasm=createRequire(import.meta.url)(resolve(process.argv[2],'samepage_board.js'));
const cases=JSON.parse(readFileSync(process.argv[3],'utf8'));
assert.equal(wasm.abi(),'samepage.capability.v2');
const raw=v=>typeof v==='string'?v:JSON.stringify(v);
const results=cases.calls.map(c=>{
 const v=JSON.stringify(c.view||cases.view),b=raw(c.board),s=JSON.stringify(c.since??0);
 switch(c.export){
 case 'descriptor':return wasm.descriptor();
 case 'project':return wasm.project(v);
 case 'digest':return wasm.digest(v);
 case 'legend':return wasm.legend(v);
 case 'plan':return wasm.plan(raw(c.call),v);
 case 'validate':return wasm.validate(JSON.stringify(c.delta),v);
 case 'cascade':return wasm.cascade(JSON.stringify(c.deleted),v);
 case 'transition':return wasm.transition(b,raw(c.call),raw(c.context));
 case 'project_board':return wasm.project_board(b,s);
 case 'validate_board':return wasm.validate_board(b);
 case 'delta':return wasm.delta(b,s);
 case 'migrate':return wasm.migrate(b);
 case 'readback':return wasm.readback(b,s);
 case 'advisory_preview':return wasm.advisory_preview(b,c.revision,c.timestamp);
 case 'changes':return wasm.changes(JSON.stringify(c.before),JSON.stringify(c.after),'agent');
 default:throw Error(`unknown export ${c.export}`);
 }
});
writeFileSync(process.argv[4],JSON.stringify(results,null,1));
console.log(`samepage-board wasm: ${results.length} calls answered`);
