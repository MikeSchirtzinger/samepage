import {execFileSync} from 'node:child_process';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
const args=process.argv.slice(2),options={};
for(let i=0;i<args.length;i+=2){if(!args[i+1])throw new Error(`Missing value for ${args[i]}`);options[args[i]]=args[i+1];}
const url=options['--url'];
if(!url)throw new Error('--url is required');
const root=options['--repo-root']||process.cwd();
const output=options['--output']||'.local/source-view/audit-source';
const impeccable=options['--impeccable-root']||process.env.IMPECCABLE_ROOT||path.join(execFileSync('npm',['root','-g'],{encoding:'utf8'}).trim(),'impeccable');
const {default:puppeteer}=await import(pathToFileURL(path.join(impeccable,'node_modules/puppeteer/lib/puppeteer/puppeteer.js')));
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import assert from 'node:assert/strict';
const browser=await puppeteer.connect({browserURL:options['--browser-url']||'http://127.0.0.1:9222',defaultViewport:null});
await mkdir(path.dirname(path.resolve(output)),{recursive:true});
const beforeTabs=(await browser.pages()).map(p=>p.target()._targetId);
const context=await browser.createBrowserContext();
const evidence={cases:[],screenshots:[],events:[]};
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const capture=async(page,name)=>{const file=`${output}-${name}.png`;await page.screenshot({path:file});evidence.screenshots.push(file);};
const state=page=>page.evaluate(()=>({camera:{...window.atlas.camera},snapshot:window.atlas.doc.snapshot(),scroll:{viewX:document.querySelector('.atlas-view').scrollLeft,viewY:document.querySelector('.atlas-view').scrollTop,pageX:scrollX,pageY:scrollY}}));
const close=async(page,key)=>{if(key)await page.keyboard.press('Escape');else await page.click('[aria-label="Close code viewer"]');await page.waitForFunction(()=>!document.querySelector('.source-viewer'));};
try{
for(const width of [1440,390]){
 const page=await context.newPage(),responses=[],pending=[];
 page.on('console',m=>{if(['error','warning'].includes(m.type()))evidence.events.push({width,type:m.type(),message:m.text()})});
 page.on('pageerror',e=>evidence.events.push({width,type:'pageerror',message:String(e)}));
 page.on('response',r=>{if(new URL(r.url()).pathname==='/atlas/source')pending.push(r.json().then(payload=>responses.push(payload)));});
 await page.setViewport({width,height:width===1440?1000:844});
 await page.goto(url,{waitUntil:'load'});
 await page.waitForFunction(()=>window.atlas?.atlas.nodes.length===5 && document.querySelector('#page-validation')?.dataset.status==='passed');await sleep(1600);
 const nodes=await page.evaluate(()=>[...document.querySelectorAll('.node')].map(n=>({id:n.dataset.id,label:n.querySelector('.node-label').textContent})));
 for(const [index,node] of nodes.entries()){
  const before=await state(page);
  await page.click(`.node[data-id="${node.id}"] .node-source`);
  await page.waitForFunction(()=>document.querySelector('.source-code-text')&&!document.querySelector('.source-notice').textContent.startsWith('Reading'));
  await Promise.all(pending);
  const rendered=await page.evaluate(()=>({title:document.querySelector('.source-viewer h2').textContent,location:document.querySelector('.source-location').textContent,code:document.querySelector('.source-code-text').textContent,numbers:document.querySelector('.source-line-numbers').textContent.split('\n').map(Number),options:[...document.querySelector('[aria-label="Source file"]').options].map(o=>o.textContent)}));
  assert.equal(rendered.title,node.label);
  const payload=responses.findLast(r=>r.ok&&rendered.location===`${r.path}:${r.lines} · ${r.total_lines} lines`);assert.ok(payload,'render matches received source route');
  assert.match(payload.path,/\.rs$/,'package defaults to actual Rust entry file');
  const disk=(await readFile(path.resolve(root,payload.path),'utf8')).split(/\r?\n/).slice(payload.first_line-1,Number(payload.lines.split('-').at(-1))).join('\n');
  assert.equal(payload.text,disk,'host source equals current filesystem range');
  assert.equal(rendered.code,payload.text,'rendered source equals host bytes exactly');
  assert.deepEqual(rendered.numbers,payload.text.split('\n').map((_,i)=>payload.first_line+i));
  const after=await state(page);assert.deepEqual(after.camera,before.camera,'code viewer must preserve camera');assert.equal(after.snapshot,before.snapshot,'code viewer must preserve shared state');
  await capture(page,`${width}-${index+1}-${node.label}`);
  await close(page,index%2===0);
  const focus=await page.evaluate(()=>({tag:document.activeElement.tagName,class:document.activeElement.className,id:document.activeElement.closest('.node')?.dataset.id}));
  assert.equal(focus.id,node.id,'closing returns focus to initiating card control');
  evidence.cases.push({width,node:node.label,path:payload.path,lines:payload.lines,sourceBytesMatch:true,cameraUnchanged:true,semanticStateUnchanged:true,closeFocus:focus});
 }
 // Reopen the host entry point to exercise paging and a directory traversal.
 const host=nodes.find(n=>n.label==='same-page-atlas');
 await page.click(`.node[data-id="${host.id}"] .node-source`);await page.waitForSelector('.source-code-text');
 const nextLine=await page.$eval('.source-line-numbers',n=>Number(n.textContent.split('\n').at(-1))+1);
 await page.click('.source-viewer-pages button:last-child');
 await page.waitForFunction(expected=>Number(document.querySelector('.source-line-numbers')?.textContent.split('\n')[0])===expected,{},nextLine);
 await capture(page,`${width}-next-lines`);
 await page.click('.source-viewer-pages button:first-child');
 await page.waitForFunction(expected=>Number(document.querySelector('.source-line-numbers')?.textContent.split('\n')[0])===expected,{},Math.max(1,nextLine-200));
 await page.click('.source-viewer-pages button:nth-child(2)');
 await page.waitForFunction(()=>document.querySelector('.source-line-numbers')?.textContent.split('\n')[0]==='1');
 await page.click('.source-viewer-files summary');await page.waitForSelector('.source-file');
 await page.click('.source-directory button');
 await page.waitForFunction(()=>[...document.querySelectorAll('.source-file')].some(b=>b.textContent==='Cargo.toml'));
 const manifest=await page.$$('.source-file');for(const button of manifest){if(await button.evaluate(e=>e.textContent)==='Cargo.toml'){await button.click();break;}}
 await page.waitForFunction(()=>document.querySelector('.source-location')?.textContent.startsWith('examples/same-page-atlas/Cargo.toml:'));
 await capture(page,`${width}-browsed-manifest`);
 await close(page,true);
 evidence.cases.push({width,pagingNextPrevious:true,browseParentAndManifest:true});
 // Theme clicks are isolated to this incognito context, preserving user preferences.
 for(const theme of ['paper','sand','contrast','slate']){
  await page.click('.workspace-file-menu > summary');
  await page.click(`[data-theme-choice="${theme}"]`);
  await page.click('.workspace-file-menu > summary');
  await page.click(`.node[data-id="${host.id}"] .node-source`);await page.waitForSelector('.source-code-text');
  assert.equal(await page.evaluate(()=>document.documentElement.dataset.theme),theme);
  await capture(page,`${width}-theme-${theme}`);
  await close(page,true);
 }
 await page.close();
}
assert.equal(evidence.events.length,0,'browser console must contain no errors or warnings');
evidence.pass=true;
}catch(error){evidence.pass=false;evidence.error=String(error);console.error(error);process.exitCode=1;}
finally{await context.close();evidence.existingTabsPreserved=beforeTabs.every(id=>(browser.targets().some(t=>t._targetId===id)));await writeFile(`${output}-qa.json`,JSON.stringify(evidence,null,2));browser.disconnect();console.log(JSON.stringify({pass:evidence.pass,cases:evidence.cases.length,error:evidence.error}));}
