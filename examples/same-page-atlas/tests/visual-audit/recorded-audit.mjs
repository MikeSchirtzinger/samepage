import puppeteer from '/opt/homebrew/lib/node_modules/impeccable/node_modules/puppeteer/lib/puppeteer/puppeteer.js';
import { createBrowserDetector } from '/opt/homebrew/lib/node_modules/impeccable/cli/engine/detect-antipatterns.mjs';
import { writeFile } from 'node:fs/promises';
const browser = await puppeteer.connect({ browserURL:'http://127.0.0.1:9222', defaultViewport:null });
let detector;
try {
 detector = await createBrowserDetector({browser,waitUntil:'load',settleMs:1500});
 const results=[];
 for(const viewport of [{width:1440,height:1000},{width:390,height:844}]) {
  const findings = await detector.detectUrl('http://127.0.0.1:8174/',{viewport});
  results.push({url:'http://127.0.0.1:8174/',viewport,findings});
 }
 await writeFile('/Users/mike/dev/ag-ui-rust-wt-samepage/.local/architecture-polish/evidence/impeccable-rendered.json',JSON.stringify(results,null,2));
 console.log(JSON.stringify(results,null,2));
 process.exitCode=results.some(r=>r.findings.some(f=>f.severity!=='advisory'))?2:0;
} catch(error) {console.error(error);process.exitCode=1;}
finally {await detector?.close();browser.disconnect();}
