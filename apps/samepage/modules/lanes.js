// Native extraction is data. Verified WASM reconciles it with the shared map.
export function activate({wasm, application, mount}) {
  if (!wasm || !application) throw Error('Lanes needs verified WASM and the shared document');
  application.registerCapability('lanes', wasm);
  let disposed=false, busy=false, opened=false, selected='', group='', page=0;
  const expanded=new Set();
  const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(cls)n.className=cls;return n;};
  const fail=(e,action='read')=>application.problem('lanes',action,action==='read'?`Found in source could not be read: ${e.message}`:`Lane claim refused: ${e.message}`);
  const dispatch=async(name,args)=>JSON.parse((await application.dispatch(name,args)).result);
  const envelope=raw=>{const r=JSON.parse(raw);if(r.abi!=='samepage.capability.v2'||r.error)throw Error(r.error?.message||'Lanes ABI mismatch');return r.ok;};
  const button=(label,action,key='read')=>{const n=el('button',label);n.type='button';n.addEventListener('click',()=>Promise.resolve().then(action).then(()=>application.resolved('lanes',key)).catch(e=>fail(e,key)));return n;};
  async function render({view}) {
    if(disposed||busy||!application.ready())return;busy=true;
    try {
      const data=(await dispatch('lanes_read',{})).lanes;
      if(data.revision!==view.revision)return;
      const projection=envelope(wasm.project_with_facts(JSON.stringify(view),JSON.stringify(data.facts)));
      const strip=el('details',undefined,'lanes-strip');strip.open=true;
      strip.append(el('summary',`Found in source · ${projection.unresolved_count} unresolved · ${projection.lanes.length} findings`));
      strip.addEventListener('toggle',()=>{opened=strip.open;});
      const description=el('p','Static source patterns, not observed running processes. Extraction may miss or misclassify code. A vanished source remains unresolved until reviewed.');
      const controls=el('div',undefined,'lanes-controls');
      const nodes=view.records.filter(r=>r.kind==='node');
      // The claim uses the card selected on the map, not a list of every node.
      if(!nodes.some(n=>n.id===selected))selected='';
      const picker=el('span',undefined,'lanes-selection');picker.id='lanes-card';picker.setAttribute('role','status');
      const groups=el('select');groups.id='lanes-group';groups.setAttribute('aria-label','Finding group');
      if(!projection.groups.some(g=>g.name===group))group=projection.groups[0]?.name||'';
      for(const g of projection.groups){const o=el('option',`${g.name} (${g.count})`);o.value=g.name;o.selected=g.name===group;groups.append(o);}
      groups.addEventListener('change',()=>{group=groups.value;page=0;void render({view:application.view()});});
      controls.append(picker,el('label','Group: '),groups,button('Rescan',()=>render({view:application.view()})));
      const all=projection.lanes.filter(l=>`${l.kind} · ${l.package||'project'}`===group);
      if(page>=all.length)page=0;
      const findings=el('div',undefined,'lane-findings');
      for(const lane of all.slice(page,page+20)) {
        const item=el('details',undefined,'lane-finding');item.dataset.laneId=lane.lane_id;item.dataset.state=lane.removal?'removal':lane.matched_node_id?'claimed':'unresolved';item.open=expanded.has(lane.lane_id);
        item.append(el('summary',`${lane.matched_node_id?'✓ Claimed':lane.removal?.tier==='gate-met'?'✓ Removal met':'? Unresolved'} · ${lane.label} · ${lane.evidence.path}:${lane.evidence.line}${lane.removal?' · '+lane.removal.chip:''}`),el('p',lane.detail),el('pre',lane.evidence.snippet,'lane-evidence'));
        const binding=view.records.find(r=>r.kind==='binding'&&r.props.lane_id===lane.lane_id);
        if(binding)item.append(el('p',`Claimed by ${binding.props.touched_by||binding.props.created_by||'unknown'} for ${nodes.find(n=>n.id===binding.props.node)?.props.label||binding.props.node}`,'lane-byline'));
        const excerpt=el('pre','', 'lane-excerpt');item.append(excerpt);
        item.addEventListener('toggle',async()=>{if(item.open){expanded.add(lane.lane_id);try{const r=await dispatch('source_read',{path:lane.evidence.path,lines:String(lane.evidence.line)});excerpt.textContent=r.sources.text;}catch(e){excerpt.textContent=`Source unavailable: ${e.message}`;}}else expanded.delete(lane.lane_id);});
        if(lane.removal)item.append(el('p',lane.removal.reason||`${lane.removal.chip}: ${lane.removal.obligation_id}`));
        if(!lane.removal || lane.evidence.sha256) {
          const claim=button('Claim with the selected card',async()=>{if(!selected)throw Error('Select a card on the map first');await dispatch('lane_claim',{lane_id:lane.lane_id,node_id:selected,scan_sha256:lane.evidence.sha256});await render({view:application.view()});},'lane_claim');claim.className='lane-claim';item.append(claim);
        }
        findings.append(item);
      }
      const pagination=el('div',undefined,'lanes-controls');pagination.append(el('span',`${all.length? page+1:0}–${Math.min(page+20,all.length)} of ${all.length} in this group`));
      if(page>0)pagination.append(button('Previous findings',()=>{page=Math.max(0,page-20);return render({view:application.view()});}));
      if(page+20<all.length)pagination.append(button('More findings',()=>{page+=20;return render({view:application.view()});}));
      strip.append(description,controls,findings,pagination);mount.replaceChildren(strip);showSelection();
      application.resolved('lanes','read');
      const opener=document.querySelector('#lanes-open');if(opener){opener.textContent=`Found in source · ${projection.unresolved_count} unresolved`;opener.dataset.attention=String(projection.unresolved_count>0);}
    } catch(e){fail(e);} finally{busy=false;}
  }
  const open=()=>{opened=true;void render({view:application.view()});};
  function showSelection(){const node=application.view().records.find(r=>r.kind==='node'&&r.id===selected);const label=node?.props.label||'';const hint=mount.querySelector('#lanes-card');if(hint)hint.textContent=label?`Claiming with ${label}`:'Select a card on the map, then claim a finding with it.';mount.querySelectorAll('.lane-claim').forEach(b=>{b.disabled=!label;b.textContent=label?`Claim with ${label}`:'Claim with the selected card';});}
  const selectionChanged=event=>{selected=event.detail?.kind==='node'?event.detail.id:'';showSelection();};
  selected=window.samepageSelection?.kind==='node'?window.samepageSelection.id:'';
  document.addEventListener('map-selection-changed',selectionChanged);
  document.addEventListener('samepage:lanes-open',open);
  const unregister=application.registerRenderer('lanes',{render});
  // Rescan on a timer only while the findings are on screen.
  const timer=setInterval(()=>{const drawer=document.getElementById('dock-drawer');if(drawer&&!drawer.hidden&&drawer.dataset.drawer==='lanes')void render({view:application.view()});},5000);
  return {dispose(){disposed=true;clearInterval(timer);unregister();document.removeEventListener('samepage:lanes-open',open);document.removeEventListener('map-selection-changed',selectionChanged);}};
}
