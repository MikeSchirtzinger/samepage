// Review decorates the existing Map and renders the portable WASM projection.
// All writes go through authenticated server actions and its authority hook.
export function activate({wasm, application}) {
  const panel = document.getElementById('review');
  const map = document.getElementById('map');
  if (!panel || !map || !wasm || !application) throw new Error('Review needs its verified WASM, shared document and mounts');
  application.registerCapability('review', wasm);
  let selected = null, latest = null, disposed = false, questionDraft = '';
  const drafts = new Map();
  const el = (tag, text, className) => {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  };
  const showError = error => {
    const banner = document.getElementById('error-banner');
    banner.textContent = `Review refused: ${error.message}`; banner.hidden = false;
  };
  const dispatch = async (action, args) => {
    try { return await application.dispatch(action, args); }
    catch (error) { showError(error); throw error; }
  };
  const button = (text, action) => {
    const b = el('button', text); b.type = 'button';
    b.addEventListener('click', () => { Promise.resolve().then(action).catch(() => {}); });
    return b;
  };
  function renderPanel(projection) {
    const nodes = new Map(projection.nodes.map(n => [n.id,n]));
    if (!selected || !nodes.has(selected)) selected = projection.claims.find(c => !c.withdrawn)?.about || projection.marks.find(m => nodes.has(m.target))?.target || projection.nodes[0]?.id || null;
    // The shell owns whether the Review tab is open; rendering never opens it.
    const fragment = document.createDocumentFragment();
    const header = el('div', undefined, 'review-header');
    header.append(el('h2','Review'));
    fragment.append(header);
    if (!selected) { fragment.append(el('p','Select a card to ask a question.')); panel.replaceChildren(fragment); return; }
    const node = nodes.get(selected);
    const picker = el('select'); picker.setAttribute('aria-label','Review card');
    for (const n of projection.nodes) { const option=el('option',n.label); option.value=n.id; option.selected=n.id===selected; picker.append(option); }
    picker.addEventListener('change',()=>{ selected=picker.value; questionDraft=''; renderPanel(projection); });
    fragment.append(picker,el('p',node.state,'review-standing'));
    const form = el('form',undefined,'review-question-form');
    const label = el('label','Question about this card'); label.htmlFor='review-question';
    const input = el('textarea'); input.id='review-question'; input.rows=2; input.maxLength=1200; input.value=questionDraft;
    input.addEventListener('input',()=>{questionDraft=input.value;});
    const submit=el('button','Place ? mark');submit.type='submit';
    form.append(label,input,submit);
    form.addEventListener('submit',async event=>{
      event.preventDefault(); submit.disabled=true;
      try { await dispatch('mark_add',{target:selected,glyph:'?',text:input.value}); questionDraft=''; input.value=''; }
      catch {} finally { submit.disabled=false; }
    });
    fragment.append(form);
    const claims=projection.claims.filter(c=>c.about===selected);
    const claimIds=new Set(claims.map(c=>c.id));
    const marks=projection.marks.filter(m=>m.target===selected||claimIds.has(m.target));
    fragment.append(el('h3',`Marks (${marks.length})`));
    for (const mark of marks) {
      const article=el('article',undefined,'review-mark'); article.dataset.id=mark.id;
      article.append(el('p',`${mark.glyph} ${mark.text || 'Question about this card'}`), el('small',`Asked by ${mark.created_by || 'human'}`));
      if (mark.answer) article.append(el('p',`Answer: ${mark.answer}`,'review-answer'));
      const answerForm=el('form');const answer=el('input');answer.setAttribute('aria-label','Answer this mark');answer.placeholder='Answer in place';answer.maxLength=1200;answer.value=drafts.get(mark.id)||'';
      answer.addEventListener('input',()=>drafts.set(mark.id,answer.value));
      const save=el('button','Answer');save.type='submit';answerForm.append(answer,save);
      answerForm.addEventListener('submit',async event=>{event.preventDefault();save.disabled=true;try{await dispatch('mark_answer',{id:mark.id,answer:answer.value});drafts.delete(mark.id);answer.value='';}catch{}finally{save.disabled=false;}});
      article.append(answerForm);fragment.append(article);
    }
    fragment.append(el('h3',`Claims (${claims.length})`));
    for (const claim of claims) {
      const article=el('article',undefined,'review-claim');article.dataset.id=claim.id;article.dataset.verdict=claim.verdict;
      article.append(el('p',claim.text),el('small',`${claim.basis} · ${claim.state} · ${claim.verdict}`));
      if (claim.path) article.append(el('p',`${claim.path}:${claim.lines}${claim.revision?'@'+claim.revision:''}`,'review-source'));
      if (!claim.withdrawn) {
        const controls=el('div',undefined,'review-verdicts');
        for (const [title,verdict] of [['Accept','accepted'],['Disagree','rejected'],['Reopen','open']]) {
          const b=button(title,()=>dispatch('claim_verdict',{id:claim.id,verdict}));b.disabled=claim.verdict===verdict;controls.append(b);
        }
        article.append(controls);
      }
      fragment.append(article);
    }
    fragment.append(button('Mark card agreed',()=>dispatch('card_place',{id:selected,status:'agreed'})));
    const context=projection.context;
    if (context.current) fragment.append(el('h3','Shared question'),el('p',context.current.question));
    else if (context.heads.length>1) fragment.append(el('p','Shared scope needs reconciliation.'));
    fragment.append(el('p','Readable source supports the citation. It does not prove the claim’s wording or arrows.','review-boundary'));
    panel.replaceChildren(fragment);
  }
  async function render({view}) {
    if (disposed) return;
    const projection=await application.project('review',view);latest=projection;
    document.dispatchEvent(new CustomEvent('samepage:shared-question',{detail:{question:projection.context?.current?.question||null}}));
    map.querySelectorAll('.review-badge, .review-mark-glyphs').forEach(n=>n.remove());
    for (const card of map.querySelectorAll('.map-card')) {
      const node=projection.nodes.find(n=>n.id===card.dataset.id);
      const marks=projection.marks.filter(m=>m.target===card.dataset.id);
      if (node) {
        const badge=el('span',node.state==='SETTLED'?'Settled':node.state==='NO CLAIMS'?'No claims':'Under challenge','review-badge');badge.dataset.state=node.state;card.append(badge);
      }
      if (marks.length) card.append(el('span',marks.map(m=>m.glyph).join(' '),'review-mark-glyphs'));
    }
    renderPanel(projection);
  }
  const opened=event=>{selected=event.detail.id;questionDraft='';if(latest){renderPanel(latest);document.getElementById('review-question')?.focus();}};
  document.addEventListener('samepage:review-open',opened);
  const unregister=application.registerRenderer('review',{render});
  return {dispose(){disposed=true;unregister();document.removeEventListener('samepage:review-open',opened);panel.replaceChildren();panel.hidden=true;}};
}
