pub const POINTER_BRIDGE_DOC: &str = concat!(
    "<!doctype html><meta charset=\"utf-8\">",
    "<style>html,body{margin:0;background:transparent}</style>",
    "<script>",
    "const AGUI_ROLES={A:\"link\",BUTTON:\"button\",INPUT:\"field\",TEXTAREA:\"field\",",
    "SELECT:\"field\",IMG:\"image\",UL:\"list\",OL:\"list\",LI:\"list item\",TABLE:\"table\",",
    "TR:\"row\",TD:\"cell\",TH:\"cell\",FORM:\"form\",NAV:\"navigation\",HEADER:\"header\",",
    "FOOTER:\"footer\",MAIN:\"main\",ASIDE:\"aside\",SECTION:\"section\",ARTICLE:\"article\",",
    "DIALOG:\"dialog\",FIELDSET:\"group\",LEGEND:\"legend\",LABEL:\"label\",P:\"paragraph\",",
    "H1:\"heading\",H2:\"heading\",H3:\"heading\",H4:\"heading\",H5:\"heading\",H6:\"heading\",",
    "CANVAS:\"canvas\",SVG:\"graphic\",VIDEO:\"video\",AUDIO:\"audio\",SUMMARY:\"summary\",",
    "DETAILS:\"disclosure\",PROGRESS:\"progress\",METER:\"meter\"};",
    "const aguiAttr=(el,name)=>el.getAttribute?(el.getAttribute(name)||\"\"):\"\";",
    "const aguiRole=(el)=>(aguiAttr(el,\"role\")||AGUI_ROLES[el.tagName]||\"\").trim().slice(0,120);",
    "const aguiName=(el)=>{",
    "const aria=aguiAttr(el,\"aria-label\")||aguiAttr(el,\"alt\")||aguiAttr(el,\"title\")",
    "||aguiAttr(el,\"placeholder\")||aguiAttr(el,\"aria-placeholder\");",
    "const text=aria||el.value||el.textContent||\"\";",
    "return String(text).replace(/\\s+/g,\" \").trim().slice(0,120);};",
    "const aguiPath=(event)=>{",
    // A click carries coordinates; a key press does not, and `detail` is 0 for
    // it. Asking elementsFromPoint about 0,0 in that case would answer about
    // the corner of the pane rather than about what they activated.
    "const positioned=event.detail>0&&(event.clientX>0||event.clientY>0);",
    "const stack=positioned?document.elementsFromPoint(event.clientX,event.clientY)",
    ":event.composedPath().filter((node)=>node&&node.nodeType===1);",
    "const out=[];",
    "for(const el of stack){",
    "if(el===document.body||el===document.documentElement)break;",
    "const level={role:aguiRole(el),name:aguiName(el),point:aguiAttr(el,\"data-point\")};",
    "if(!level.role&&!level.name&&!level.point)continue;",
    "out.push(level);",
    "if(out.length>=6)break;}",
    "return out;};",
    "document.addEventListener(\"click\",(event)=>{",
    "const hit=event.target.closest(\"[data-point]\");",
    "if(!hit&&event.target.closest(\"[data-quiet]\"))return;",
    // Clicking the background is not pointing at anything. Without this the
    // fallback reads `body.textContent`, which is every text node in the
    // document, including this script and the style above it, so a click on
    // empty space reported the bridge's own source back as what the person
    // touched. Nobody could act on that, and it landed in their note.
    "const bare=event.target===document.body||event.target===document.documentElement;",
    "if(!hit&&bare)return;",
    "const label=hit?hit.getAttribute(\"data-point\")",
    ":(event.target.textContent||event.target.tagName||\"\").trim().slice(0,80);",
    "if(label)parent.postMessage({aguiPoint:label,aguiPath:aguiPath(event)},\"*\");",
    "},true);</script>"
);
