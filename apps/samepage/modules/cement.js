// Explicit reviewed intent; native publication executes the real gate.
export function activate({wasm,application,mount}) {
  if(!wasm||!application)throw Error('Cement requires verified WASM and the shared document');
  application.registerCapability('cement',wasm);
  let disposed=false,busy=false,selected='',preview=null,history=null,lastReceipt=null;
  const el=(tag,text,cls)=>{const e=document.createElement(tag);if(text!==undefined)e.textContent=text;if(cls)e.className=cls;return e;};
  // Problems stay in the Cement tab, keyed by the control that failed, and
  // that control's next success clears them.
  const fail=(e,action='read')=>application.problem('cement',action,action==='read'?`Cement could not read the page: ${e.message}`:`Cement refused: ${e.message}`);
  const dispatch=async(name,args)=>JSON.parse((await application.dispatch(name,args)).result);
  const envelope=raw=>{const r=JSON.parse(raw);if(r.abi!=='samepage.capability.v2'||r.error)throw Error(r.error?.message||'Cement ABI mismatch');return r.ok;};
  const button=(text,id,fn)=>{const b=el('button',text);b.type='button';b.id=id;b.addEventListener('click',()=>{b.disabled=true;Promise.resolve().then(fn).then(()=>application.resolved('cement',id)).catch(e=>fail(e,id)).finally(()=>{if(b.isConnected)b.disabled=false;});});return b;};
  async function render({view}) {
    if(disposed||busy||!application.ready())return;busy=true;
    try {
      const data=await dispatch('mode_read',{});if(data.revision!==view.revision)return;
      const projected=envelope(wasm.project(JSON.stringify(view)));
      const strip=el('section',undefined,'cement-strip');strip.append(el('strong',`Mode: ${projected.mode}`));
      strip.append(button('Learning','cement-learning',async()=>{await dispatch('mode_set',{mode:'learning'});preview=null;await refresh();}),button('Alignment','cement-alignment',async()=>{await dispatch('mode_set',{mode:'alignment'});preview=null;await refresh();}));
      strip.append(el('p','Review explicit decisions and checks before publication. Structural gates do not prove behavior.'));
      if(projected.drafts.length){
        if(!projected.drafts.some(d=>d.id===selected))selected=projected.drafts.at(-1).id;
        const picker=el('select');picker.id='cement-draft';picker.setAttribute('aria-label','Decision draft');
        for(const d of projected.drafts){const o=el('option',`${d.input.title} · ${d.created_by}`);o.value=d.id;o.selected=d.id===selected;picker.append(o);}picker.addEventListener('change',()=>{selected=picker.value;preview=null;});strip.append(picker);
        strip.append(button('Review decision','cement-preview',async()=>{const d=projected.drafts.find(d=>d.id===selected);const r=await dispatch('decision_preview',{ids:d.input.ids,draft_id:d.id});const actual=envelope(wasm.bundle_with_facts(JSON.stringify(application.view()),JSON.stringify({ids:d.input.ids,draft_id:d.id}),JSON.stringify(r.cement.facts)));if(JSON.stringify(actual)!==JSON.stringify(r.cement.bundle)) {
            // Native and WASM serialization may order keys differently; compare token and artifact bytes.
            if(actual.review_token!==r.cement.bundle.review_token||actual.adr!==r.cement.bundle.adr||JSON.stringify(actual.fragment)!==JSON.stringify(r.cement.bundle.fragment))throw Error('Portable reviewed bundle differs from native publication');
          }
          preview=actual;await refresh();}));
      }else strip.append(el('p','No decision drafts on this page.'));
      if(preview){const review=el('details',undefined,'cement-review');review.open=true;review.append(el('summary',`Review: ${preview.draft.input.title}`),el('pre',preview.adr),el('pre',JSON.stringify(preview.fragment,null,2)),el('p',`Review token: ${preview.review_token}`));review.append(button('Cement reviewed decision','cement-publish',async()=>{const reviewed=preview;const r=await dispatch('decision_cement',{ids:reviewed.draft.input.ids,draft_id:reviewed.draft.id,review_token:reviewed.review_token});preview=null;lastReceipt=r.cement.receipt;await refresh();}));strip.append(review);}
      strip.append(button('Decision history','cement-history',async()=>{history=(await dispatch('decision_list',{})).cement;await refresh();}));
      if(history){const list=el('details',undefined,'cement-history');list.open=true;list.append(el('summary','Published decisions'));for(const d of history)list.append(el('pre',d.body));list.append(button('Close history','cement-history-close',async()=>{history=null;await refresh();}));strip.append(list);}
      if(lastReceipt){const receipt=el('details',undefined,'cement-receipt');receipt.append(el('summary',`Publication receipt: ${lastReceipt.g8_status}`),el('pre',JSON.stringify(lastReceipt,null,2)));strip.append(receipt);}
      mount.replaceChildren(strip);


      application.resolved('cement','read');
    }catch(e){fail(e);}finally{busy=false;}
  }
  async function refresh(){await render({view:application.view()});}
  const open=()=>void refresh();document.addEventListener('samepage:cement-open',open);
  const unregister=application.registerRenderer('cement',{render});const timer=setInterval(()=>void refresh(),3000);
  return{dispose(){disposed=true;clearInterval(timer);unregister();document.removeEventListener('samepage:cement-open',open);}};
}
