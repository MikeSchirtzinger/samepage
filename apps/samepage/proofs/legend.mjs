// Read-only audit of a real Chromium page. Does not activate capabilities or
// substitute for pointer/keyboard proof of the human actions that produced it.
import fs from 'node:fs';

const [endpoint, output] = process.argv.slice(2);
if (!endpoint || !output) {
  throw Error('usage: node apps/samepage/proofs/legend.mjs CDP_HTTP_URL OUTPUT_JSON');
}
const targets = await (await fetch(new URL('/json/list', endpoint))).json();
const target = targets.find(t => t.type === 'page' && /^http/.test(t.url));
if (!target) throw Error('No Chromium HTTP page target');
const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  socket.addEventListener('open', resolve, {once: true});
  socket.addEventListener('error', reject, {once: true});
});
try {
  const expression = `(async () => {
    // Let the shell finish the animation-frame refresh scheduled by the most
    // recent UI mutation before comparing the two paints.
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const root = document.querySelector('#legend-content')?.shadowRoot;
    if (!root || !globalThis.samepageReadback) throw Error('No composed SamePage legend');
    const paint = ['color','background-color','border-top-color','border-top-style',
      'border-top-width','border-left-color','border-left-style','border-left-width',
      'fill','stroke','stroke-width','stroke-dasharray','opacity'];
    const inViewport = n => {
      const r = n.getBoundingClientRect();
      let left = Math.max(0,r.left), top = Math.max(0,r.top);
      let right = Math.min(innerWidth,r.right), bottom = Math.min(innerHeight,r.bottom);
      for (let p = n.parentElement; p; p = p.parentElement) {
        const s = getComputedStyle(p), b = p.getBoundingClientRect();
        if (s.visibility === 'hidden' || s.display === 'none') return false;
        if (['auto','scroll','hidden','clip'].includes(s.overflowX)) {
          left = Math.max(left,b.left); right = Math.min(right,b.right);
        }
        if (['auto','scroll','hidden','clip'].includes(s.overflowY)) {
          top = Math.max(top,b.top); bottom = Math.min(bottom,b.bottom);
        }
        if (s.position === 'fixed') break;
      }
      return right >= left && bottom >= top;
    };
    const rows = [...root.querySelectorAll('.legend-key')].map(row => {
      const entry = {...row.dataset, label: row.querySelector('.legend-label')?.textContent,
        symbol: row.querySelector('.legend-symbol')?.textContent};
      if (entry.present !== 'true') return entry;
      const selector = entry.class.split(/\\s+/).map(c => '.' + CSS.escape(c)).join('');
      let candidates = [...document.querySelectorAll(selector)].filter(n => {
        const r = n.getBoundingClientRect(); return r.width > 0 || r.height > 0;
      });
      if (!['element','state_role'].includes(entry.attribute)) {
        candidates = candidates.filter(n =>
          n.getAttribute('data-' + entry.attribute.replace(/_/g,'-')) === entry.value ||
          (entry.attribute === 'tier' && n.classList.contains('source-tier-' + entry.value)));
      }
      const source = candidates[0], sample = row.querySelector('.legend-swatch')?.firstElementChild;
      const svg = source?.namespaceURI === 'http://www.w3.org/2000/svg';
      const drawn = svg ? sample?.firstElementChild : sample;
      if (!source || !drawn) return {...entry, error: 'Missing real source or swatch'};
      const originals = svg ? [source, ...source.querySelectorAll('*')] : [source];
      const clones = svg ? [drawn, ...drawn.querySelectorAll('*')] : [drawn];
      const pairs = originals.map((n, i) => {
        const a = getComputedStyle(n), b = clones[i] && getComputedStyle(clones[i]);
        return {tag: n.tagName, class: n.getAttribute('class'),
          paint: Object.fromEntries(paint.map(p => [p, {page: a.getPropertyValue(p),
            swatch: b?.getPropertyValue(p)}])),
          equal: !!b && paint.every(p => a.getPropertyValue(p) === b.getPropertyValue(p))};
      });
      return {...entry, selector, sourceId: source.dataset.id ||
        source.closest('[data-id]')?.dataset.id || source.closest('[data-pane-id]')?.dataset.paneId,
        sourceText: source.textContent.slice(0,160),
        sourceRect: source.getBoundingClientRect().toJSON(), sourceInViewport: inViewport(source),
        swatchRect: sample.getBoundingClientRect().toJSON(), swatchInViewport: inViewport(sample), pairs,
        equal: originals.length === clones.length && pairs.every(p => p.equal)};
    });
    return {url: location.href, readback: samepageReadback(), rows,
      sections: [...root.querySelectorAll('section')].map(n => n.dataset.capability),
      resources: performance.getEntriesByType('resource').map(r => r.name)};
  })()`;
  const result = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(Error('CDP audit timed out')), 20000);
    socket.addEventListener('message', event => {
      const message = JSON.parse(event.data);
      if (message.id !== 1) return;
      clearTimeout(timeout);
      if (message.error || message.result?.exceptionDetails) reject(Error(JSON.stringify(message)));
      else resolve(message.result.result.value);
    });
    socket.send(JSON.stringify({id: 1, method: 'Runtime.evaluate',
      params: {expression, returnByValue: true, awaitPromise: true}}));
  });
  fs.writeFileSync(output, JSON.stringify(result, null, 2) + '\n');
  const ready = new Set(result.readback.status.capabilities
    .filter(c => c.status === 'ready').map(c => c.id));
  const failures = result.rows.filter(r => !r.label || !r.symbol ||
    (r.present === 'true' && !r.equal) || !ready.has(r.capability));
  for (const id of result.sections) if (!ready.has(id)) failures.push({capability: id, error: 'Not ready'});
  if (failures.length) throw Error(JSON.stringify(failures));
  console.log('PASS: ' + result.rows.filter(r => r.present === 'true').length +
    ' present keys match real paint (including SVG children); ' + result.sections.length + ' ready sections');
} finally {
  socket.close();
}
