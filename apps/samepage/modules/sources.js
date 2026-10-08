// Native facts are data. The verified Sources WASM computes every trust tier
// and the source viewer read-back; this module owns browser-local presentation.
export function activate({wasm, application}) {
  if (!wasm || !application) throw new Error('Sources needs its verified WASM and shared document');
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
  const fail = e => {
    const n = document.getElementById('error-banner');
    n.textContent = `Sources unavailable: ${e.message}`; n.hidden = false;
  };
  const dispatch = async (name, args) => {
    const response = await application.dispatch(name, args);
    return JSON.parse(response.result).sources;
  };
  async function openViewer(id) {
    const view = application.view();
    const node = view.records.find(r => r.id === id);
    const bindings = latest?.cards[id]?.bindings || [];
    document.getElementById('source-viewer')?.close();
    const origin = document.activeElement;
    const panel = el('dialog', undefined, 'source-viewer'); panel.id = 'source-viewer';
    panel.setAttribute('aria-label', `Code for ${node?.props.label || 'project'}`);
    const title = el('h2', node?.props.label || 'Project sources');
    const close = el('button', 'Close'); close.type = 'button'; close.className = 'source-close';
    close.addEventListener('click', () => panel.close());
    const header = el('header'); header.append(title, close);
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
        const excerpt = await dispatch('source_read', b);
        if (own !== generation || !panel.open) return;
        const projection = envelope(wasm.viewer_readback(JSON.stringify(excerpt)));
        current = excerpt;
        location.textContent = `${projection.location} · ${excerpt.total_lines} lines`;
        context.textContent = excerpt.revision ? `Git revision ${excerpt.revision} · read from the host.` : 'Current working tree · read from the host. Viewing never edits the map.';
        pre.replaceChildren();
        for (const line of projection.lines) {
          const row = el('span', undefined, 'source-line'); row.dataset.line = String(line.number);
          row.classList.toggle('source-highlight', line.highlighted);
          row.append(el('span', String(line.number), 'source-line-number'), el('code', line.text)); pre.append(row);
        }
        notice.textContent = `${excerpt.truncated ? 'More lines available. ' : ''}Shown lines highlighted · read from the host just now.`;
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
    panel.addEventListener('close', () => { generation++; panel.remove(); if (origin?.isConnected) origin.focus(); }, {once:true});
    document.body.append(panel); panel.showModal(); close.focus();
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
        card.querySelectorAll('.source-tier,.source-view-open').forEach(n => n.remove());
        const tier = latest.cards[card.dataset.id];
        if (!tier || (!tier.bindings.length && tier.tier === 'claimed')) continue;
        const chip = el('span', `${tier.symbol} ${tier.chip}`, `source-tier source-tier-${tier.tier}`);
        card.dataset.sourceTier = tier.tier; card.append(chip);
        if (tier.bindings.length) {
          const open = el('button', 'View source', 'source-view-open'); open.type = 'button';
          open.addEventListener('pointerdown', e => e.stopPropagation());
          open.addEventListener('click', () => openViewer(card.dataset.id).catch(fail)); card.append(open);
        }
      }
    } catch (e) { fail(e); } finally { busy = false; }
  }
  const open = e => openViewer(e.detail?.id || '').catch(fail);
  document.addEventListener('samepage:sources-open', open);
  const unregister = application.registerRenderer('sources', {render});
  const timer = setInterval(() => void render({view:application.view()}), 3000);
  return {dispose() { disposed = true; clearInterval(timer); unregister(); document.removeEventListener('samepage:sources-open', open); }};
}
