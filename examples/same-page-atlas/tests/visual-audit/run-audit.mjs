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

// A defect the built-in impeccable rules do not name: a text run that wraps
// to one word per line (its box is narrower than its own longest word), or
// that wraps to more lines than a short string should ever need. Both read
// as "a flex/grid child got squeezed past the point where its text still
// reads naturally" -- exactly what happened to the shared-context question
// once the mode strip and Legend button joined the same header row.
// Neither symptom trips scrollWidth > clientWidth (the text FITS its box,
// by wrapping), so the existing text-overflow rule never sees it; this is a
// distinct failure mode and gets its own check, run against a plain page
// (not the instrumented one `createBrowserDetector` manages) so it needs no
// coupling to impeccable's own internals.
async function checkWordWrapCollapse(url, viewport) {
  const page = await originalNewPage();
  try {
    await page.setViewport(viewport);
    await page.goto(url, { waitUntil: 'load' });
    await new Promise(resolve => setTimeout(resolve, 1500));
    return await page.evaluate(() => {
      const isRendered = (el) => {
        for (let cur = el; cur && cur.nodeType === 1; cur = cur.parentElement) {
          const s = getComputedStyle(cur);
          if (s.display === 'none' || s.visibility === 'hidden' || (parseFloat(s.opacity) || 0) <= 0.01) return false;
        }
        return true;
      };
      const classSelector = (el) => {
        const cls = typeof el.className === 'string' && el.className.trim() ? `.${el.className.trim().split(/\s+/).join('.')}` : '';
        return `${el.tagName.toLowerCase()}${cls}`;
      };
      const probe = document.createElement('span');
      probe.style.cssText = 'position:fixed;visibility:hidden;white-space:nowrap;top:-9999px;left:-9999px;pointer-events:none;';
      document.body.appendChild(probe);
      const longestWordWidth = (text, cs) => {
        probe.style.font = cs.font;
        probe.style.letterSpacing = cs.letterSpacing;
        let widest = 0;
        for (const word of text.split(/\s+/).filter(Boolean)) {
          probe.textContent = word;
          widest = Math.max(widest, probe.getBoundingClientRect().width);
        }
        return widest;
      };
      const findings = [];
      for (const el of document.querySelectorAll('*')) {
        const hasDirectText = [...el.childNodes].some(n => n.nodeType === 3 && n.textContent.trim().length > 0);
        if (!hasDirectText || !isRendered(el)) continue;
        const cs = getComputedStyle(el);
        // Only elements actually allowed to wrap: nowrap/pre already fail a
        // different way (overflow), which the existing text-overflow rule
        // covers.
        if (/^(nowrap|pre)$/.test(cs.whiteSpace)) continue;
        const text = el.textContent.trim();
        if (!text || !/\s/.test(text)) continue; // a single unbreakable run can't collapse per-word
        const rect = el.getBoundingClientRect();
        if (rect.width < 4 || rect.height < 4) continue;
        // overflow-wrap: anywhere/break-word means a long unbroken run (a
        // URL, a code line) is EXPECTED to split mid-token; the box being
        // narrower than that run's un-split width is then the design, not
        // the defect, so only judge against the longest word where the
        // element still needs a real word boundary to wrap at all.
        const canBreakAnywhere = /^(anywhere|break-word)$/.test(cs.overflowWrap) || cs.wordBreak === 'break-all';
        const widest = canBreakAnywhere ? 0 : longestWordWidth(text, cs);
        const narrowerThanLongestWord = !canBreakAnywhere && rect.width < widest - 1;
        const lineHeight = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.2;
        const lines = Math.round(rect.height / lineHeight);
        // The same escape hatch applies to the line-count heuristic: a code
        // snippet or URL under 60 characters can legitimately need more than
        // four lines once every break is mid-token; only short, ordinary
        // (space-breakable) prose collapsing to many lines is the defect.
        const tooManyLinesForShortText = !canBreakAnywhere && text.length < 60 && lines > 4;
        if (narrowerThanLongestWord || tooManyLinesForShortText) {
          findings.push({
            antipattern: 'word-wrap-collapse',
            name: 'Text wraps narrower than its own longest word, or into too many lines for its length',
            description: 'A text run\'s box is narrower than one of its own words, or a short string wraps across more than four lines. Usually a flex/grid child lost a min-width fight and is being squeezed by a sibling instead of the row reflowing. Give the text a sensible min-width, clamp and ellipsize past a line limit, or move a competing sibling to its own row.',
            severity: 'warning',
            category: 'quality',
            selector: classSelector(el),
            snippet: `"${text.slice(0, 60)}"${text.length > 60 ? '…' : ''} is ${Math.round(rect.width)}px wide, its longest word needs ${Math.round(widest)}px, and it wraps to ${lines} line(s)`,
          });
        }
      }
      probe.remove();
      return findings;
    });
  } finally {
    await page.close();
  }
}
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
      findings.push(...(await checkWordWrapCollapse(url, viewport)));
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
