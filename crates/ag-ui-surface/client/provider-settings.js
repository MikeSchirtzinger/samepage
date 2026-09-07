const template = document.createElement('template');
template.innerHTML = `
  <link rel="stylesheet" href="/_agui/provider-settings.css">
  <button class="trigger" type="button" aria-haspopup="dialog" aria-label="Choose agent or connection">
    <span class="dot"></span><span class="current">Loading agents…</span><span class="chevron">⌄</span>
  </button>
  <dialog aria-labelledby="provider-title">
    <div class="shell">
      <header>
        <div class="head"><div><h2 id="provider-title">Agents & connections</h2>
          <p class="sub">Choose a managed agent, API connection, or local model.</p></div>
          <button class="icon close" type="button" aria-label="Close">×</button></div>
        <input class="search" type="search" placeholder="Search agents, providers, or models…" autocomplete="off">
      </header>
      <main class="catalog"></main>
      <form class="editor" hidden autocomplete="off">
        <h3 class="editor-title">Add API connection</h3>
        <p>Save an OpenAI-compatible endpoint once, then choose it like any other agent.</p>
        <input class="connection-id" type="hidden">
        <div class="fields">
          <label><span>Connection name</span><input class="connection-label" type="text" required maxlength="80" placeholder="My OpenRouter"></label>
          <label><span>Base URL</span><input class="connection-base" type="url" required placeholder="https://openrouter.ai/api/v1"></label>
          <label><span>Model</span><div class="model-wrap"><input class="connection-model" type="text" required maxlength="200" placeholder="Search or paste a model ID" autocomplete="off"><div class="model-results" hidden></div></div></label>
          <label><span>API key</span><input class="connection-key" type="password" placeholder="Leave blank to keep the saved key"><small>Stored only in this app's local auth.json.</small></label>
          <label class="check"><input class="connection-vision" type="checkbox"><span>Send canvas images to vision-capable models</span></label>
        </div>
      </form>
      <footer><button class="danger delete" type="button" hidden>Delete</button><span class="status" role="status" aria-live="polite"></span>
        <button class="secondary back" type="button" hidden>Back</button>
        <button class="secondary add" type="button">Add API connection</button>
        <button class="primary save" type="button" hidden>Save connection</button></footer>
    </div>
  </dialog>`;

class AgUiProviderSettings extends HTMLElement {
  constructor() {
    super();
    this.attachShadow({ mode: 'open' }).append(template.content.cloneNode(true));
    const $ = (selector) => this.shadowRoot.querySelector(selector);
    this.ui = { trigger:$('.trigger'), dot:$('.trigger .dot'), current:$('.current'), dialog:$('dialog'),
      close:$('.close'), search:$('.search'), catalog:$('.catalog'), editor:$('.editor'), title:$('.editor-title'),
      id:$('.connection-id'), label:$('.connection-label'), base:$('.connection-base'), model:$('.connection-model'),
      key:$('.connection-key'), vision:$('.connection-vision'), results:$('.model-results'), status:$('.status'),
      add:$('.add'), back:$('.back'), save:$('.save'), delete:$('.delete') };
    this.data = { providers: [] };
    this.modelTimer = 0;
  }

  connectedCallback() {
    if (this.connected) return;
    this.connected = true;
    const u = this.ui;
    u.trigger.addEventListener('click', () => this.open());
    u.close.addEventListener('click', () => u.dialog.close());
    u.dialog.addEventListener('click', (event) => { if (event.target === u.dialog) u.dialog.close(); });
    u.search.addEventListener('input', () => this.render());
    u.add.addEventListener('click', () => this.edit());
    u.back.addEventListener('click', () => this.showCatalog());
    u.save.addEventListener('click', () => this.save());
    u.delete.addEventListener('click', () => this.remove());
    u.model.addEventListener('input', () => this.queueModelSearch());
    u.base.addEventListener('input', () => this.queueModelSearch());
    this.refresh();
  }

  provider(id) { return this.data.providers.find((provider) => provider.id === id); }
  async refresh() {
    try {
      const response = await fetch('/auth');
      if (!response.ok) throw new Error(`settings returned ${response.status}`);
      this.data = await response.json();
      this.paintTrigger();
      this.render();
      this.dispatchEvent(new CustomEvent('provider-state', { bubbles:true, detail:this.data }));
    } catch (error) { this.setStatus(`Could not load agents: ${error.message}`, true); }
    return this.data;
  }

