// Native facts are data. The verified Sources WASM computes every trust tier
// and the source viewer read-back; this module owns browser-local presentation.
export async function activate({wasm, application}) {
  if (!wasm || !application) throw new Error('Sources needs its verified WASM and shared document');
  const { tierWords } = await import(new URL('/vocabulary.mjs', location.origin).href);
  application.registerCapability('sources', wasm);
  let disposed = false, latest = null, busy = false;
  const el = (tag, text, cls) => {
    const n = document.createElement(tag);
    if (text !== undefined) n.textContent = text;
    if (cls) n.className = cls;
    return n;
  };
  const envelope = raw => {
    const value = JSON.parse(raw);
    if (value.abi !== 'samepage.capability.v2' || value.error) throw new Error(value.error?.message || 'Sources ABI mismatch');
    return value.ok;
  };
  const fail = (e, action = 'read') => application.problem('sources', action, action === 'read' ? `Source facts could not be read: ${e.message}` : `Source could not be shown: ${e.message}`);
  const dispatch = async (name, args) => {
    const response = await application.dispatch(name, args);
    return JSON.parse(response.result).sources;
  };
  async function openViewer(id) {
    const view = application.view();
    const node = view.records.find(r => r.id === id);
    const bindings = latest?.cards[id]?.bindings || [];
    // The viewer lives in the Sources tab beside the map, never over it.
    const mount = document.getElementById('sources');
    const panel = el('section', undefined, 'source-viewer'); panel.id = 'source-viewer';
    panel.setAttribute('aria-label', `Code for ${node?.props.label || 'project'}`);
    const title = el('h2', node?.props.label || 'Project sources');
    const header = el('header'); header.append(title);
    // The card's trust tier travels into the viewer, in words.
    const tier = latest?.cards[id];
    if (tier) { const chip = el('span', tierWords(tier), `source-tier source-tier-${tier.tier}`); chip.title = tier.gate?.reason || `Source tier: ${tier.tier}`; header.append(chip); }
    const context = el('p', 'Current working tree · read from the host. Viewing never edits the map.');
    const select = el('select'); select.setAttribute('aria-label', 'Source file');
    for (const b of bindings) { const o = el('option', `${b.path}:${b.lines}`); o.value = JSON.stringify(b); select.append(o); }
    const location = el('p', '', 'source-location');
    const notice = el('p', 'Reading source…', 'source-notice'); notice.setAttribute('role', 'status');
    const pre = el('pre', undefined, 'source-code'); pre.tabIndex = 0;
    const browse = el('details'); browse.append(el('summary', 'Browse files'));
    const listing = el('div'); browse.append(listing);
    const pages = el('footer');
    const previous = el('button', 'Previous lines'), whole = el('button', 'From the start'), next = el('button', 'Next lines');
    for (const button of [previous, whole, next]) { button.type = 'button'; button.disabled = true; pages.append(button); }
    panel.append(header, context, select, browse, location, notice, pre, pages);
    let generation = 0, current = null;
    async function load(b) {
      const own = ++generation; notice.textContent = 'Reading source…'; notice.classList.remove('source-error');
      for (const button of [previous, whole, next]) button.disabled = true;
      try {
        let excerpt = await dispatch('source_read', b);
        // Show the bound lines with eight lines of context on each side.
        const bound = /^(\d+)(?:-(\d+))?$/.exec(String(b.lines || ''));
        const focus = bound ? {from: Number(bound[1]), to: Number(bound[2] || bound[1])} : null;
        if (focus && excerpt.total_lines) {
          const from = Math.max(1, focus.from - 8), to = Math.min(excerpt.total_lines, focus.to + 8);
          if (from < focus.from || to > focus.to) { try { excerpt = await dispatch('source_read', {...b, lines: `${from}-${to}`}); } catch { /* the bound range alone is still shown */ } }
        }
        if (own !== generation || !panel.isConnected) return;
        const projection = envelope(wasm.viewer_readback(JSON.stringify(excerpt)));
        for (const path of excerpt.entry_points || []) {
          if ([...select.options].some(o => JSON.parse(o.value).path === path)) continue;
          const option = el('option', `Entry point: ${path}`); option.value = JSON.stringify({path}); select.append(option); select.hidden = false;
        }
        current = excerpt;
        location.textContent = focus ? `${excerpt.path} · lines ${focus.from}–${focus.to} of ${excerpt.total_lines}, with context` : `${excerpt.path} · ${excerpt.total_lines} lines`;
        context.textContent = excerpt.revision ? `Git revision ${excerpt.revision} · read from the host.` : 'Current working tree · read from the host. Viewing never edits the map.';
        pre.replaceChildren();
        for (const line of projection.lines) {
          const row = el('span', undefined, 'source-line'); row.dataset.line = String(line.number);
          row.classList.toggle('source-highlight', focus ? line.number >= focus.from && line.number <= focus.to : line.highlighted);
          row.append(el('span', String(line.number), 'source-line-number'), el('code', line.text)); pre.append(row);
        }
        notice.textContent = `${excerpt.truncated ? 'More lines available. ' : ''}${focus ? 'Bound lines highlighted' : 'Shown from the host'} · read just now.`;
        if (excerpt.entry_points_error) notice.textContent += ` Entry points unavailable: ${excerpt.entry_points_error}`;
        previous.disabled = whole.disabled = excerpt.first_line <= 1;
        next.disabled = excerpt.first_line + projection.lines.length - 1 >= excerpt.total_lines;
      } catch (e) { if (own === generation) { notice.textContent = `Could not read source: ${e.message}`; notice.classList.add('source-error'); } }
    }
    async function list(path = '') {
      listing.textContent = 'Reading files…';
      try {
        const data = await dispatch('source_list', {path});
        listing.replaceChildren();
        if (path) { const up = el('button', 'Parent folder'); up.type = 'button'; up.addEventListener('click', () => list(path.split('/').slice(0,-1).join('/'))); listing.append(up); }
        for (const entry of data.entries) {
          const button = el('button', `${entry.name}${entry.is_dir ? '/' : ''}`); button.type = 'button';
          const next = path ? `${path}/${entry.name}` : entry.name;
          button.addEventListener('click', () => entry.is_dir ? list(next) : load({path:next})); listing.append(button);
        }
        if (data.truncated) listing.append(el('p', 'Directory listing limited to 240 entries.'));
      } catch (e) { listing.textContent = `Could not list files: ${e.message}`; }
    }
    select.addEventListener('change', () => load(JSON.parse(select.value)));
    previous.addEventListener('click', () => load({path:current.path, revision:current.revision, lines:`${Math.max(1,current.first_line-200)}-${current.first_line-1}`}));
    whole.addEventListener('click', () => load({path:current.path, revision:current.revision}));
    next.addEventListener('click', () => { const end = current.first_line + current.text.split('\n').filter((_,i,a)=>i<a.length-1||a[i] !== '').length-1; return load({path:current.path, revision:current.revision, lines:`${end+1}-${Math.min(current.total_lines,end+200)}`}); });
    browse.addEventListener('toggle', () => { if (browse.open) void list(); });
    mount.replaceChildren(panel);
    application.openPanel?.('sources');
    if (bindings.length) await load(bindings[0]); else { notice.textContent = 'No source linked. Browse project files.'; select.hidden = true; browse.open = true; }
  }
  async function render({view}) {
    if (disposed || busy || !application.ready()) return;
    busy = true;
    try {
      const data = await dispatch('sources_read', {});
      if (data.revision !== view.revision) return;
      latest = envelope(wasm.project_with_facts(JSON.stringify(view), JSON.stringify(data.facts)));
      for (const card of document.querySelectorAll('.map-card')) {
        card.querySelectorAll('.source-tier,.source-view-open,.source-gate-reason').forEach(n => n.remove());
        const tier = latest.cards[card.dataset.id];
        if (!tier) continue;
        const chip = el('span', tierWords(tier), `source-tier source-tier-${tier.tier}`);
        chip.title = `Source trust: ${tierWords(tier)}`;
        card.dataset.sourceTier = tier.tier; card.append(chip);
        if (tier.gate) card.append(el('p', tier.gate.reason, 'source-gate-reason'));
        if (tier.bindings.length) {
          const open = el('button', 'View source', 'source-view-open'); open.type = 'button';
          open.addEventListener('pointerdown', e => e.stopPropagation());
          open.addEventListener('click', () => openViewer(card.dataset.id).then(() => application.resolved('sources', 'view')).catch(e => fail(e, 'view'))); card.append(open);
        }
      }
      application.resolved('sources', 'read');
    } catch (e) { fail(e); } finally { busy = false; }
  }
  // Opening the tab by itself keeps a viewer that is already showing.
  const open = e => { if (!e.detail?.id && document.querySelector('#sources .source-viewer')) return; openViewer(e.detail?.id || '').catch(error => fail(error, 'view')); };
  document.addEventListener('samepage:sources-open', open);
  const unregister = application.registerRenderer('sources', {render});
  const timer = setInterval(() => void render({view:application.view()}), 3000);
  return {dispose() { disposed = true; clearInterval(timer); unregister(); document.removeEventListener('samepage:sources-open', open); }};
}
