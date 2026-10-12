// Explicit reviewed intent; native publication executes the real gate.
// The tab says what still stands between a draft and publication, in order,
// and only offers Review once every condition holds.
export function activate({wasm,application,mount}) {
  if(!wasm||!application)throw Error('Cement requires verified WASM and the shared document');
  application.registerCapability('cement',wasm);
  let disposed=false,busy=false,selected='',preview=null,history=null,lastReceipt=null,showStale=false,signature='';
  const el=(tag,text,cls)=>{const e=document.createElement(tag);if(text!==undefined)e.textContent=text;if(cls)e.className=cls;return e;};
  // Problems stay in the Cement tab, keyed by the control that failed, and
  // that control's next success clears them.
  const fail=(e,action='read')=>application.problem('cement',action,action==='read'?`Cement could not read the page: ${e.message}`:`Cement refused: ${e.message}`);
  const dispatch=async(name,args)=>JSON.parse((await application.dispatch(name,args)).result);
  const envelope=raw=>{const r=JSON.parse(raw);if(r.abi!=='samepage.capability.v2'||r.error)throw Error(r.error?.message||'Cement ABI mismatch');return r.ok;};
  const button=(text,id,fn)=>{const b=el('button',text);b.type='button';b.id=id;b.addEventListener('click',()=>{b.disabled=true;Promise.resolve().then(fn).then(()=>application.resolved('cement',id)).catch(e=>fail(e,id)).finally(()=>{if(b.isConnected)b.disabled=false;});});return b;};
  const node=(view,id)=>view.records.find(r=>r.kind==='node'&&r.id===id);
  // Whether the page still matches what the draft was written against, asked
  // of the verified cement WASM itself. With empty native facts the bundle
  // stops at its first check, so only a stale draft reports the page change.
  function current(view,d){
    const raw=JSON.parse(wasm.bundle_with_facts(JSON.stringify(view),JSON.stringify({ids:d.input.ids,draft_id:d.id}),JSON.stringify({repository:'',at:'',number:0,spec_hash:null,index_hash:null,evidence:{}})));
    return !String(raw.error?.message||'').includes('changed after this decision draft');
  }
  function readiness(view,d){
    const cards=d.input.ids.map(id=>node(view,id));
    const unagreed=cards.filter(n=>n&&n.props.status!=='agreed').map(n=>n.props.label||n.id);
    const claims=view.records.filter(r=>r.kind==='claim'&&d.input.ids.includes(r.props.about)&&!r.props.withdrawn);
    const open=claims.filter(c=>c.props.verdict!=='accepted'||c.props.basis==='unknown');
    const bare=d.input.ids.filter(id=>!claims.some(c=>c.props.about===id)).map(id=>node(view,id)?.props.label||id);
    const fresh=current(view,d);
    return [
      {ok:!unagreed.length,text:unagreed.length?`Not agreed yet: ${unagreed.join(', ')}. Mark ${unagreed.length===1?'it':'them'} agreed in Review.`:`${cards.length===1?'The card is':`All ${cards.length} cards are`} agreed.`,card:d.input.ids.find(id=>node(view,id)?.props.status!=='agreed')},
      {ok:!open.length&&!bare.length,text:bare.length?`No claim on ${bare.join(', ')}. An agent claim with a source is needed.`:open.length?`${open.length} claim${open.length===1?' is':'s are'} not accepted. Accept or withdraw ${open.length===1?'it':'them'} in Review.`:`${claims.length===1?'Its claim is':`All ${claims.length} claims are`} accepted.`,card:open[0]?.props.about},
      {ok:fresh,text:fresh?'The draft matches the page as it is now.':'The page changed after this draft was written. Ask the agent to redraft it.',redraft:!fresh},
    ];
  }
  function table(fragment){
    const rows=Array.isArray(fragment?.obligations)?fragment.obligations:null;
    if(!rows)return el('pre',JSON.stringify(fragment,null,2));
    const t=el('table',undefined,'cement-fragment');const head=el('tr');for(const h of ['Check','Requirement','How it is checked'])head.append(el('th',h));t.append(head);
    const words=v=>typeof v==='string'?v:v&&typeof v==='object'?(v.description||v.text||JSON.stringify(v)):'';
    for(const o of rows){const tr=el('tr');const checks=(o.checker?.checks||[]).map(c=>c.backend).filter(Boolean);tr.append(el('td',o.id||''),el('td',words(o.rule)||words(o.requirement)||o.label||''),el('td',[checks.length?checks.join(', '):o.checker?.mode,o.signal?.gate?`gate ${o.signal.gate}`:''].filter(Boolean).join(' · ')));t.append(tr);}
    return t;
  }
  function receiptLine(r){
    const failures=Array.isArray(r?.g8_check?.enforcement_failures)?r.g8_check.enforcement_failures:[];
    if(r?.g8_status==='active_passing'&&!failures.length)return 'Published · gate met';
    if(r?.g8_status==='active_passing')return `Published · checks pass, gate not met yet (${failures.map(f=>f.reason||f.code||f.id||'unratified').join(', ')})`;
    return `Published · checks failing (${r?.g8_status||'unknown'})`;
  }
  async function render({view},force=false) {
    if(disposed||busy||!application.ready())return;busy=true;
    try {
      const projected=envelope(wasm.project(JSON.stringify(view)));
      const drafts=projected.drafts.map(d=>({d,ready:readiness(view,d)}));
      // Current drafts first; drafts the page has moved past are hidden until asked for.
      drafts.sort((a,b)=>Number(b.ready[2].ok)-Number(a.ready[2].ok)||b.d.id.localeCompare(a.d.id));
      const visible=drafts.filter(x=>x.ready[2].ok||showStale);
      const next=JSON.stringify([projected.mode,drafts.map(x=>[x.d.id,x.ready]),selected,showStale,Boolean(preview),Boolean(history),lastReceipt?.g8_status]);
      if(!force&&next===signature)return;
      signature=next;
      if(!visible.some(x=>x.d.id===selected))selected=visible[0]?.d.id||'';
      const strip=el('section',undefined,'cement-strip');
      const posture=el('div',undefined,'cement-posture');posture.append(el('strong',`Posture: ${projected.mode}`),button('Learning','cement-learning',async()=>{await dispatch('mode_set',{mode:'learning'});preview=null;await refresh();}),button('Alignment','cement-alignment',async()=>{await dispatch('mode_set',{mode:'alignment'});preview=null;await refresh();}));
      strip.append(posture,el('p','Publishing writes a reviewed decision and its checks into the project. Structural checks do not prove behaviour.','cement-note'));
      const stale=drafts.length-drafts.filter(x=>x.ready[2].ok).length;
      if(!drafts.length)strip.append(el('p','No decision drafts yet. Ask the agent to draft a decision for the agreed cards.','cement-empty'));
      else if(!visible.length)strip.append(el('p','Every draft was written before the page last changed. Ask the agent to redraft.','cement-empty'));
      if(visible.length){
        const picker=el('select');picker.id='cement-draft';picker.setAttribute('aria-label','Decision draft');
        for(const {d,ready} of visible){const o=el('option',`${d.input.title}${ready[2].ok?'':' (out of date)'}`);o.value=d.id;o.selected=d.id===selected;picker.append(o);}
        picker.addEventListener('change',()=>{selected=picker.value;preview=null;void refresh(true);});strip.append(picker);
        const chosen=visible.find(x=>x.d.id===selected);
        const list=el('ul',undefined,'cement-checklist');list.setAttribute('aria-label','Ready to publish');
        for(const item of chosen.ready){
          const li=el('li');li.dataset.ok=String(item.ok);li.append(el('span',item.ok?'✓':'○','cement-check'),el('span',item.text));
          if(!item.ok&&item.card){const go=button('Open in Review',`cement-go-${item.card}`,async()=>{await application.activate?.('review');document.dispatchEvent(new CustomEvent('samepage:review-open',{detail:{id:item.card}}));document.dispatchEvent(new CustomEvent('samepage:map-focus',{detail:{id:item.card}}));});li.append(go);}
          if(item.redraft){const ask=button('Copy a redraft request','cement-redraft',async()=>{await navigator.clipboard.writeText(`Please redraft the decision "${chosen.d.input.title}" for ${chosen.d.input.ids.map(id=>node(view,id)?.props.label||id).join(', ')}: the page changed after the last draft.`);application.toast?.('Copied a redraft request for the agent.');});li.append(ask);}
          list.append(li);
        }
        const readyCount=chosen.ready.filter(x=>x.ok).length;
        strip.append(el('p',`${readyCount} of 3 ready`,'cement-ready'),list);
        const review=button('Review decision','cement-preview',async()=>{const d=chosen.d;const r=await dispatch('decision_preview',{ids:d.input.ids,draft_id:d.id});const actual=envelope(wasm.bundle_with_facts(JSON.stringify(application.view()),JSON.stringify({ids:d.input.ids,draft_id:d.id}),JSON.stringify(r.cement.facts)));if(JSON.stringify(actual)!==JSON.stringify(r.cement.bundle)) {
            // Native and WASM serialization may order keys differently; compare token and artifact bytes.
            if(actual.review_token!==r.cement.bundle.review_token||actual.adr!==r.cement.bundle.adr||JSON.stringify(actual.fragment)!==JSON.stringify(r.cement.bundle.fragment))throw Error('Portable reviewed bundle differs from native publication');
          }
          preview=actual;await refresh(true);});
        review.disabled=readyCount<3;review.title=readyCount<3?'Every condition above must hold first':'Show exactly what will be written';
        strip.append(review);
      }
      if(stale){const toggle=button(showStale?`Hide ${stale} out-of-date draft${stale===1?'':'s'}`:`Show ${stale} out-of-date draft${stale===1?'':'s'}`,'cement-stale',async()=>{showStale=!showStale;await refresh(true);});toggle.className='quiet';strip.append(toggle);}
      if(preview){const review=el('section',undefined,'cement-review');review.append(el('h3',`Review: ${preview.draft.input.title}`),el('h4','Decision record'),el('pre',preview.adr),el('h4','Checks written to the project'),table(preview.fragment));review.append(button('Cement reviewed decision','cement-publish',async()=>{const reviewed=preview;const r=await dispatch('decision_cement',{ids:reviewed.draft.input.ids,draft_id:reviewed.draft.id,review_token:reviewed.review_token});preview=null;lastReceipt=r.cement.receipt;await refresh(true);}));strip.append(review);}
      if(lastReceipt){const receipt=el('details',undefined,'cement-receipt');receipt.append(el('summary',receiptLine(lastReceipt)),el('pre',JSON.stringify(lastReceipt,null,2)));strip.append(receipt);}
      strip.append(button(history?'Refresh published decisions':'Published decisions','cement-history',async()=>{history=(await dispatch('decision_list',{})).cement;await refresh(true);}));
      if(history){const list=el('details',undefined,'cement-history');list.open=true;list.append(el('summary',`Published decisions · ${history.length}`));for(const d of history)list.append(el('pre',d.body));list.append(button('Close','cement-history-close',async()=>{history=null;await refresh(true);}));strip.append(list);}
      mount.replaceChildren(strip);
      const summary=document.getElementById('cement-summary');
      if(summary){const best=visible.find(x=>x.d.id===selected);summary.textContent=best?`Cement: ${best.ready.filter(x=>x.ok).length} of 3 ready`:drafts.length?'Cement: redraft needed':'Cement: no draft yet';}
      application.resolved('cement','read');
    }catch(e){fail(e);}finally{busy=false;}
  }
  async function refresh(force=false){await render({view:application.view()},force);}
  const open=()=>void refresh(true);document.addEventListener('samepage:cement-open',open);
  const unregister=application.registerRenderer('cement',{render});
  return{dispose(){disposed=true;unregister();document.removeEventListener('samepage:cement-open',open);}};
}
