// Kernel presentation only: never loads a capability or writes the document.
// Keys come from verified ABI exports. Swatches retain the actual classes and
// copy live computed paint, including ancestor-scoped CSS and authored SVG.
const paint = ['color','background-color','background-image','border-top-color','border-right-color','border-bottom-color','border-left-color','border-top-style','border-right-style','border-bottom-style','border-left-style','border-top-width','border-right-width','border-bottom-width','border-left-width','border-radius','outline-color','outline-style','outline-width','fill','stroke','stroke-width','stroke-dasharray','opacity','font-family','font-size','font-weight','text-decoration-line'];
const ns = 'http://www.w3.org/2000/svg';
const el = (tag, cls, text) => { const n=document.createElement(tag); if(cls)n.className=cls;if(text!==undefined)n.textContent=text;return n; };
export function createLegend({mount, read, decode}) {
  const content=mount.querySelector('#legend-content');
  // A swatch must never become a second .map-card or .pane in the renderers'
  // document queries. Shadow DOM isolates the legend from those selectors.
  const root=content.attachShadow({mode:'open'});
  const all=mount.querySelector('#legend-all');
  let queued=false;
  const schedule=()=>{if(!queued){queued=true;requestAnimationFrame(()=>{queued=false;render();});}};
  const observer=new MutationObserver(schedule);
  // Comparison dialogs and the activity drawer can live outside main. Shadow
  // DOM keeps our own swatch refreshes outside this observer's subtree.
  observer.observe(document.body,{subtree:true,childList:true,attributes:true,characterData:true});
  observer.observe(document.head,{subtree:true,childList:true});
  all.addEventListener('change',schedule);
  document.addEventListener('agui:extension-loaded',schedule);
  document.addEventListener('agui:extension-error',schedule);
  function candidates(entry) {
    if(!entry.class)return [];
    const selector=entry.class.split(/\s+/).map(c=>'.'+CSS.escape(c)).join('');
    const nodes=[...document.querySelectorAll(selector)].filter(n=>{
      const r=n.getBoundingClientRect();if(!r.width&&!r.height)return false;
      // Chromium can retain geometry for content in a closed details element.
      // Only its summary is painted until the person opens it.
      for(let p=n.parentElement;p;p=p.parentElement){
        if(p.tagName==='DETAILS'&&!p.open&&!p.querySelector(':scope > summary')?.contains(n))return false;
      }
      return true;
    });
    if(entry.attribute==='element'||entry.attribute==='state_role')return nodes;
    const key=entry.attribute.replace(/_/g,'-');
    return nodes.filter(n=>n.getAttribute('data-'+key)===entry.value ||
      (entry.attribute==='tier'&&n.classList.contains('source-tier-'+entry.value)));
  }
  function copyPaint(source, target) {
    const style=getComputedStyle(source);
    for(const property of paint)target.style.setProperty(property,style.getPropertyValue(property));
  }
  function swatch(entry,sample) {
    const box=el('span','legend-swatch');box.setAttribute('aria-hidden','true');
    if(!sample){const n=el('span',entry.class,entry.symbol||entry.value);n.dataset.sample='absent';if(entry.attribute!=='element')n.setAttribute('data-'+entry.attribute.replace(/_/g,'-'),entry.value);box.append(n);return box;}
    if(sample.namespaceURI===ns){
      const svg=document.createElementNS(ns,'svg');svg.setAttribute('width','80');svg.setAttribute('height','36');
      const clone=sample.cloneNode(true);
      const originals=[sample,...sample.querySelectorAll('*')],clones=[clone,...clone.querySelectorAll('*')];
      for(let i=0;i<clones.length;i++){
        const n=clones[i];copyPaint(originals[i],n);
        for(const a of [...n.attributes])if(a.name==='id'||a.name.startsWith('on')||['href','src','tabindex'].includes(a.name))n.removeAttribute(a.name);
        n.style.pointerEvents='none';
      }
      const b=sample.getBBox();svg.setAttribute('viewBox',`${b.x-4} ${b.y-4} ${Math.max(12,b.width+8)} ${Math.max(12,b.height+8)}`);svg.append(clone);box.append(svg);
    }else{
      const clone=el('span',entry.class,entry.symbol||entry.value);
      for(const [key,value]of Object.entries(sample.dataset))if(!['id','paneId'].includes(key))clone.dataset[key]=value;
      copyPaint(sample,clone);
      clone.style.cssText+=';position:static!important;display:inline-block!important;min-height:0!important;width:auto!important;max-width:80px;padding:3px 6px!important;margin:0!important;font-size:10px!important;box-shadow:none!important;';
      box.append(clone);
    }
    return box;
  }
  // Rendered only while the popover is open; it re-reads the map when opened.
  document.addEventListener('samepage:legend-open',schedule);
  function render() {
    if(mount.hidden)return;
    const {modules,states,view,failed=[]}=read();
    const fragment=document.createDocumentFragment();
    const style=el('style');style.textContent=[...document.styleSheets].map(sheet=>{try{return [...sheet.cssRules].map(rule=>rule.cssText).join('\n');}catch{return '';}}).join('\n')+` :host{display:block;color:#eaf0f5;font:12px/1.4 system-ui}h3{margin:15px 0 6px;font-size:13px;text-transform:capitalize}ul{padding:0;margin:0;list-style:none}.legend-key{display:grid;grid-template-columns:84px 1fr;gap:9px;align-items:center;padding:7px 0;border-bottom:1px solid #345066}.legend-swatch{display:flex;align-items:center;min-height:36px;overflow:hidden}.legend-swatch svg{overflow:visible}.legend-swatch>[data-sample="absent"]{position:static!important;display:inline-block!important;min-height:0!important;width:auto!important;padding:3px 6px!important;margin:0!important;font-size:10px!important;box-shadow:none!important}.legend-label{overflow-wrap:anywhere}.legend-symbol{display:block;font-size:10px;color:#a9bcca}.legend-absent{color:#a9bcca;font-size:10px}p{color:#a9bcca}`;fragment.append(style);
    for(const {id,status} of states){
      if(status!=='ready'||failed.includes(id)||!modules.has(id))continue;
      const section=el('section');section.dataset.capability=id;
      section.append(el('h3',null,id));
      try{
        const entries=decode(modules.get(id).legend(JSON.stringify(view)),`${id}.legend`);
        const list=el('ul');
        for(const entry of entries){
          if(entry.capability!==id)throw Error('legend entry has a foreign capability');
          const sample=candidates(entry)[0];
          if(!sample&&!all.checked)continue;
          const row=el('li','legend-key');Object.assign(row.dataset,{capability:id,attribute:entry.attribute,value:entry.value,present:String(!!sample),class:entry.class||''});
          const label=el('span','legend-label',entry.label);label.append(el('span','legend-symbol',entry.symbol||entry.value));
          if(!sample)label.append(el('span','legend-absent','Not visible in this view'));
          row.append(swatch(entry,sample),label);list.append(row);
        }
        section.append(list);
        if(!entries.length)section.append(el('p',null,id==='trace'?'No map keys. Activity uses its own drawer.':'No visual keys in this view.'));
        else if(!list.children.length)section.append(el('p',null,'No keys visible here. Use Show all for this loaded capability.'));
      }catch(error){section.replaceChildren(el('p',null,`${id} legend unavailable: ${error.message}`));section.dataset.extensionState='failed';}
      fragment.append(section);
    }
    root.replaceChildren(fragment);
  }
  schedule();
  return {refresh:schedule,dispose(){observer.disconnect();all.removeEventListener('change',schedule);document.removeEventListener('agui:extension-loaded',schedule);document.removeEventListener('agui:extension-error',schedule);}};
}
