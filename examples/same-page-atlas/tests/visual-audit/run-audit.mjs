// Run against an existing real Chrome session. Only tabs created here are closed.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
const urls = [];
let output = '.local/source-view/audit-current';
let browserURL = 'http://127.0.0.1:9222';
let impeccableRoot = process.env.IMPECCABLE_ROOT;
let sourceNode;
let expectedNodeCounts;
let allowAuditStyleProbes = false;
for (let i = 0; i < args.length; i += 2) {
  const value = args[i + 1];
  if (!value) throw new Error(`Missing value for ${args[i]}`);
  switch (args[i]) {
    case '--url': urls.push(value); break;
    case '--output': output = value; break;
    case '--browser-url': browserURL = value; break;
    case '--impeccable-root': impeccableRoot = value; break;
    case '--source-node': sourceNode = value; break;
    case '--expected-nodes': expectedNodeCounts = value.split(',').map(Number); break;
    case '--allow-audit-style-probes':
      if (!['true', 'false'].includes(value)) throw new Error('--allow-audit-style-probes requires true or false.');
      allowAuditStyleProbes = value === 'true';
      break;
    default: throw new Error(`Unknown argument ${args[i]}`);
  }
}
if (!urls.length) throw new Error('At least one --url is required.');
if (expectedNodeCounts && (expectedNodeCounts.some(count => !Number.isInteger(count) || count < 1) || ![1, urls.length].includes(expectedNodeCounts.length))) {
  throw new Error('--expected-nodes requires a positive count, or one positive count per URL.');
}
if (!impeccableRoot) {
  impeccableRoot = path.join(execFileSync('npm', ['root', '-g'], { encoding: 'utf8' }).trim(), 'impeccable');
}
const detectorFile = path.join(impeccableRoot, 'cli/engine/detect-antipatterns.mjs');
const browserRulesFile = path.join(impeccableRoot, 'cli/engine/detect-antipatterns-browser.js');
const { default: puppeteer } = await import(pathToFileURL(path.join(impeccableRoot, 'node_modules/puppeteer/lib/puppeteer/puppeteer.js')));
const { createBrowserDetector } = await import(pathToFileURL(detectorFile));
await mkdir(path.dirname(path.resolve(output)), { recursive: true });
const browser = await puppeteer.connect({ browserURL, defaultViewport: null });
const originalNewPage = browser.newPage.bind(browser);
const before = (await browser.pages()).map(page => page.target()._targetId);
const owned = new Set();
const captures = [];
const results = [];
const captureFailures = [];
let capturePrefix;
let expectedNodes;
let detector;
try {
  browser.newPage = async () => {
    const page = await originalNewPage();
    owned.add(page);
    const events = [];
    page.on('console', message => {
      if (['error', 'warning'].includes(message.type())) events.push({ type: message.type(), message: message.text() });
    });
    page.on('pageerror', error => events.push({ type: 'pageerror', message: String(error) }));
    {
      const originalGoto = page.goto.bind(page);
      page.goto = async (...navigationArgs) => {
        let response = await originalGoto(...navigationArgs);
        const prepareAtlas = async () => {
        if (expectedNodes) {
          await page.waitForFunction(count => Boolean(window.atlas?.doc)
            && document.querySelector('.atlas-view')?.dataset.extensionState === 'ready'
            && document.querySelectorAll('.node').length === count
            && document.querySelector('#page-validation')?.dataset.status === 'passed', { timeout: 15000 }, expectedNodes);
        }
        if (sourceNode) {
          await page.waitForFunction(label => [...document.querySelectorAll('.node')].some(node => node.querySelector('.node-label')?.textContent === label), {}, sourceNode);
          const nodes = await page.$$('.node');
          const node = await (async () => {
            for (const candidate of nodes) {
              if (await candidate.$eval('.node-label', label => label.textContent) === sourceNode) return candidate;
            }
          })();
          if (!node) throw new Error(`Source node disappeared: ${sourceNode}`);
          const control = await node.$('.node-source');
          if (!control) throw new Error(`Source node has no code control: ${sourceNode}`);
          await control.click();
          await page.waitForFunction(() => document.querySelector('.source-code-text') && !document.querySelector('.source-notice')?.textContent.startsWith('Reading'));
        }
        };
        await prepareAtlas();
        await new Promise(resolve => setTimeout(resolve, 1500));
        // Capture real app health before the detector injects its inline styles.
        // Atlas correctly rejects those styles under its CSP and reports the
        // resulting resource errors. Keep post-scan errors as raw evidence too.
        const pristine = await page.evaluate(() => ({
          url: location.href,
          nodes: document.querySelectorAll('.node').length,
          pageStatus: document.querySelector('#page-validation')?.dataset.status,
          pageValidation: window.atlasValidation?.snapshot(),
          detectorPresent: typeof window.impeccableDetect === 'function',
        }));
        const pristineScreenshot = `${capturePrefix}-pristine.png`;
        const pristineDiagnostics = `${capturePrefix}-pristine.json`;
        await page.screenshot({ path: pristineScreenshot });
        await writeFile(pristineDiagnostics, JSON.stringify({ ...pristine, events: [...events] }, null, 2));
        captures.push({ phase: 'before-detector', screenshot: pristineScreenshot, diagnostics: pristineDiagnostics });
        if (expectedNodes && (pristine.pageStatus !== 'passed' || pristine.nodes !== expectedNodes || pristine.pageValidation?.errors?.length || events.some(event => event.type === 'pageerror' || event.type === 'error'))) {
          throw new Error(`Atlas was not healthy before detector injection: ${JSON.stringify(pristine)}`);
        }
        if (allowAuditStyleProbes) {
          if (events.some(event => event.type === 'pageerror' || event.type === 'error')) throw new Error('Pristine page emitted errors before detector injection.');
          // Scoped to this disposable page, after strict-CSP startup passed.
          // The detector injects styles for pixel contrast probes. This allows
          // those instruments to execute without changing the application CSP.
          await page.setBypassCSP(true);
          // Chromium applies this setting when creating a document. Reload
          // this page only, and repeat readiness and source-opening checks.
          response = await originalGoto(...navigationArgs);
          await prepareAtlas();
        }
        return response;
      };
    }
    const originalClose = page.close.bind(page);
    page.close = async () => {
      try {
        const diagnostics = await page.evaluate(() => ({
          url: location.href,
          title: document.title,
          nodes: document.querySelectorAll('.node').length,
          pageValidation: window.atlasValidation?.snapshot(),
          groups: window.impeccableDetect?.({ decorate: false, serialize: true }),
          hiddenText: window.impeccableMeasureHiddenText?.(),
        }));
        const screenshot = `${capturePrefix}.png`;
        const diagnosticFile = `${capturePrefix}-dom.json`;
        await page.screenshot({ path: screenshot });
        await writeFile(diagnosticFile, JSON.stringify({ ...diagnostics, events }, null, 2));
        captures.push({ phase: 'after-detector', screenshot, diagnostics: diagnosticFile });
        if (allowAuditStyleProbes && (diagnostics.pageValidation?.errors?.length || events.some(event => event.type === 'pageerror' || event.type === 'error'))) {
          throw new Error(`Detector page emitted errors: ${JSON.stringify(events)}`);
        }
      } catch (error) {
        captureFailures.push(String(error));
        throw error;
      } finally {
        await originalClose();
        owned.delete(page);
      }
    };
    return page;
  };
  detector = await createBrowserDetector({ browser, waitUntil: 'load', settleMs: 1500 });
  for (const [index, url] of urls.entries()) {
    expectedNodes = expectedNodeCounts?.[expectedNodeCounts.length === 1 ? 0 : index];
    for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }]) {
      capturePrefix = `${output}-${index + 1}-${viewport.width}`;
      const findings = await detector.detectUrl(url, { viewport });
      results.push({ url, viewport, findings });
      if (captureFailures.length) throw new Error(captureFailures.join('\n'));
      console.log(JSON.stringify({ url, viewport, failing: findings.filter(f => f.severity !== 'advisory').length, advisory: findings.filter(f => f.severity === 'advisory').length }));
    }
  }
  await writeFile(`${output}-raw.json`, JSON.stringify(results, null, 2));
  process.exitCode = results.some(result => result.findings.some(f => f.severity !== 'advisory')) ? 2 : 0;
} catch (error) {
  await writeFile(`${output}-failure.json`, JSON.stringify({ error: String(error), results }, null, 2));
  console.error(error);
  process.exitCode = 1;
} finally {
  // Detector closes its page even on navigation failure. This handles an
  // exception during setup without closing any pre-existing user tab.
  for (const page of owned) await page.close().catch(() => {});
  await detector?.close();
  const after = (await browser.pages()).map(page => page.target()._targetId);
  await writeFile(`${output}-receipt.json`, JSON.stringify({
    command: [process.execPath, ...process.argv.slice(1)],
    recordedAt: new Date().toISOString(),
    detectorFile,
    browserRulesSha256: createHash('sha256').update(await readFile(browserRulesFile)).digest('hex'),
    allowAuditStyleProbes,
    styleProbeScope: allowAuditStyleProbes ? 'Disposable detector page reload only, after strict-CSP pristine startup capture. Atlas readiness and source state checked again after reload.' : 'CSP remains enforced during detector probes.',
    before,
    after,
    existingTabsPreserved: before.every(id => after.includes(id)),
    ownedTabsRemaining: [...owned].map(page => page.target()._targetId),
    captures,
    exitCode: process.exitCode,
  }, null, 2));
  browser.disconnect();
}