  open() {
    this.refresh();
    this.showCatalog();
    if (!this.ui.dialog.open) this.ui.dialog.showModal();
    requestAnimationFrame(() => this.ui.search.focus());
  }

  paintTrigger() {
    const active = this.provider(this.data.current);
    this.ui.current.textContent = active?.label || 'Choose agent';
    this.ui.dot.className = `dot ${this.data.error ? 'error' : this.data.ready ? 'ready' : this.data.warming ? 'warming' : ''}`;
    this.ui.trigger.title = this.data.error || (this.data.ready ? 'Ready' : this.data.warming ? 'Connecting…' : 'Not ready');
  }

  render() {
    const query = this.ui.search.value.trim().toLowerCase();
    const groups = [
      ['managed','Managed agents'], ['connection','Your API connections'], ['preset','Configured endpoints']
    ];
    this.ui.catalog.replaceChildren();
    let count = 0;
    for (const [key, title] of groups) {
      const providers = this.data.providers.filter((provider) => provider.group === key &&
        (!query || `${provider.label} ${provider.model || ''} ${provider.base_url || ''}`.toLowerCase().includes(query)));
      if (!providers.length) continue;
      const section = document.createElement('section'); section.className = 'section';
      const heading = document.createElement('div'); heading.className = 'section-title'; heading.textContent = title;
      section.append(heading);
      for (const provider of providers) section.append(this.row(provider));
      this.ui.catalog.append(section); count += providers.length;
    }
    if (!count) { const empty=document.createElement('div'); empty.className='empty'; empty.textContent='No matching agents or connections.'; this.ui.catalog.append(empty); }
  }

  row(provider) {
    const row=document.createElement('div'); row.className='row';
    const main=document.createElement('button'); main.type='button'; main.className='row-main';
    const active=provider.id===this.data.current;
    const state=active && this.data.error ? 'error' : active && this.data.ready ? 'ready' : active && this.data.warming ? 'warming' : provider.configured ? 'configured' : '';
    const dot=document.createElement('span'); dot.className=`dot ${state}`;
    const label=document.createElement('span'); label.className='label'; label.textContent=provider.label;
    const meta=document.createElement('span'); meta.className='meta';
    const detail=provider.model || provider.note || '';
    const status=active
      ? (this.data.error ? 'Failed' : this.data.ready ? 'Active · request proven' : this.data.warming ? 'Connecting' : 'Not ready')
      : provider.group==='managed' ? `Available · ${provider.adapter}` : provider.configured ? 'Configured' : 'Needs key';
    meta.textContent = `${status}${detail ? ` · ${detail}` : ''}`;
    main.append(dot,label,meta); main.addEventListener('click',()=>this.select(provider)); row.append(main);
    if (provider.editable) { const edit=document.createElement('button'); edit.type='button'; edit.className='edit'; edit.textContent='Edit'; edit.setAttribute('aria-label',`Edit ${provider.label}`); edit.addEventListener('click',()=>this.edit(provider)); row.append(edit); }
    return row;
  }

