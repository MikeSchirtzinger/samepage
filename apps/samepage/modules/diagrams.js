// Diagrams decorates the Map renderer from the separately verified WASM
// projection. It shares the shell's document and never authors replica state.
export function activate({wasm, application}) {
  const mount = document.getElementById("map");
  if (!mount) throw new Error("Diagrams needs the Map mount");
  if (!wasm || !application) throw new Error('Diagrams needs its verified WASM and shared document');
  application.registerCapability('diagrams', wasm);
  const style = document.createElement('style');
  style.textContent = `
    .map-card.diagram-container { background: #13232e66; border: 1px dashed #58788c; box-shadow: none; border-radius: 16px; z-index: 0; }
    .diagram-container > h2 { color: #9ab6c9; font-size: 13px; letter-spacing: .07em; text-transform: uppercase; }
    .diagram-container > .byline, .diagram-container > .map-card-meta { display: none; }
    .map-card:not(.diagram-container) { z-index: 2; }
    .diagram-edges { position: absolute; inset: 0; overflow: visible; pointer-events: none; z-index: 1; }
    .diagram-transition { stroke: #7fb5d5; stroke-width: 2; fill: none; stroke-linejoin: round; }
    .diagram-event { fill: #e3eff9; font: 12px system-ui, sans-serif; paint-order: stroke; stroke: #101b24; stroke-width: 6px; stroke-linejoin: round; }
    .diagram-roles { display:flex; gap:8px; margin-bottom: 8px; color:#9ae6c1; font-size:11px; letter-spacing:.08em; font-weight:600; }
    .map-card.diagram-terminal { border: 3px double #9ae6c1; }
    .map-card.diagram-initial { border-color:#9ae6c1; }
    .diagram-legend { position:sticky; left:20px; bottom:12px; width:max-content; max-width:90%; padding:9px 13px; background:#13232ef5; border:1px solid #476175; border-radius:8px; color:#bbcedb; font-size:12px; z-index:4; }
    .diagram-problems { margin: 0; color:#ffd49d; white-space:pre-wrap; }
  `;
  document.head.appendChild(style);
  const ns = 'http://www.w3.org/2000/svg';
  const svgNode = (tag, attrs = {}) => {
    const node = document.createElementNS(ns, tag);
    for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, String(value));
    return node;
  };
  let disposed = false;
  async function render({view, revision}) {
    let projection = await application.project('diagrams', view);
    if (disposed) return;
    const stage = mount.querySelector('.map-stage');
    if (!stage) throw new Error('Map stage is absent');
    stage.querySelectorAll('.diagram-edges, .diagram-roles').forEach(node => node.remove());
    const byId = new Map([...stage.querySelectorAll('.map-card')].map(node => [node.dataset.id, node]));
    for (const container of projection.containers) {
      const node = byId.get(container.id);
      if (node) {node.classList.add('diagram-container'); node.dataset.diagramKind = container.kind;}
    }
    for (const state of projection.states) {
      const node = byId.get(state.id);
      if (!node) throw new Error(`Missing state ${state.id}`);
      node.dataset.diagramState = 'true';
      if (state.initial) node.classList.add('diagram-initial');
      if (state.terminal) node.classList.add('diagram-terminal');
      const roles = document.createElement('div');
      roles.className = 'diagram-roles';
      roles.textContent = [state.initial ? '● INITIAL' : '', state.terminal ? '◎ TERMINAL' : ''].filter(Boolean).join('   ');
      if (roles.textContent) node.prepend(roles);
    }
    const x = Number(stage.dataset.originX), y = Number(stage.dataset.originY);
    const stageBox = stage.getBoundingClientRect();
    const bounds = Object.fromEntries([...byId].map(([id,node]) => {
      const box = node.getBoundingClientRect();
      return [id,[box.left-stageBox.left-x, box.top-stageBox.top-y, box.right-stageBox.left-x, box.bottom-stageBox.top-y]];
    }));
    const routed = JSON.parse(wasm.project_with_bounds(JSON.stringify(view), JSON.stringify(bounds)));
    if (routed.abi !== 'samepage.capability.v2' || routed.error || !routed.ok) throw new Error(routed.error?.message || 'Diagrams could not route measured card bounds');
    projection = routed.ok;
    const svg = svgNode('svg', {width: stage.style.width, height: stage.style.height, role: 'img', 'aria-label': 'State machine transitions'});
    svg.classList.add('diagram-edges');
    const defs = svgNode('defs');
    const marker = svgNode('marker', {id:'diagram-arrow', viewBox:'0 0 10 10', refX:9, refY:5, markerWidth:7, markerHeight:7, orient:'auto-start-reverse'});
    marker.appendChild(svgNode('path', {d:'M 0 0 L 10 5 L 0 10 z', fill:'#7fb5d5'}));
    defs.appendChild(marker); svg.appendChild(defs);
    for (const edge of projection.transitions) {
      if (!Array.isArray(edge.points) || edge.points.length < 2) throw new Error(`Transition ${edge.id} has no route`);
      const points = edge.points.map(([a,b]) => [a+x,b+y]);
      const path = svgNode('path', {d:points.map(([a,b],i) => `${i?'L':'M'} ${a} ${b}`).join(' '), 'marker-end':'url(#diagram-arrow)', 'data-id':edge.id, 'data-event':edge.event, 'aria-label':edge.sentence});
      path.classList.add('diagram-transition');
      const title = svgNode('title'); title.textContent = edge.sentence; path.appendChild(title); svg.appendChild(path);
      // Keep the event on the longest routing segment, away from card titles.
      let segment = [points[0],points[1]], longest = 0;
      for (let i=1;i<points.length;i++) {
        const a=points[i-1],b=points[i],length=Math.hypot(b[0]-a[0],b[1]-a[1]);
        if (length>longest) {longest=length;segment=[a,b];}
      }
      const [[ax,ay],[bx,by]]=segment;
      const label = svgNode('text',{x:(ax+bx)/2+((ax===bx)?8:0),y:(ay+by)/2-8,'text-anchor':ax===bx?'start':'middle','data-edge':edge.id});
      label.classList.add('diagram-event');
      label.textContent=edge.event+(edge.guard?` [${edge.guard}]`:'');
      svg.appendChild(label);
    }
    stage.appendChild(svg);
    const legend=document.createElement('aside'); legend.className='diagram-legend';
    legend.textContent='● Initial state   ·   ◎ Terminal state   ·   Arrow: event [guard]';
    if (projection.problems.length) {
      const problems=document.createElement('p'); problems.className='diagram-problems';
      problems.textContent=projection.problems.map(p=>p.detail).join('\n'); legend.appendChild(problems);
    }
    mount.querySelectorAll('.diagram-legend').forEach(node=>node.remove());
    if (projection.states.length) mount.appendChild(legend);
    globalThis.samepageDiagramMetrics = {revision, states:projection.states.length, transitions:projection.transitions.length, readback:projection.readback};
  }
  const unregister=application.registerRenderer('diagrams',{render});
  return {dispose(){disposed=true;unregister();style.remove();}};
}
