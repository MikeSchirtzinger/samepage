// The verified Explain WASM owns graphs, cursors, validation and read-back.
// This adapter renders that one document and runs browser-local animations.
export function activate({wasm, application, mount}) {
  if (!wasm || !application || !mount) throw Error('explain needs its verified WASM and shared document');
  application.registerCapability('explain',wasm);
  const map=document.getElementById('map');
  let disposed=false, selectedFlow=null, projection=null, panelSignature='', busy=false;
  let selectedIds=[], responseDraft='', transitionDraft='', scoped=false;
  const effects=new Map();
  const el=(tag,text,cls)=>{const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(cls)n.className=cls;return n;};
  const failure=e=>{const b=document.getElementById('error-banner');b.textContent=`explain refused: ${e.message}`;b.hidden=false;};
  const button=(label,fn,cls)=>{const b=el('button',label,cls);b.type='button';b.addEventListener('click',()=>Promise.resolve().then(fn).catch(failure));return b;};
  const decode=raw=>{const r=JSON.parse(raw);if(r.error)throw Error(`${r.error.code}: ${r.error.message}`);return r.ok;};
  const target=id=>map.querySelector(`[data-id="${CSS.escape(id)}"]`);
  const current=()=>projection?.flows.find(f=>f.definition.id===selectedFlow);
  async function dispatch(name,args) {
    if(busy)throw Error('wait for the current walkthrough action');
    busy=true;mount.querySelectorAll('button,select').forEach(n=>n.disabled=true);
    try {return await application.dispatch(name,args);}
    finally {busy=false;panelSignature='';void render({view:application.view()}).catch(failure);}
  }
  const advance=(f,t,response,ids)=>dispatch('walkthrough_advance',{flow_id:f.definition.id,expected_revision:f.state.revision,transition_id:t.id,response,selected_target_ids:ids});
  function clearEffects() {
    for(const {animation,overlay,node} of effects.values()){animation?.cancel();overlay?.remove();node?.classList.remove('explain-target-active','explain-target-reveal');}
    effects.clear();
  }
  function animate(f,beat) {
    const prefix=`${f.object_id}:${f.state.entered_revision}:${beat.id}:`;
    for(const [key,e] of effects){if(!key.startsWith(prefix)||f.state.status==='stopped'){e.animation?.cancel();e.overlay?.remove();e.node?.classList.remove('explain-target-active','explain-target-reveal');effects.delete(key);}}
    if(f.state.status==='stopped')return;
    for(const [i,a] of beat.actions.entries()) {
      if(a.kind==='replay-motion')continue;
      const node=target(a.target_id);if(!node)throw Error(`explain ${a.kind}: target ${a.target_id} has no rendered element`);
      const key=prefix+i, previous=effects.get(key);
      if(previous?.node===node) {
        if(f.state.status==='paused')previous.animation?.pause();
        else if(previous.animation?.playState==='paused')previous.animation.play();
        if(f.state.status==='completed'&&previous.animation?.effect.getTiming().iterations===Infinity){previous.animation.effect.updateTiming({iterations:1});previous.animation.finish();}
        continue;
      }
      const savedTime=previous?.animation?.currentTime;previous?.animation?.cancel();previous?.overlay?.remove();
      let overlay=null,animation=null;
      const reduced=matchMedia('(prefers-reduced-motion: reduce)').matches;
      if(a.kind==='trace-path') {
        const path=node.matches('path,polyline,line')?node:node.querySelector('path,polyline,line');
        if(!path)throw Error(`explain trace-path: connector ${a.target_id} has no rendered path`);
        const length=path.getTotalLength();if(!(length>0))throw Error(`explain trace-path: connector ${a.target_id} has an empty path`);
        overlay=path.cloneNode(false);overlay.classList.add('explain-path-trace');overlay.removeAttribute('data-id');overlay.setAttribute('aria-hidden','true');overlay.setAttribute('pointer-events','none');overlay.setAttribute('stroke','#fbbf24');overlay.setAttribute('stroke-width','6');overlay.setAttribute('fill','none');
        const dash=Math.max(8,length*.08);overlay.setAttribute('stroke-dasharray',`${dash} ${length+dash}`);path.parentNode.append(overlay);
        if(!reduced)animation=overlay.animate([{strokeDashoffset:String(dash)},{strokeDashoffset:String(-length)}],{duration:2400,iterations:f.state.status==='completed'?1:Infinity,easing:'linear',fill:'forwards'});
      } else {
        node.classList.add('explain-target-active');if(a.kind==='reveal')node.classList.add('explain-target-reveal');
        if(!previous&&a.kind==='reveal')node.scrollIntoView({behavior:reduced?'instant':'smooth',block:'nearest',inline:'nearest'});
        if(!reduced)animation=node.animate([{filter:'brightness(1)'},{filter:'brightness(1.65)'},{filter:'brightness(1)'}],{duration:900,fill:'forwards'});
      }
      if(animation&&savedTime!==undefined&&savedTime!==null)animation.currentTime=savedTime;
      if(f.state.status==='paused')animation?.pause();
      effects.set(key,{animation,overlay,node});
      node.dataset.explainAction=a.kind;node.dataset.explainEnteredRevision=String(f.state.entered_revision);
    }
  }
  function renderPanel(f,beat) {
    const activeInput=mount.contains(document.activeElement)?document.activeElement.id:null;
    const selectionStart=document.activeElement?.selectionStart;
    const fragment=document.createDocumentFragment(), header=el('div',undefined,'explain-header');
    header.append(el('h2','Walkthrough'));
    const picker=el('select');picker.id='explain-flow';picker.setAttribute('aria-label','Walkthrough');
    for(const flow of projection.flows){const o=el('option',flow.definition.title);o.value=flow.definition.id;o.selected=flow.definition.id===f.definition.id;picker.append(o);}
    picker.addEventListener('change',()=>{selectedFlow=picker.value;responseDraft='';transitionDraft='';scoped=false;delete map.dataset.explorationId;clearEffects();panelSignature='';void render({view:application.view()}).catch(failure);});header.append(picker);
    header.append(el('span',`${f.state.status} · revision ${f.state.revision}`,'explain-status'));fragment.append(header,el('p',f.definition.goal));
    const steps=el('ol',undefined,'explain-steps');for(const b of f.definition.beats){const li=el('li',b.title);li.dataset.beat=b.id;if(b.id===f.state.current_beat){li.classList.add('explain-current');li.setAttribute('aria-current','step');}steps.append(li);}fragment.append(steps);
    fragment.append(el('h3',beat.title),el('p',beat.cue,'explain-cue'));
    const unsupported=projection.unsupported.filter(a=>a.flow_id===f.definition.id);for(const a of unsupported){const warning=el('p',`Unsupported visual action: ${a.kind} (step ${a.beat_id}). Other actions and transitions remain available.`,'explain-unsupported');warning.setAttribute('role','status');fragment.append(warning);}
    const evidence=el('ul',undefined,'explain-evidence');for(const e of beat.evidence){const li=el('li');li.append(button(e.detail,()=>{const n=target(e.target_id);if(!n)throw Error(`explain evidence target ${e.target_id} is unavailable`);n.scrollIntoView({behavior:'smooth',block:'nearest'});selectedIds=[e.target_id];panelSignature='';void render({view:application.view()}).catch(failure);}));evidence.append(li);}fragment.append(evidence);
    const controls=el('div',undefined,'explain-controls');for(const command of ['pause','resume','restart','stop']) {
      if(command==='pause'&&f.state.status!=='active'||command==='resume'&&f.state.status!=='paused'||command==='stop'&&!['active','paused'].includes(f.state.status))continue;
      controls.append(button(command[0].toUpperCase()+command.slice(1),()=>dispatch('walkthrough_control',{flow_id:f.definition.id,expected_revision:f.state.revision,command})));
    }fragment.append(controls);
    if(f.state.status==='active') {
      const transitions=el('div',undefined,'explain-transitions');for(const t of beat.advance.transitions.filter(t=>t.control==='button')) {
        const b=button(t.label,async()=>{await advance(f,t,t.label,selectedIds.length?selectedIds:t.target_ids);responseDraft='';transitionDraft='';},'explain-transition');b.dataset.transition=t.id;transitions.append(b);
      }fragment.append(el('p',beat.advance.prompt),transitions);
      const resolved=beat.advance.transitions.filter(t=>!t.control);
      if(resolved.length){const form=el('form',undefined,'explain-response-form'),label=el('label','Your response');const input=el('textarea');input.id='explain-response';input.value=responseDraft;input.maxLength=1200;input.rows=2;input.addEventListener('input',()=>responseDraft=input.value);label.append(input);form.append(label);
        const choice=el('select');choice.id='explain-transition-choice';choice.setAttribute('aria-label','Response transition');for(const t of resolved){const o=el('option',t.label);o.value=t.id;o.selected=t.id===transitionDraft;choice.append(o);}choice.addEventListener('change',()=>transitionDraft=choice.value);form.append(choice);
        const submit=el('button','Send response');submit.type='submit';form.append(submit);form.addEventListener('submit',event=>{event.preventDefault();const t=resolved.find(t=>t.id===choice.value);void advance(f,t,input.value,selectedIds).then(()=>{responseDraft='';transitionDraft='';}).catch(failure);});fragment.append(form);}
    }
    fragment.append(el('p',`Selected evidence: ${selectedIds.map(id=>application.view().records.find(r=>r.id===id)?.props.label||id).join(', ')||'none — select a card or drawing'}`,'explain-selection'));
    if(f.definition.scene_ids.length){const scope=button(scoped?'Finish drawing in exploration':'Draw in this exploration',()=>{scoped=!scoped;if(scoped)map.dataset.explorationId=f.definition.id;else delete map.dataset.explorationId;panelSignature='';void render({view:application.view()}).catch(failure);});scope.setAttribute('aria-pressed',String(scoped));fragment.append(scope);}
    fragment.append(el('small',`Created by ${f.created_by||'unknown'} · last by ${f.state.touched_by||'unknown'}`,'explain-byline'));
    mount.replaceChildren(fragment);mount.hidden=false;mount.dataset.flow=f.definition.id;mount.dataset.beat=f.state.current_beat;mount.dataset.revision=String(f.state.revision);mount.dataset.status=f.state.status;
    mount.querySelectorAll('button,select').forEach(n=>n.disabled=busy||!application.ready());
    if(activeInput){const n=document.getElementById(activeInput);n?.focus({preventScroll:true});if(typeof selectionStart==='number'&&n?.setSelectionRange)n.setSelectionRange(selectionStart,selectionStart);}
  }
  async function render({view}) {
    if(disposed)return;projection=decode(wasm.project(JSON.stringify(view)));
    if(!projection.flows.length){mount.hidden=true;clearEffects();delete map.dataset.explorationId;return;}
    if(!projection.flows.some(f=>f.definition.id===selectedFlow))selectedFlow=projection.flows[0].definition.id;
    const f=current(),beat=f.definition.beats.find(b=>b.id===f.state.current_beat);
    const signature=JSON.stringify([projection,selectedFlow,selectedIds,scoped,busy,application.ready()]);
    if(signature!==panelSignature){renderPanel(f,beat);panelSignature=signature;}
    animate(f,beat);
  }
  function select(event){const n=event.target.closest('[data-id]');if(!n||!map.contains(n)||event.target.closest('button,input,textarea'))return;const id=n.dataset.id;if(event.shiftKey){selectedIds=selectedIds.includes(id)?selectedIds.filter(s=>s!==id):[...selectedIds,id].slice(0,32);}else selectedIds=[id];panelSignature='';void render({view:application.view()}).catch(failure);}
  map.addEventListener('click',select,true);
  const unsubscribe=application.subscribe(()=>{mount.querySelectorAll('button,select').forEach(n=>n.disabled=busy||!application.ready());});
  application.registerRenderer('explain',{render,dispose(){disposed=true;clearEffects();unsubscribe();map.removeEventListener('click',select,true);delete map.dataset.explorationId;mount.replaceChildren();mount.hidden=true;}});
}
