// A minimal, single-file visual audit: only impeccable's own built-in rules,
// against one URL, with no CLI arguments. Useful for a quick manual check
// against a running atlas; run-audit.mjs is the full portable runner (also
// checks the word-wrap-collapse rule and accepts multiple URLs and viewports
// on the command line), and is what CI and the repository gates use.
//
// Requires impeccable installed and resolvable from IMPECCABLE_ROOT, or
// globally via `npm install -g impeccable` (its own bundled puppeteer comes
// along with it). AUDIT_URL defaults to this app's own default port; a real
// Chrome must already be listening on --remote-debugging-port=9222 (see the
// Browser skill's `browser-start`, or `chrome --remote-debugging-port=9222`).
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { mkdir, writeFile } from 'node:fs/promises';

const impeccableRoot =
  process.env.IMPECCABLE_ROOT ??
  path.join(execFileSync('npm', ['root', '-g'], { encoding: 'utf8' }).trim(), 'impeccable');
const auditUrl = process.env.AUDIT_URL ?? 'http://127.0.0.1:8098/';
const browserUrl = process.env.BROWSER_URL ?? 'http://127.0.0.1:9222';
const outputPath = process.env.AUDIT_OUTPUT ?? '.local/visual-audit/recorded-audit.json';

const { default: puppeteer } = await import(
  pathToFileURL(path.join(impeccableRoot, 'node_modules/puppeteer/lib/puppeteer/puppeteer.js'))
);
const { createBrowserDetector } = await import(
  pathToFileURL(path.join(impeccableRoot, 'cli/engine/detect-antipatterns.mjs'))
);

const browser = await puppeteer.connect({ browserURL: browserUrl, defaultViewport: null });
let detector;
try {
  detector = await createBrowserDetector({ browser, waitUntil: 'load', settleMs: 1500 });
  const results = [];
  for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }]) {
    const findings = await detector.detectUrl(auditUrl, { viewport });
    results.push({ url: auditUrl, viewport, findings });
  }
  await mkdir(path.dirname(path.resolve(outputPath)), { recursive: true });
  await writeFile(outputPath, JSON.stringify(results, null, 2));
  console.log(JSON.stringify(results, null, 2));
  process.exitCode = results.some(r => r.findings.some(f => f.severity !== 'advisory')) ? 2 : 0;
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  await detector?.close();
  browser.disconnect();
}
