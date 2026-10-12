// Review decorates the existing Map and renders the portable WASM projection.
// All writes go through authenticated server actions and its authority hook.
export function activate({wasm, application}) {
  const panel = document.getElementById('review');
  const map = document.getElementById('map');
  if (!panel || !map || !wasm || !application) throw new Error('Review needs its verified WASM, shared document and mounts');
  application.registerCapability('review', wasm);
  let selected = null, latest = null, disposed = false, questionDraft = '';
  const drafts = new Map();
  // A card's standing in words. UNDER CHALLENGE stays readable for an older host.
  const STANDING = {SETTLED:'Settled', 'NO CLAIMS':'No claims', UNREVIEWED:'Unreviewed', QUESTIONED:'Questioned', DISPUTED:'Disputed', 'UNDER CHALLENGE':'Under challenge'};
  const standing = state => STANDING[state] || String(state || '').toLowerCase().replace(/^./, c => c.toUpperCase());
  // "you" is decided here by participant id, never by a name.
  const who = (name, participant) => participant && participant === globalThis.samepagePerson?.id ? 'you' : (String(participant || '').startsWith('agent-') ? String(name || 'agent').replace(/-\d+$/, '') : (name || 'someone'));
  const el = (tag, text, className) => {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  };
  // A refusal stays in the Review tab until the same action succeeds.
  const dispatch = async (action, args) => {
    try { return await application.dispatch(action, args); }
    catch (error) { application.problem('review', action, `Review refused: ${error.message}`); throw error; }
  };
  const button = (text, action) => {
    const b = el('button', text); b.type = 'button';
    b.addEventListener('click', () => { Promise.resolve().then(action).catch(() => {}); });
    return b;
  };
  // The panel is rebuilt only when what it shows changed, and a rebuild puts
  // focus, caret and every draft back, so typing survives agent writes.
  let panelSignature = '';
  function focusKey(node) {
    if (!node || !panel.contains(node)) return null;
    return node.id ? `#${node.id}` : node.dataset.answerFor ? `answer:${node.dataset.answerFor}` : null;
  }
  function restoreFocus(key, start, end) {
    if (!key) return;
    const node = key.startsWith('#') ? panel.querySelector(key) : panel.querySelector(`[data-answer-for="${CSS.escape(key.slice(7))}"]`);
    if (!node) return;
    node.focus({preventScroll:true});
    if (typeof start === 'number' && node.setSelectionRange) node.setSelectionRange(start, end ?? start);
  }
  function renderPanel(projection, force = false) {
    const nodes = new Map(projection.nodes.map(n => [n.id,n]));
    if (!selected || !nodes.has(selected)) selected = projection.claims.find(c => !c.withdrawn)?.about || projection.marks.find(m => nodes.has(m.target))?.target || projection.nodes[0]?.id || null;
    const claimIdsFor = new Set(projection.claims.filter(c=>c.about===selected).map(c=>c.id));
    const signature = JSON.stringify([selected, projection.nodes, projection.claims.filter(c=>c.about===selected), projection.marks.filter(m=>m.target===selected||claimIdsFor.has(m.target)), projection.context]);
    if (!force && signature === panelSignature) return;
    panelSignature = signature;
    const active = focusKey(document.activeElement);
    const caret = [document.activeElement?.selectionStart, document.activeElement?.selectionEnd];
    // The shell owns whether the Review tab is open; rendering never opens it.
    const fragment = document.createDocumentFragment();
    const header = el('div', undefined, 'review-header');
    header.append(el('h2','Review'));
    fragment.append(header);
    if (!selected) { fragment.append(el('p','Select a card to ask a question.')); panel.replaceChildren(fragment); restoreFocus(active, ...caret); return; }
    const node = nodes.get(selected);
    const picker = el('select'); picker.setAttribute('aria-label','Review card');
    for (const n of projection.nodes) { const option=el('option',n.label); option.value=n.id; option.selected=n.id===selected; picker.append(option); }
    picker.addEventListener('change',()=>{ selected=picker.value; questionDraft=''; renderPanel(latest||projection, true); });
    const standingLine = el('p',standing(node.state),'review-standing'); standingLine.dataset.state = node.state;
    fragment.append(picker,standingLine);
    const form = el('form',undefined,'review-question-form');
    const label = el('label','Question about this card'); label.htmlFor='review-question';
    const input = el('textarea'); input.id='review-question'; input.rows=2; input.maxLength=1200; input.value=questionDraft;
    input.addEventListener('input',()=>{questionDraft=input.value;});
    const submit=el('button','Place ? mark');submit.type='submit';
    form.append(label,input,submit);
    form.addEventListener('submit',async event=>{
      event.preventDefault(); submit.disabled=true;
      try { await dispatch('mark_add',{target:selected,glyph:'?',text:input.value}); questionDraft=''; input.value=''; if (latest) renderPanel(latest, true); }
      catch {} finally { submit.disabled=false; }
    });
    fragment.append(form);
    const claims=projection.claims.filter(c=>c.about===selected);
    const claimIds=new Set(claims.map(c=>c.id));
    const marks=projection.marks.filter(m=>m.target===selected||claimIds.has(m.target));
    fragment.append(el('h3',`Marks (${marks.length})`));
    for (const mark of marks) {
      const article=el('article',undefined,'review-mark'); article.dataset.id=mark.id;
      article.dataset.state = mark.answer ? 'answered' : 'open';
      article.append(el('p',`${mark.glyph} ${mark.text || 'Question about this card'}`), el('small',`Asked by ${who(mark.created_by, mark.created_by_participant)}${mark.answer ? '' : ' · awaiting an answer'}`));
      // Same words as the agent read-back: "Answer by <name>: <text>", "you" for this person.
      if (mark.answer) { const answered = el('p', undefined, 'review-answer'); answered.append(el('strong', `Answer by ${who(mark.touched_by, mark.touched_by_participant)}:`), document.createTextNode(` ${mark.answer}`)); article.append(answered); }
      const answerForm=el('form');const answer=el('input');answer.dataset.answerFor=mark.id;answer.setAttribute('aria-label','Answer this mark');answer.placeholder='Answer in place';answer.maxLength=1200;answer.value=drafts.get(mark.id)||'';
      answer.addEventListener('input',()=>drafts.set(mark.id,answer.value));
      const save=el('button','Answer');save.type='submit';answerForm.append(answer,save);
      answerForm.addEventListener('submit',async event=>{event.preventDefault();save.disabled=true;try{await dispatch('mark_answer',{id:mark.id,answer:answer.value});drafts.delete(mark.id);answer.value='';if(latest)renderPanel(latest,true);}catch{}finally{save.disabled=false;}});
      article.append(answerForm);fragment.append(article);
    }
    fragment.append(el('h3',`Claims (${claims.length})`));
    for (const claim of claims) {
      const article=el('article',undefined,'review-claim');article.dataset.id=claim.id;article.dataset.verdict=claim.verdict;
      article.append(el('p',claim.text),el('small',`${claim.basis} · ${standing(claim.state)} · ${claim.verdict}`));
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
    // An agreed card offers to take the agreement back instead of agreeing twice.
    const agreed = node.status === 'agreed';
    const agree = button(agreed ? 'Withdraw agreement' : 'Mark card agreed', () => dispatch('card_place', {id:selected, status: agreed ? 'open' : 'agreed'}));
    agree.className = agreed ? 'review-agreed' : 'review-agree';
    if (agreed) fragment.append(el('p', 'This card is marked agreed.', 'review-agreed-note'));
    fragment.append(agree);
    const context=projection.context;
    if (context.current) fragment.append(el('h3','Shared question'),el('p',context.current.question));
    else if (context.heads.length>1) fragment.append(el('p','Shared scope needs reconciliation.'));
    fragment.append(el('p','Readable source supports the citation. It does not prove the claim’s wording or arrows.','review-boundary'));
    panel.replaceChildren(fragment);
    restoreFocus(active, ...caret);
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
        const badge=el('span',standing(node.state),'review-badge');badge.dataset.state=node.state;card.append(badge);
      }
      if (marks.length) {
        // One chip per card: open questions in amber, all answered in green.
        const open = marks.filter(m => !m.answer).length;
        const chip = el('span', open ? `? ${open}` : `✓ ${marks.length}`, 'review-mark-glyphs');
        chip.dataset.state = open ? 'open' : 'answered';
        chip.title = open ? `${open} question${open === 1 ? '' : 's'} awaiting an answer` : `${marks.length === 1 ? 'Question' : `All ${marks.length} questions`} answered`;
        chip.setAttribute('aria-label', chip.title);
        card.append(chip);
      }
    }
    renderPanel(projection);
  }
  const opened=event=>{if(selected!==event.detail.id)questionDraft='';selected=event.detail.id;if(latest){renderPanel(latest,true);document.getElementById('review-question')?.focus();}};
  document.addEventListener('samepage:review-open',opened);
  const unregister=application.registerRenderer('review',{render});
  return {dispose(){disposed=true;unregister();document.removeEventListener('samepage:review-open',opened);panel.replaceChildren();panel.hidden=true;}};
}