  async select(provider) {
    this.setStatus(`Connecting to ${provider.label}…`); this.ui.catalog.inert=true;
    try {
      const response=await fetch('/provider',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({id:provider.id})});
      const result=await response.json(); if (!response.ok || !result.ok) throw new Error(result.error || `switch failed (${response.status})`);
      await this.refresh(); this.ui.dialog.close();
      this.dispatchEvent(new CustomEvent('provider-change',{bubbles:true,detail:{provider,...result,snapshot:this.data}}));
    } catch(error) { this.setStatus(error.message,true); }
    finally { this.ui.catalog.inert=false; }
  }

  edit(provider=null) {
    const userConnection=provider?.group==='connection';
    this.ui.catalog.hidden=true; this.ui.editor.hidden=false; this.ui.search.hidden=true;
    this.ui.add.hidden=true; this.ui.back.hidden=false; this.ui.save.hidden=false;
    this.ui.delete.hidden=!userConnection; this.ui.title.textContent=provider ? `Edit ${provider.label}` : 'Add API connection';
    this.ui.id.value=userConnection ? provider.id : '';
    this.ui.label.value=provider?.label || '';
    this.ui.label.disabled=!!provider && !userConnection;
    this.ui.base.value=provider?.base_url || (provider?.id==='openai' ? this.data.byok?.base_url || '' : '');
    this.ui.base.disabled=!!provider && !userConnection && !provider.byok;
    this.ui.model.value=provider?.model || (provider?.id==='openai' ? this.data.byok?.model || '' : '');
    this.ui.key.value=''; this.ui.key.placeholder=provider?.source==='stored' ? 'Saved. Leave blank to keep it' : 'Paste API key';
    this.ui.vision.checked=!!provider?.vision; this.ui.vision.disabled=!!provider && !userConnection;
    this.ui.results.hidden=true; this.editing=provider;
    this.setStatus(provider?.group==='preset' ? 'Saving updates this configured endpoint.' : '');
    requestAnimationFrame(()=>this.ui.label.focus());
  }

  showCatalog() {
    this.editing=null; this.ui.catalog.hidden=false; this.ui.editor.hidden=true; this.ui.search.hidden=false;
    this.ui.add.hidden=false; this.ui.back.hidden=true; this.ui.save.hidden=true; this.ui.delete.hidden=true;
    this.ui.results.hidden=true; this.setStatus(''); this.render();
  }

  async save() {
    if (!this.ui.editor.reportValidity()) return;
    const provider=this.editing; const isSaved=provider?.group==='connection';
    const isPreset=provider && !isSaved;
    let endpoint='/connections'; let body;
    if (isPreset) {
      endpoint='/auth'; body={provider:provider.id,model:this.ui.model.value.trim()};
      if (provider.byok) body.base_url=this.ui.base.value.trim();
      if (this.ui.key.value.trim()) body.api_key=this.ui.key.value.trim();
    } else {
      body={action:'save',id:this.ui.id.value,label:this.ui.label.value.trim(),base_url:this.ui.base.value.trim(),
        model:this.ui.model.value.trim(),vision:this.ui.vision.checked};
      if (this.ui.key.value.trim()) body.api_key=this.ui.key.value.trim();
    }
    this.ui.save.disabled=true; this.setStatus('Saving connection…');
    try {
      const response=await fetch(endpoint,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
      const result=await response.json(); if (!response.ok || result.ok===false) throw new Error(result.error || `save failed (${response.status})`);
      this.data=result; this.paintTrigger(); this.showCatalog(); this.render(); this.setStatus('Saved.');
      this.dispatchEvent(new CustomEvent('provider-state',{bubbles:true,detail:this.data}));
    } catch(error) { this.setStatus(error.message,true); }
    finally { this.ui.save.disabled=false; }
  }

  async remove() {
    const id=this.ui.id.value; if (!id) return;
    this.ui.delete.disabled=true; this.setStatus('Deleting connection…');
    try {
      const response=await fetch('/connections',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({action:'delete',id})});
      const result=await response.json(); if (!response.ok || !result.ok) throw new Error(result.error || `delete failed (${response.status})`);
      this.data=result; this.paintTrigger(); this.showCatalog(); this.render(); this.setStatus('Deleted.');
    } catch(error) { this.setStatus(error.message,true); }
    finally { this.ui.delete.disabled=false; }
  }

  queueModelSearch() {
    clearTimeout(this.modelTimer);
    if (!this.ui.base.value.toLowerCase().includes('openrouter.ai')) { this.ui.results.hidden=true; return; }
    this.modelTimer=setTimeout(()=>this.searchModels(),180);
  }

  async searchModels() {
    const query=this.ui.model.value.trim();
    try {
      const response=await fetch(`/models/openrouter?q=${encodeURIComponent(query)}`); const result=await response.json();
      if (!response.ok || !result.ok) throw new Error(result.error || 'model search failed');
      this.ui.results.replaceChildren();
      for (const model of result.models.slice(0,30)) {
        const button=document.createElement('button'); button.type='button'; button.className='model-option';
        const id=document.createElement('span'); id.className='model-id'; id.textContent=model.id;
        const name=document.createElement('span'); name.className='model-name'; name.textContent=model.name || '';
        button.append(id,name); button.addEventListener('click',()=>{this.ui.model.value=model.id;this.ui.results.hidden=true;}); this.ui.results.append(button);
      }
      this.ui.results.hidden=!result.models.length;
    } catch(error) { this.setStatus(error.message,true); this.ui.results.hidden=true; }
  }

  setStatus(message,error=false) { this.ui.status.textContent=message; this.ui.status.classList.toggle('error',error); }
}

if (!customElements.get('agui-provider-settings')) customElements.define('agui-provider-settings',AgUiProviderSettings);
