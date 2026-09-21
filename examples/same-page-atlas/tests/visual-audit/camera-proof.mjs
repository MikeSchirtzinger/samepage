import {execFileSync} from 'node:child_process';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
const args=process.argv.slice(2),options={};
for(let i=0;i<args.length;i+=2){if(!args[i+1])throw new Error(`Missing value for ${args[i]}`);options[args[i]]=args[i+1];}
const url=options['--url'];
if(!url)throw new Error('--url is required');
const output=options['--output']||'.local/source-view/audit-camera';
const impeccable=options['--impeccable-root']||process.env.IMPECCABLE_ROOT||path.join(execFileSync('npm',['root','-g'],{encoding:'utf8'}).trim(),'impeccable');
const {default:p}=await import(pathToFileURL(path.join(impeccable,'node_modules/puppeteer/lib/puppeteer/puppeteer.js')));
import{writeFile,mkdir}from'node:fs/promises';import assert from'node:assert/strict';
await mkdir(path.dirname(path.resolve(output)),{recursive:true});
const b=await p.connect({browserURL:options['--browser-url']||'http://127.0.0.1:9222',defaultViewport:null}),context=await b.createBrowserContext(),page=await context.newPage(),out={checks:[],events:[]};
const wait=ms=>new Promise(r=>setTimeout(r,ms));
const state=()=>page.evaluate(()=>({camera:{...window.atlas.camera},snapshot:window.atlas.doc.snapshot(),scroll:{viewX:document.querySelector('.atlas-view').scrollLeft,viewY:document.querySelector('.atlas-view').scrollTop,pageX:scrollX,pageY:scrollY}}));
try{
page.on('console',m=>{if(['error','warning'].includes(m.type()))out.events.push({type:m.type(),text:m.text()})});
await page.setViewport({width:1440,height:1000});await page.goto(url,{waitUntil:'load'});await page.waitForFunction(()=>window.atlas?.atlas.nodes.length>0&&document.querySelector('#page-validation')?.dataset.status==='passed');await wait(1800);
await page.evaluate(()=>{window.auditTrusted=[];for(const type of ['wheel','pointerdown','keydown'])document.addEventListener(type,e=>window.auditTrusted.push({type,trusted:e.isTrusted,key:e.key}),true)});
const original=await state();
await page.mouse.move(750,320);await page.keyboard.down('Control');await page.mouse.wheel({deltaY:-30});await page.keyboard.up('Control');await wait(500);
const zoomed=await state();assert.ok(zoomed.camera.scale>original.camera.scale,'trusted wheel zoom changes camera scale');assert.equal(zoomed.snapshot,original.snapshot,'wheel leaves shared document unchanged');out.checks.push({check:'wheel',before:original.camera,after:zoomed.camera});
await page.mouse.click(50,210);await page.keyboard.down('Space');await page.mouse.move(50,210);await page.mouse.down();await page.mouse.move(560,40,{steps:8});await page.mouse.up();await page.keyboard.up('Space');await wait(400);
const panned=await state();assert.ok(Math.abs(panned.camera.x-zoomed.camera.x)>100,'trusted space-drag pans');assert.equal(panned.snapshot,original.snapshot,'pan leaves shared document unchanged');
const boundary=await page.evaluate(()=>{const v=document.querySelector('.atlas-view'),r=v.getBoundingClientRect(),checks=[];for(const n of document.querySelectorAll('.node')){const q=n.getBoundingClientRect();if(q.top<r.top&&q.bottom>0&&q.right>0&&q.left<innerWidth){const x=Math.max(1,Math.min(innerWidth-1,(q.left+q.right)/2)),y=Math.max(1,Math.min(r.top-2,(q.top+Math.min(q.bottom,r.top))/2));checks.push({node:n.dataset.id,x,y,painted:document.elementsFromPoint(x,y).some(e=>e===n||n.contains(e))});}}return{contain:getComputedStyle(v).contain,overflow:getComputedStyle(v).overflow,checks};});
assert.ok(boundary.checks.length>0,'pan put real nodes behind top canvas boundary');assert.ok(boundary.checks.every(c=>!c.painted),'off-canvas nodes cannot paint or receive hit tests over header');out.checks.push({check:'pan-boundary',...boundary});
await page.screenshot({path:`${output}-clipped.png`});
// Fit is a trusted control. Opening and discarding an inline label must not
// turn camera containment into a scrolling ancestor or create a semantic edit.
await page.click('.zoom-controls button:last-child');await wait(400);
const labels=await page.$$('.node .node-label');let rightmost;let rx=-Infinity;for(const label of labels){const box=await label.boundingBox();if(box&&box.x>rx&&box.x+box.width<1438){rightmost=label;rx=box.x;}}
assert.ok(rightmost);await rightmost.click();const beforeFocus=await state();await page.keyboard.press('Enter');await page.waitForSelector('.inline-direct-editor');
const focused=await state();assert.deepEqual(focused.scroll,beforeFocus.scroll,'inline focus leaves all scroll offsets unchanged');assert.deepEqual(focused.camera,beforeFocus.camera,'inline focus preserves camera');
await page.screenshot({path:`${output}-inline-focus.png`});await page.keyboard.press('Escape');await wait(100);const afterFocus=await state();assert.equal(afterFocus.snapshot,beforeFocus.snapshot,'discarded inline editor creates no semantic edit');
out.checks.push({check:'inline-focus',before:beforeFocus.scroll,after:focused.scroll,cameraUnchanged:true,snapshotUnchanged:true});
out.trusted=await page.evaluate(()=>window.auditTrusted);assert.ok(out.trusted.length>0&&out.trusted.every(e=>e.trusted),'all recorded interaction input was trusted');assert.equal(out.events.length,0);out.pass=true;
}catch(error){out.pass=false;out.error=String(error);console.error(error);await page.screenshot({path:`${output}-failure.png`});process.exitCode=1;}
finally{await writeFile(`${output}-proof.json`,JSON.stringify(out,null,2));await context.close();b.disconnect();console.log(JSON.stringify(out));}
