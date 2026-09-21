// Real Chrome and live MCP proof. This creates two explicitly labeled chat messages
// and temporarily edits the imported Trace Log card, restoring its authored fields.
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
const base=process.env.AGUI_PROOF_URL || 'http://127.0.0.1:8099';
const cdp=process.env.BROWSER_CDP_URL || 'http://127.0.0.1:9222';
const tabId=process.env.BROWSER_TAB_ID;
const token=process.env.AGUI_MCP_TOKEN;
if(!tabId || !token) throw Error('BROWSER_TAB_ID and AGUI_MCP_TOKEN are required');
const output=path.resolve(process.argv[2] || '.local/workspace-proof/'+new Date().toISOString().replaceAll(':','-'));
await fs.mkdir(output,{recursive:true});
const gitHead=execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim();
const gitDiffSha256=createHash('sha256').update(execFileSync('git',['diff','--binary','HEAD'])).digest('hex');
const sourceFiles=execFileSync('git',['ls-files','--cached','--others','--exclude-standard','crates/ag-ui-surface','examples/same-page-atlas'],{encoding:'utf8'}).trim().split('\n').filter(file=>/\.(rs|js|mjs|css|html|json|toml)$/.test(file));
const sourceHashes=Object.fromEntries(await Promise.all(sourceFiles.map(async file=>[file,createHash('sha256').update(await fs.readFile(file)).digest('hex')])));
await fs.writeFile(path.join(output,'source-hashes.json'),JSON.stringify({gitHead,gitDiffSha256,files:sourceHashes},null,2));
const targets=await fetch(cdp+'/json/list').then(r=>r.json());
const target=targets.find(t=>t.id===tabId);
assert.ok(target?.url.startsWith(base),'The isolated proof tab must already show the intended local app');
const socket=new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
let id=0; const pending=new Map(); const errors=[]; const checks=[]; const artifacts=[];
socket.addEventListener('message',event=>{const message=JSON.parse(event.data);if(message.id){const p=pending.get(message.id);pending.delete(message.id);if(message.error)p?.reject(Error(JSON.stringify(message.error)));else p?.resolve(message.result);}else if(message.method==='Runtime.exceptionThrown')errors.push(message.params.exceptionDetails);});
function send(method,params={}) { return new Promise((resolve,reject)=>{const next=++id;pending.set(next,{resolve,reject});socket.send(JSON.stringify({id:next,method,params}));}); }
async function evaluate(expression) { const result=await send('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(result.exceptionDetails)throw Error(JSON.stringify(result.exceptionDetails));return result.result.value; }
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function until(fn,label){for(let i=0;i<100;i++){if(await fn())return;await delay(100);}throw Error('Timed out: '+label);}
async function click(selector,exactText=null){const rect=await evaluate(`(()=>{const nodes=[...document.querySelectorAll(${JSON.stringify(selector)})];const node=nodes.find(n=>${exactText===null?'true':`n.textContent.trim()===${JSON.stringify(exactText)}`});if(!node) return null;node.scrollIntoView({block:'nearest'});const r=node.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2,w:r.width,h:r.height};})()`);assert.ok(rect?.w>0 && rect.h>0,'Visible control: '+selector+' '+exactText);await send('Input.dispatchMouseEvent',{type:'mousePressed',x:rect.x,y:rect.y,button:'left',clickCount:1});await send('Input.dispatchMouseEvent',{type:'mouseReleased',x:rect.x,y:rect.y,button:'left',clickCount:1});await delay(180);}
async function key(key,code=key,modifiers=0){await send('Input.dispatchKeyEvent',{type:'keyDown',key,code,modifiers,windowsVirtualKeyCode:({Backspace:8,Enter:13,Escape:27}[key]||key.toUpperCase().charCodeAt(0))});await send('Input.dispatchKeyEvent',{type:'keyUp',key,code,modifiers,windowsVirtualKeyCode:({Backspace:8,Enter:13,Escape:27}[key]||key.toUpperCase().charCodeAt(0))});}
async function insert(text){await send('Input.insertText',{text});await delay(150);}
async function screenshot(name){const file=name+'.png';const image=await send('Page.captureScreenshot',{format:'png'});await fs.writeFile(path.join(output,file),Buffer.from(image.data,'base64'));artifacts.push(file);console.log('captured',file);}
async function save(name,value){await fs.writeFile(path.join(output,name),JSON.stringify(value,null,2));artifacts.push(name);}
function pass(name,details={}){checks.push({name,pass:true,...details});console.log('PASS',name);}
let session;let rpcId=0;
async function rpc(method,params){const headers={'Content-Type':'application/json',Accept:'application/json, text/event-stream',Authorization:`Bearer ${token}`,'MCP-Protocol-Version':'2025-06-18'};if(session)headers['Mcp-Session-Id']=session;const response=await fetch(base+'/mcp',{method:'POST',headers,body:JSON.stringify({jsonrpc:'2.0',id:++rpcId,method,params}),signal:AbortSignal.timeout(20000)});if(response.headers.has('Mcp-Session-Id'))session=response.headers.get('Mcp-Session-Id');assert.ok(response.ok,'MCP HTTP '+response.status);const body=await response.text();const result=JSON.parse(body.startsWith('event:')?body.split('\n').find(l=>l.startsWith('data:')).slice(5):body);assert.ok(!result.error,JSON.stringify(result.error));return result.result;}
const call=(name,args={})=>rpc('tools/call',{name,arguments:args});
const snapshot=()=>evaluate('JSON.parse(atlas.doc.snapshot())');
const journal=()=>fetch(base+'/activity').then(r=>r.json());
const flowId='agent-tool-call-workflow';
let restore=null;
try {
 await send('Runtime.enable');await send('Page.enable');
 await send('Emulation.setDeviceMetricsOverride',{width:1440,height:900,deviceScaleFactor:1,mobile:false});
 await send('Page.reload',{ignoreCache:true});await until(()=>evaluate('Boolean(globalThis.atlas?.doc)'), 'live WASM replica');
 await rpc('initialize',{protocolVersion:'2025-06-18',capabilities:{},clientInfo:{name:'samepage-browser-proof',version:'1'}});
 await until(async()=>((await snapshot()).explanations || []).some(f=>f.definition.id===flowId),'initial shared workflow sync');
 const initial=await snapshot();await save('before.json',initial);
 assert.ok(initial.explanations.some(f=>f.definition.id===flowId),'Imported workflow exploration exists');
 if(await evaluate('Boolean(document.querySelector(".in-exploration"))')) await click(".exploration-home");
 await screenshot('01-blueprint');
 await click(`.guided-view[data-exploration-id="${flowId}"]`);
 const sharedBefore=(await snapshot()).explanations.find(f=>f.definition.id===flowId).state;
 await screenshot('02-workflow-overview');
 await click('.exploration-step[data-beat-id="recovery"]');
 assert.equal(await evaluate('document.querySelector(".walkthrough-state").textContent'),'Reviewing locally');
 const sharedAfter=(await snapshot()).explanations.find(f=>f.definition.id===flowId).state;
 assert.deepEqual(sharedAfter,sharedBefore);pass('Local step review preserves shared cursor');
 assert.ok(await evaluate('Boolean(document.querySelector(".in-exploration.step-focused"))'));await screenshot('03-focused-review');
 await click('.workspace-find');await insert('Agent tool call');
 assert.ok(await evaluate('[...document.querySelectorAll(".canvas-search-results button")].some(n=>n.textContent.includes("Exploration"))'));
 await key('Escape');pass('Search includes named explorations');
 await click('.exploration-edit');
 await click('.workspace-find');await insert('Trace Log');await click('.canvas-search-results button');
 await until(()=>evaluate('Boolean(document.querySelector(".node.selected"))'),'search selects shared object');
 const beforeEdit=(await snapshot()).nodes.find(n=>n.label==='Trace Log');assert.ok(beforeEdit);
 restore={id:beforeEdit.id,x:beforeEdit.x,y:beforeEdit.y,note:beforeEdit.note};
 const nodeSelector=`.node[data-id="${beforeEdit.id}"]`;
 const rect=await evaluate(`(()=>{const r=document.querySelector(${JSON.stringify(nodeSelector)}).getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height*.65};})()`);
 await send('Input.dispatchMouseEvent',{type:'mouseMoved',x:rect.x,y:rect.y});
 await send('Input.dispatchMouseEvent',{type:'mousePressed',x:rect.x,y:rect.y,button:'left',clickCount:1});
 await send('Input.dispatchMouseEvent',{type:'mouseMoved',x:rect.x+12,y:rect.y+8,button:'left',buttons:1});
 const proofNote='Events and output. Concurrent edit verified through live MCP.';
 const mutation=await call('atlas_draw',{nodes:[{id:beforeEdit.id,note:proofNote}]});assert.ok(!mutation.isError);
 await send('Input.dispatchMouseEvent',{type:'mouseMoved',x:rect.x+42,y:rect.y+28,button:'left',buttons:1});
 await send('Input.dispatchMouseEvent',{type:'mouseReleased',x:rect.x+42,y:rect.y+28,button:'left',clickCount:1});
 await until(async()=>{const n=(await snapshot()).nodes.find(n=>n.id===beforeEdit.id);return n.x!==beforeEdit.x && n.note===proofNote;},'human drag and agent note converge');
 const afterEdit=(await snapshot()).nodes.find(n=>n.id===beforeEdit.id);
 const readBack=await call('atlas_read');assert.ok(JSON.stringify(readBack).includes(proofNote));await save('agent-readback.json',readBack);
 await save('concurrent-edit.json',{before:beforeEdit,after:afterEdit});
 pass('Trusted browser drag and concurrent MCP note survive on the same object',{id:beforeEdit.id,delta:{x:afterEdit.x-beforeEdit.x,y:afterEdit.y-beforeEdit.y}});
 await screenshot('04-concurrent-edit');
 assert.ok(!(await call('atlas_draw',{nodes:[restore]})).isError);restore=null;
 const beforeRejection=await snapshot();
 const rejected=await call('atlas_draw',{nodes:'invalid'});assert.equal(rejected.isError,true);await save('rejected-action.json',rejected);
 const rejectedEvent=(await journal()).events.findLast(e=>e.kind==='action.atlas_draw'&&e.outcome==='failed');
 assert.ok(rejectedEvent?.timing);assert.ok(!rejectedEvent.timing.spans.some(s=>s.name==='execute_effect'));
 assert.deepEqual(rejectedEvent.stateRevisionBefore,rejectedEvent.stateRevisionAfter);
 assert.deepEqual((await snapshot()).nodes.map(n=>[n.id,n.x,n.y,n.note]),beforeRejection.nodes.map(n=>[n.id,n.x,n.y,n.note]));
 pass('Malformed action rejected before effect execution without changing shared nodes');
 const actionEvents=(await journal()).events.filter(e=>e.kind==='action.atlas_draw'&&e.outcome==='succeeded'&&e.timing);
 assert.ok(actionEvents.some(e=>e.stateRevisionBefore.some(r=>r.scope==='atlas/document') && JSON.stringify(e.stateRevisionBefore)!==JSON.stringify(e.stateRevisionAfter)));
 pass('Action evidence contains real Atlas revision change and measured phases');
 // A suspended action must retain its actor after a second MCP caller acts.
 await call('atlas_read');
 const firstActor=(await journal()).events.findLast(e=>e.kind==='action.atlas_read').actor;
 const firstSession=session;
 const suspended=call('await_input',{seconds:2});
 await delay(150);
 session=undefined;
 try {
   await rpc('initialize',{protocolVersion:'2025-06-18',capabilities:{},clientInfo:{name:'samepage-concurrent-proof',version:'1'}});
   await call('atlas_read');
   const secondEvent=(await journal()).events.findLast(e=>e.kind==='action.atlas_read');
   await suspended;
   const firstEvent=(await journal()).events.findLast(e=>e.kind==='action.await_input');
   assert.equal(firstEvent.actor.id,firstActor.id);
   assert.notEqual(secondEvent.actor.id,firstActor.id);
   assert.ok(secondEvent.timing.startedAtMs>=firstEvent.timing.startedAtMs);
   assert.ok(secondEvent.timing.startedAtMs<firstEvent.timing.startedAtMs+firstEvent.timing.durationUs/1000);
   await save('concurrent-actors.json',{firstEvent,secondEvent});
   pass('Overlapping MCP actions retain distinct resolved actors');
 } finally {
   if(session)await fetch(base+'/mcp',{method:'DELETE',headers:{Authorization:`Bearer ${token}`,'Mcp-Session-Id':session,'MCP-Protocol-Version':'2025-06-18'}});
   session=firstSession;
 }
 await click('#workspace-discuss');
 await call('chat_read',{seconds:1});
 const message=`Automated browser verification ${Date.now()}: please confirm receipt in this shared conversation.`;
 const waiting=call('chat_read',{seconds:10});
 await delay(150);
 await insert(message);
 const sendRect=await evaluate('(()=>{const r=document.querySelector("agui-conversation").shadowRoot.querySelector(".send").getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()');
 await send('Input.dispatchMouseEvent',{type:'mousePressed',...sendRect,button:'left',clickCount:1});await send('Input.dispatchMouseEvent',{type:'mouseReleased',...sendRect,button:'left',clickCount:1});
 const wake=await waiting;await save('conversation-pickup.json',{wake});
 assert.ok(JSON.stringify(wake).includes(message));
 const reply='Verified: the live MCP reader received this browser message. This is an automated interaction check.';
 assert.ok(!(await call('chat_reply',{text:reply})).isError);
 await until(()=>evaluate(`document.querySelector('agui-conversation').shadowRoot.textContent.includes(${JSON.stringify(reply)})`),'MCP reply reaches browser transcript');
 await screenshot('05-shared-discussion');pass('Browser message wakes real MCP reader and agent reply reaches transcript');
 await click('.dock-close');await click('#activity-toggle');await click('#activity-tab-host');
 await until(()=>evaluate('document.querySelectorAll(".flame-span").length>=2'),'host flame graph');await screenshot('06-host-flame');
 await click('.activity-filter');await insert('atlas_draw');
 assert.ok(await evaluate('document.querySelectorAll(".flame-span").length>=2'),'Filtering keeps measured child phases');
 await click('#activity-tab-wasm');
 assert.ok(await evaluate('document.querySelector(".activity-body").textContent.includes("No recorded traces match")'));
 await click('.activity-clear-filter');
 await until(()=>evaluate('document.querySelectorAll(".flame-span").length>=2'),'unfiltered WASM trace');
 await screenshot('07-wasm-flame');
 assert.ok(await evaluate('document.querySelectorAll(".flame-span").length>=2'));pass('Host phases and nested WASM spans render from recorded evidence');
 await click('.activity-controls button','Pause view');
 await click('.activity-controls button','Export trace');
 await save('trace.json',{host:await journal(),browser:await evaluate('__samePageAtlasTrace.snapshot()'),clocksCorrelated:false});
 await click('.activity-controls button','Close');
 await click('#presentation-toggle');assert.ok(await evaluate('document.body.classList.contains("presentation")'));assert.ok(await evaluate('[...document.querySelectorAll(".draw-tools")].every(e=>e.getBoundingClientRect().height===0)'));await screenshot('08-presentation');await key('Escape');assert.ok(!await evaluate('document.body.classList.contains("presentation")'));pass('Presentation mode exits with Escape');
 await send('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});await delay(350);
 await click('.exploration-edit');await screenshot('09-mobile-walkthrough');
 assert.ok(await evaluate('document.documentElement.scrollWidth<=innerWidth+1'),'No document horizontal overflow');
 assert.ok(await evaluate('(()=>{const a=document.querySelector(".agui-semantic-presence").getBoundingClientRect();const b=document.querySelector(".zoom-controls").getBoundingClientRect();return a.bottom<=b.top || b.bottom<=a.top || a.right<=b.left || b.right<=a.left;})()'),'Presence and zoom do not overlap');
 await click('#workspace-discuss');await screenshot('10-mobile-discussion');
 assert.ok(await evaluate('(()=>{const r=document.querySelector("agui-conversation").shadowRoot.querySelector(".send").getBoundingClientRect();return r.bottom<=innerHeight && r.width>0;})()'));
 await click('.dock-close');await click('#activity-toggle');await click('#activity-tab-host');await screenshot('11-mobile-activity');
 assert.ok(await evaluate('document.documentElement.scrollWidth<=innerWidth+1'));pass('Phone layout contains canvas, discussion composer, and activity');
 await click('.activity-controls button','Close');
 await send('Emulation.setDeviceMetricsOverride',{width:1440,height:900,deviceScaleFactor:1,mobile:false});
 const finalState=await snapshot();
 for(const kind of ['marks','claims','shapes'])assert.deepEqual(finalState[kind],initial[kind]);
 pass('Existing marks, claims, and drawings remain unchanged');
 await save('after.json',finalState);
 assert.deepEqual(errors,[]);pass('No uncaught browser exceptions');
} catch(error) {checks.push({name:'run completion',pass:false,error:error.stack});console.error(error.stack);await screenshot('failure').catch(()=>{});process.exitCode=1;}
finally {
 if(restore)await call('atlas_draw',{nodes:[restore]}).catch(()=>{});
 await save('receipt.json',{schema:'same-page-workspace-proof-v1',capturedAt:new Date().toISOString(),base,tabId,gitHead,gitDiffSha256,sourceHashes:'source-hashes.json',checks,errors,artifacts,realBrowser:true,realHost:true,humanAcceptance:false});
 if(session)await fetch(base+'/mcp',{method:'DELETE',headers:{Authorization:`Bearer ${token}`,'Mcp-Session-Id':session,'MCP-Protocol-Version':'2025-06-18'}}).catch(()=>{});
 socket.close();console.log('receipt',path.join(output,'receipt.json'));
}
