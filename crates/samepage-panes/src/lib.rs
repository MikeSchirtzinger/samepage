//! Portable pane content and layout. Host stamps each independent aspect so
//! dragging cannot take credit for content, and a note cannot take credit for a mark.
pub mod diagram;
pub mod layout;
pub mod model;
pub mod view;

use layout::{Placement, Sited, Spot};
use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CallerKind, CapError, Capability, Change, Delta,
    Descriptor, Effect, LegendEntry, Op, Origin, Plan, Record, Scalar, Target, Trigger, Value,
    View,
};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
pub const ID: &str = "panes";
pub const VERSION: &str = "0.1.0";
pub const KINDS: &[&str] = &[
    "pane",
    "pane-layout",
    "pane-annotation",
    "panes-settings",
    "panes-history",
];
fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("invalid", message)
}
fn str_<'a>(r: &'a Record, key: &str) -> &'a str {
    r.props.get(key).and_then(Scalar::as_str).unwrap_or("")
}
fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, CapError> {
    serde_json::from_str(raw).map_err(|e| invalid(e.to_string()))
}
// JSON strings stored in scalar properties must ignore serde_json's
// feature-unified map ordering, which differs between the app and leaf WASM.
pub fn encode_json(value: &Json) -> String {
    match value {
        Json::Object(object) => {
            let sorted: BTreeMap<_, _> = object.iter().collect();
            format!(
                "{{{}}}",
                sorted
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        encode_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Json::Array(array) => format!(
            "[{}]",
            array.iter().map(encode_json).collect::<Vec<_>>().join(",")
        ),
        Json::Number(number) if number.is_f64() => {
            let f = number.as_f64().unwrap();
            // JSON.stringify writes integral floats as integers. Match that
            // lexical form without rounding non-integral or large values.
            if f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0 {
                format!("{f:.0}")
            } else {
                number.to_string()
            }
        }
        _ => value.to_string(),
    }
}
fn action(
    name: &str,
    alias: Option<&str>,
    audience: Audience,
    effect: Effect,
    required: &[&str],
    props: Json,
    description: &str,
) -> ActionDecl {
    let mut a = ActionDecl::new(name, audience, effect, description);
    if let Some(alias) = alias {
        a = a.alias(alias);
    }
    a.input_schema = json!({"type":"object","required":required,"properties":props,"additionalProperties":false});
    a
}
/// The typed contract of a pane view: one node and, through `children`, a
/// tree of them. Recursion is stated in words; each node's `kind` is typed.
pub fn view_schema() -> serde_json::Value {
    let kinds = [
        "stack", "row", "deck", "heading", "text", "code", "list", "kv", "table", "badge",
        "divider", "button", "field", "link", "image", "source", "options", "embed", "html",
        "diagram",
    ];
    let tone = json!({"enum":["neutral","muted","strong","accent","good","warn","bad"]});
    let child = json!({"type":"object","required":["kind"],"properties":{"kind":{"enum":kinds}},"description":"A nested view node, same shape as view."});
    json!({
        "type":"object",
        "required":["kind"],
        "description":"One view node chosen by kind. Containers: stack (vertical) and row (horizontal) take children; deck shows one child at a time with optional titles. Content: heading {text, level 1-3}, text {text, tone}, code {text, lang}, list {items, ordered}, kv {items: [{label, value}]}, table {columns, rows}, badge {text, tone}, divider. Interaction: button {label, ask, tone} sends ask to you as if the person typed it, with any field values in the pane; field {key, label, placeholder, multiline}; link {label, url}. Host-resolved: source {path, from, to} is re-read from the file on every read; options {filter} lists what the workspace can run. Escape hatches: embed {url, height}, html {html, height} in an isolated sandbox. diagram {nodes: [{id, label, tone}], edges: [{from, to, label, arrow}], direction right or down, caption} is laid out by the host.",
        "properties":{
            "kind":{"enum":kinds},
            "children":{"type":"array","minItems":1,"items":child},
            "gap":{"type":"integer","minimum":0,"maximum":255},
            "wrap":{"type":"boolean"},
            "titles":{"type":"array","items":{"type":"string"}},
            "text":{"type":"string"},
            "level":{"type":"integer","minimum":1,"maximum":3},
            "tone":tone,
            "lang":{"type":"string"},
            "items":{"type":"array"},
            "ordered":{"type":"boolean"},
            "columns":{"type":"array","items":{"type":"string"}},
            "rows":{"type":"array","items":{"type":"array","items":{"type":"string"}}},
            "label":{"type":"string"},
            "ask":{"type":"string"},
            "key":{"type":"string"},
            "placeholder":{"type":"string"},
            "multiline":{"type":"boolean"},
            "url":{"type":"string"},
            "src":{"type":"string"},
            "alt":{"type":"string"},
            "path":{"type":"string"},
            "from":{"type":"integer","minimum":1},
            "to":{"type":"integer","minimum":1},
            "filter":{"type":"string"},
            "height":{"type":"integer","minimum":0},
            "html":{"type":"string"},
            "nodes":{"type":"array","items":{"type":"object","required":["id"]}},
            "edges":{"type":"array","items":{"type":"object","required":["from","to"]}},
            "direction":{"type":"string","description":"right (the default) or down."},
            "caption":{"type":"string"}
        }
    })
}
pub fn descriptor() -> Descriptor {
    let place = json!({"type":"string","description":"start, end, or a relation to another pane: \"right of: <pane id>\", \"left of: <id>\", \"below: <id>\", \"above: <id>\", \"near: <id>\"."});
    let size = json!({"type":"string","description":"small, medium, wide, tall or large."});
    let adjustment = json!({"id":{"type":"string"},"place":place.clone(),"size":size.clone(),"pinned":{"type":"boolean"}});
    let mut d=Descriptor::new(ID,VERSION,Artifact::Wasm)
 .owns("pane", &["pane_id","title","tree","historical"], "Typed content and its independent host byline")
 .owns("pane-layout", &["pane","spot","stack","pinned","method","historical"], "Shared geometry, distinct from content authorship")
 .owns("pane-annotation", &["pane","slot","value","historical"], "Independent human mark, note and pointed path bylines")
 .owns("panes-settings", &["intent","theme"], "Document intent and appearance")
 .owns("panes-history", &["original","migrated"], "Immutable legacy document, including every historical field and log")
 .action(action("panes_read",Some("read_room"),Audience::Agent,Effect::Query,&[],json!({}),"Read every pane: title, author, content summary, where each sits relative to the others, a person's marks and notes, and what changed since your last read. Read before writing; pass its revision to pane_put."))
 .action(action("panes_present",None,Audience::Human,Effect::Query,&[],json!({}),"Resolve native source and workspace facts for the human renderer."))
 .action(action("panes_await",Some("await_room"),Audience::Agent,Effect::Query,&[],json!({"seconds":{"type":"integer","minimum":0,"maximum":60}}),"Wait for unread pane changes from other participants."))
 .action(action("pane_put",Some("room_put_pane"),Audience::Both,Effect::Mutate,&["expected_revision","id"],json!({"expected_revision":{"type":"string","description":"The revision from your last panes_read or page_read."},"id":{"type":"string","description":"A short pane slug such as status; reusing one rewrites that pane."},"title":{"type":"string"},"view":view_schema(),"place":place.clone(),"size":size.clone(),"pinned":{"type":"boolean"}}),"Create or rewrite one pane: a titled panel whose view is a tree of typed nodes (text, lists, tables, code, a source excerpt, buttons that send you a request, a diagram), never raw markup outside an html node. Agents place panes by relation (place) and named size; a person drags the exact geometry. Read panes_read first and pass its revision."))
 .action(action("pane_remove",Some("room_remove_pane"),Audience::Both,Effect::Mutate,&["expected_revision","id"],json!({"expected_revision":{"type":"string"},"id":{"type":"string"}}),"Remove one unpinned pane with its layout and annotations. A pinned pane must be unpinned first."))
 .action(action("panes_arrange",Some("room_arrange"),Audience::Both,Effect::Mutate,&["expected_revision","panes"],json!({"expected_revision":{"type":"string"},"panes":{"type":"array","minItems":1,"maxItems":24,"items":{"type":"object","required":["id"],"properties":adjustment,"additionalProperties":false}}}),"Move or resize several panes at once by relation and named size, in order; later entries can refer to panes placed earlier. Pinning keeps a pane where a person put it."))
 .action(action("panes_configure",Some("room_configure"),Audience::Both,Effect::Mutate,&["expected_revision"],json!({"expected_revision":{"type":"string"},"intent":{"type":"string","description":"What these panes are for, in a sentence; read back to every participant."},"theme":{"type":"object","additionalProperties":false,"properties":{"accent":{"type":"string","pattern":"^#[0-9A-Fa-f]{6}$","description":"Accent colour as #rrggbb."},"surface":{"enum":["ink","slate","warm","paper"]},"density":{"enum":["compact","cozy","roomy"]},"radius":{"type":"integer","minimum":0,"maximum":24},"scale":{"type":"number","minimum":0.8,"maximum":1.4,"description":"Text scale."}}}}),"Set what the panes are for (intent) and their appearance from fixed tokens: accent colour, surface, density, corner radius and text scale. Only the fields you pass change."))
 .action(action("pane_annotate",Some("room_annotate_pane"),Audience::Human,Effect::Mutate,&["expected_revision","id"],json!({"expected_revision":{"type":"string"},"id":{"type":"string"},"mark":{"enum":["none","question","important","agree","disagree"]},"note":{"type":"string"},"pointed":{"type":"string"},"pointed_path":{"type":"array"}}),"Place a human mark, note or pointed referent, preserving separate bylines."))
 .action(action("panes_import",None,Audience::Both,Effect::Mutate,&["expected_revision","document"],json!({"expected_revision":{"type":"string"},"document":{"type":"object"}}),"Import the contents of a legacy room JSON copy once; historical authors remain historical."))
 .trigger(Trigger::kind("pane")).trigger(Trigger::route("panes"));
    for a in &d.actions {
        d.triggers.push(Trigger::tool(&a.name));
        for alias in &a.aliases {
            d.triggers.push(Trigger::tool(alias));
        }
    }
    d
}
pub struct Panes;
fn set(id: &str, props: BTreeMap<String, Value>) -> Op {
    Op::Set {
        target: Target::Id(id.into()),
        props,
    }
}
fn create(reference: &str, kind: &str, props: BTreeMap<String, Value>) -> Op {
    Op::Create {
        reference: reference.into(),
        kind: kind.into(),
        props,
    }
}
fn delete(id: &str) -> Op {
    Op::Delete {
        target: Target::Id(id.into()),
    }
}
fn pane<'a>(v: &'a View, slug: &str) -> Result<&'a Record, CapError> {
    v.records()
        .find(|r| r.kind == "pane" && (str_(r, "pane_id") == slug || r.id == slug))
        .ok_or_else(|| invalid(format!("no pane {slug:?}")))
}
fn aspect<'a>(v: &'a View, id: &str, kind: &str, slot: &str) -> Option<&'a Record> {
    v.records().find(|r| {
        r.kind == kind && str_(r, "pane") == id && (slot.is_empty() || str_(r, "slot") == slot)
    })
}
fn spot(v: &View, id: &str) -> Result<Spot, CapError> {
    parse(str_(
        aspect(v, id, "pane-layout", "").ok_or_else(|| invalid("pane has no layout"))?,
        "spot",
    ))
}
fn sited<'a>(v: &'a View, except: &str) -> Result<Vec<Sited<'a>>, CapError> {
    v.records()
        .filter(|r| r.kind == "pane" && r.id != except)
        .map(|r| {
            Ok(Sited {
                id: str_(r, "pane_id"),
                title: str_(r, "title"),
                spot: spot(v, &r.id)?,
            })
        })
        .collect()
}
fn arg<'a>(a: &'a Json, key: &str) -> &'a str {
    a.get(key).and_then(Json::as_str).unwrap_or("")
}
fn update_aspect(v: &View, id: &str, kind: &str, slot: &str, props: BTreeMap<String, Value>) -> Op {
    if let Some(r) = aspect(v, id, kind, slot) {
        set(&r.id, props)
    } else {
        let mut props = props;
        props.insert("pane".into(), id.into());
        if !slot.is_empty() {
            props.insert("slot".into(), slot.into());
        }
        create(&format!("{id}-{slot}-{kind}"), kind, props)
    }
}
impl Capability for Panes {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, v: &View) -> Result<Json, CapError> {
        projection(v, &BTreeMap::new())
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            (
                "element",
                "heading",
                "Heading pane content",
                "n-heading",
                "H",
            ),
            ("element", "text", "Text pane content", "n-text", "T"),
            ("element", "code", "Code pane content", "n-code", "{}"),
            ("element", "list", "List pane content", "n-list", "≡"),
            ("element", "kv", "Kv pane content", "n-kv", "K:V"),
            ("element", "table", "Table pane content", "n-table", "▦"),
            (
                "element",
                "diagram",
                "Diagram pane content",
                "n-diagram",
                "→",
            ),
            ("element", "badge", "Badge pane content", "n-badge", "●"),
            (
                "element",
                "divider",
                "Divider pane content",
                "n-divider",
                "—",
            ),
            ("element", "button", "Button pane content", "n-button", "↵"),
            ("element", "field", "Field pane content", "n-field", "⌨"),
            ("element", "link", "Link pane content", "n-link", "↗"),
            ("element", "image", "Image pane content", "n-image", "▣"),
            ("element", "source", "Source pane content", "n-source", "{}"),
            (
                "element",
                "options",
                "Options pane content",
                "n-options",
                "⋯",
            ),
            ("element", "embed", "Embed pane content", "n-embed", "↗"),
            ("element", "html", "Html pane content", "n-html", "HTML"),
            ("element", "stack", "Stack pane content", "n-stack", "≡"),
            ("element", "row", "Row pane content", "n-row", "⋯"),
            ("element", "deck", "Deck pane content", "n-deck", "‹ ›"),
        ]
        .into_iter()
        .map(|(attribute, value, label, class, symbol)| LegendEntry {
            capability: ID.into(),
            attribute: attribute.into(),
            value: value.into(),
            label: label.into(),
            class: Some(class.into()),
            symbol: Some(symbol.into()),
        })
        .collect())
    }
    fn digest(&self, v: &View) -> Result<Json, CapError> {
        self.project(v)
    }
    fn plan(&self, c: &Call, v: &View) -> Result<Plan, CapError> {
        let d = descriptor();
        let declaration = d
            .actions
            .iter()
            .find(|a| a.name == c.action)
            .ok_or_else(|| CapError::new("unknown_action", &c.action))?;
        let args = c
            .args
            .as_object()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        for key in args.keys() {
            if declaration.input_schema["properties"].get(key).is_none()
                && !(c.action == "pane_put" && ["spot", "raise"].contains(&key.as_str()))
            {
                return Err(invalid(format!("unknown argument {key}")));
            }
        }
        if declaration.effect == Effect::Mutate && arg(&c.args, "expected_revision") != v.revision()
        {
            return Err(invalid("revision conflict: call panes_read and retry"));
        }
        let a = &c.args;
        if c.action == "panes_read" || c.action == "panes_await" || c.action == "panes_present" {
            return Ok(Plan {
                ops: vec![],
                readback: describe(v)?,
            });
        }
        let mut ops = vec![];
        let readback = match c.action.as_str() {
            "pane_put" => {
                let slug = model::pane_id(arg(a, "id")).map_err(invalid)?;
                let old = pane(v, &slug).ok();
                if old.is_none() && v.records().filter(|r| r.kind == "pane").count() >= 24 {
                    return Err(invalid("the room already holds 24 panes"));
                }
                if let Some(tree) = a.get("view") {
                    let node: view::Node =
                        serde_json::from_value(tree.clone()).map_err(|e| invalid(e.to_string()))?;
                    view::validate(&node).map_err(invalid)?;
                    let title =
                        model::checked("title", arg(a, "title"), 1, 120).map_err(invalid)?;
                    let props = [
                        ("pane_id".into(), slug.clone().into()),
                        ("title".into(), title.into()),
                        ("tree".into(), serde_json::to_string(&node).unwrap().into()),
                        ("historical".into(), "".into()),
                    ]
                    .into();
                    ops.push(match old {
                        Some(r) => set(&r.id, props),
                        None => create("pane", "pane", props),
                    });
                } else if old.is_none() || a.get("title").is_some() {
                    return Err(invalid("pane content needs both title and view"));
                }
                let current = old.map(|r| spot(v, &r.id)).transpose()?;
                let size = a
                    .get("size")
                    .map(|_| model::parse_size(arg(a, "size")).map_err(invalid))
                    .transpose()?;
                let place = a
                    .get("place")
                    .map(|_| Placement::parse(arg(a, "place")).map_err(invalid))
                    .transpose()?;
                let requested = a
                    .get("spot")
                    .map(|raw| model::parse_spot(raw).map_err(invalid))
                    .transpose()?;
                if requested.is_some() && (place.is_some() || size.is_some()) {
                    return Err(invalid("spot cannot be combined with place or size"));
                }
                let size = match current {
                    Some(s) => (s.w, s.h),
                    None => size.unwrap_or((layout::DEFAULT_W, layout::DEFAULT_H)),
                };
                let next = match requested {
                    Some(s) => s,
                    None => match (current, place.as_ref()) {
                        (Some(s), None) => s,
                        _ => layout::resolve(
                            place.as_ref(),
                            &sited(v, old.map(|r| r.id.as_str()).unwrap_or(""))?,
                            size,
                        )
                        .map_err(invalid)?,
                    },
                };
                let front = v
                    .records()
                    .filter(|r| r.kind == "pane-layout")
                    .filter_map(|r| r.props.get("stack").and_then(Scalar::as_f64))
                    .fold(0.0, f64::max);
                if let Some(r) = old {
                    if requested.is_some()
                        || place.is_some()
                        || a.get("raise").is_some()
                        || a.get("pinned").is_some()
                    {
                        let layout = aspect(v, &r.id, "pane-layout", "").unwrap();
                        let mut props = BTreeMap::new();
                        props.insert(
                            "method".into(),
                            if requested.is_some() {
                                "rectangle"
                            } else if a.get("raise").is_some() {
                                "raise"
                            } else {
                                "relational"
                            }
                            .into(),
                        );
                        if next != current.unwrap() {
                            props.insert(
                                "spot".into(),
                                serde_json::to_string(&next).unwrap().into(),
                            );
                        }
                        if a["raise"] == true
                            && layout
                                .props
                                .get("stack")
                                .and_then(Scalar::as_f64)
                                .unwrap_or(0.0)
                                < front
                        {
                            props.insert("stack".into(), (front + 1.0).into());
                        }
                        if let Some(pinned) = a["pinned"].as_bool() {
                            props.insert("pinned".into(), pinned.into());
                        }
                        if !props.is_empty() {
                            props.insert("historical".into(), "".into());
                            ops.push(set(&layout.id, props));
                        }
                    }
                } else {
                    ops.push(create(
                        "layout",
                        "pane-layout",
                        [
                            ("pane".into(), Value::reference("pane")),
                            ("spot".into(), serde_json::to_string(&next).unwrap().into()),
                            ("stack".into(), (front + 1.0).into()),
                            (
                                "pinned".into(),
                                a["pinned"].as_bool().unwrap_or(false).into(),
                            ),
                            (
                                "method".into(),
                                if requested.is_some() {
                                    "rectangle"
                                } else {
                                    "relational"
                                }
                                .into(),
                            ),
                            ("historical".into(), "".into()),
                        ]
                        .into(),
                    ));
                }
                match current {
                    Some(s) if next != s => geometry_verb(s, next).into(),
                    Some(_) => "Rewrote pane content in place.".into(),
                    None => "Put up a pane.".into(),
                }
            }
            "pane_remove" => {
                let p = pane(v, arg(a, "id"))?;
                if aspect(v, &p.id, "pane-layout", "")
                    .is_some_and(|r| r.props.get("pinned") == Some(&Scalar::Bool(true)))
                {
                    return Err(invalid("pane is pinned"));
                }
                ops.push(delete(&p.id));
                "Took down the pane.".into()
            }
            "panes_arrange" => {
                let mut working = v.clone();
                for adjustment in a["panes"]
                    .as_array()
                    .filter(|x| !x.is_empty())
                    .ok_or_else(|| invalid("panes must contain adjustments"))?
                {
                    let object = adjustment
                        .as_object()
                        .ok_or_else(|| invalid("adjustment must be an object"))?;
                    if object
                        .keys()
                        .any(|k| !["id", "place", "size", "pinned"].contains(&k.as_str()))
                    {
                        return Err(invalid("agents arrange by relation and named size; human geometry uses pane_put"));
                    }
                    let p = pane(&working, arg(adjustment, "id"))?;
                    let current = spot(&working, &p.id)?;
                    let size = if adjustment.get("size").is_some() {
                        model::parse_size(arg(adjustment, "size")).map_err(invalid)?
                    } else {
                        (current.w, current.h)
                    };
                    let place = adjustment
                        .get("place")
                        .map(|_| Placement::parse(arg(adjustment, "place")).map_err(invalid))
                        .transpose()?;
                    let next = if place.is_some() {
                        layout::resolve(place.as_ref(), &sited(&working, &p.id)?, size)
                            .map_err(invalid)?
                    } else {
                        Spot::new(current.x, current.y, size.0, size.1)
                    };
                    let mut props: BTreeMap<String, Value> = [
                        ("spot".into(), serde_json::to_string(&next).unwrap().into()),
                        ("method".into(), "relational".into()),
                        ("historical".into(), "".into()),
                    ]
                    .into();
                    if let Some(pin) = adjustment["pinned"].as_bool() {
                        props.insert("pinned".into(), pin.into());
                    }
                    let layout = aspect(&working, &p.id, "pane-layout", "").unwrap();
                    ops.push(set(&layout.id, props.clone()));
                    let id = layout.id.clone();
                    let mut records: Vec<_> = working.records().cloned().collect();
                    let record = records.iter_mut().find(|r| r.id == id).unwrap();
                    for (key, value) in props {
                        if let Value::Scalar(s) = value {
                            record.props.insert(key, s);
                        }
                    }
                    working =
                        View::new(v.revision(), records).map_err(|e| invalid(e.to_string()))?;
                }
                "Arranged panes by relation and named size.".into()
            }
            "pane_annotate" => {
                let p = pane(v, arg(a, "id"))?;
                for slot in ["mark", "note", "pointed"] {
                    if let Some(value) = a.get(slot) {
                        let text = value
                            .as_str()
                            .ok_or_else(|| invalid("annotation must be text"))?;
                        let value = if slot == "mark" {
                            model::Mark::parse(text).map_err(invalid)?;
                            json!(text)
                        } else if slot == "note" {
                            json!(model::checked("note", text, 0, 600).map_err(invalid)?)
                        } else {
                            model::checked("pointed", text, 0, 8000).map_err(invalid)?;
                            let path: Vec<model::PointLevel> = serde_json::from_value(
                                a.get("pointed_path").cloned().unwrap_or(json!([])),
                            )
                            .map_err(|e| invalid(e.to_string()))?;
                            if path.len() > 6
                                || path.iter().any(|p| {
                                    p.role.chars().count() > 120
                                        || p.name.chars().count() > 120
                                        || p.point.as_ref().is_some_and(|s| s.chars().count() > 120)
                                })
                            {
                                return Err(invalid("pointed path exceeds its bounds"));
                            }
                            json!({"text":text,"path":path})
                        };
                        ops.push(update_aspect(
                            v,
                            &p.id,
                            "pane-annotation",
                            slot,
                            [
                                ("value".into(), encode_json(&value).into()),
                                ("historical".into(), "".into()),
                            ]
                            .into(),
                        ));
                    }
                }
                if ops.is_empty() || a.get("pointed_path").is_some() && a.get("pointed").is_none() {
                    return Err(invalid(
                        "annotation needs a mark, note or point; a path needs its point",
                    ));
                }
                "Recorded the person's annotation.".into()
            }
            "panes_configure" => {
                let old = v.records().find(|r| r.kind == "panes-settings");
                let mut props = BTreeMap::new();
                if a.get("intent").is_some() {
                    props.insert(
                        "intent".into(),
                        model::checked("intent", arg(a, "intent"), 1, 600)
                            .map_err(invalid)?
                            .into(),
                    );
                }
                if let Some(theme) = a.get("theme") {
                    let current = old
                        .map(|r| parse(str_(r, "theme")))
                        .transpose()?
                        .unwrap_or_default();
                    props.insert(
                        "theme".into(),
                        serde_json::to_string(
                            &model::merge_theme(&current, theme).map_err(invalid)?,
                        )
                        .unwrap()
                        .into(),
                    );
                }
                if props.is_empty() {
                    return Err(invalid("configure needs intent or theme"));
                }
                if let Some(r) = old {
                    ops.push(set(&r.id, props));
                } else {
                    props.entry("intent".into()).or_insert("".into());
                    props.entry("theme".into()).or_insert(
                        serde_json::to_string(&model::Theme::default())
                            .unwrap()
                            .into(),
                    );
                    ops.push(create("settings", "panes-settings", props));
                }
                "Configured panes.".into()
            }
            "panes_import" => return import(&a["document"], v),
            _ => return Err(CapError::new("unknown_action", &c.action)),
        };
        Ok(Plan { ops, readback })
    }
    fn validate(&self, delta: &Delta, v: &View) -> Result<(), CapError> {
        let mut ids = BTreeSet::new();
        for r in v.records().filter(|r| r.kind == "pane") {
            if !ids.insert(str_(r, "pane_id")) {
                return Err(invalid("duplicate pane id"));
            }
        }
        if ids.len() > 24 {
            return Err(invalid("too many panes"));
        }
        for id in delta.created.iter().chain(delta.modified.keys()) {
            let Some(r) = v.get(id) else { continue };
            match r.kind.as_str() {
                "pane" => {
                    model::pane_id(str_(r, "pane_id")).map_err(invalid)?;
                    model::checked("title", str_(r, "title"), 1, 120).map_err(invalid)?;
                    view::validate(&parse(str_(r, "tree"))?).map_err(invalid)?;
                }
                "pane-layout" => {
                    let s: Spot = parse(str_(r, "spot"))?;
                    if !matches!(
                        str_(r, "method"),
                        "relational" | "rectangle" | "raise" | "import"
                    ) {
                        return Err(invalid("invalid pane placement method"));
                    }
                    if s != s.clamped() {
                        return Err(invalid("unclamped pane rectangle"));
                    }
                    if !matches!(r.props.get("pinned"), Some(Scalar::Bool(_)))
                        || r.props
                            .get("stack")
                            .and_then(Scalar::as_f64)
                            .is_none_or(|s| s < 0.0 || s.fract() != 0.0)
                    {
                        return Err(invalid("invalid pin or stack"));
                    }
                }
                "pane-annotation" => {
                    let value: Json = parse(str_(r, "value"))?;
                    match str_(r, "slot") {
                        "mark" => {
                            model::Mark::parse(
                                value.as_str().ok_or_else(|| invalid("mark must be text"))?,
                            )
                            .map_err(invalid)?;
                        }
                        "note" => {
                            model::checked(
                                "note",
                                value.as_str().ok_or_else(|| invalid("note must be text"))?,
                                0,
                                600,
                            )
                            .map_err(invalid)?;
                        }
                        "pointed" => {
                            let text = value["text"]
                                .as_str()
                                .ok_or_else(|| invalid("point needs text"))?;
                            model::checked("pointed", text, 0, 8000).map_err(invalid)?;
                            let path: Vec<model::PointLevel> =
                                serde_json::from_value(value["path"].clone())
                                    .map_err(|e| invalid(e.to_string()))?;
                            if path.len() > 6
                                || path.iter().any(|p| {
                                    p.role.chars().count() > 120
                                        || p.name.chars().count() > 120
                                        || p.point.as_ref().is_some_and(|s| s.chars().count() > 120)
                                })
                            {
                                return Err(invalid("pointed path exceeds its bounds"));
                            }
                        }
                        _ => return Err(invalid("unknown annotation slot")),
                    }
                }
                "panes-settings" => {
                    let t: model::Theme = parse(str_(r, "theme"))?;
                    model::merge_theme(&model::Theme::default(), &serde_json::to_value(t).unwrap())
                        .map_err(invalid)?;
                    model::checked("intent", str_(r, "intent"), 0, 600).map_err(invalid)?;
                }
                "panes-history" => {
                    for k in ["original", "migrated"] {
                        let _: Json = parse(str_(r, k))?;
                    }
                }
                _ => {}
            }
        }
        let mut aspects = BTreeSet::new();
        for r in v
            .records()
            .filter(|r| matches!(r.kind.as_str(), "pane-layout" | "pane-annotation"))
        {
            if !v.get(str_(r, "pane")).is_some_and(|p| p.kind == "pane") {
                return Err(invalid("orphan pane aspect"));
            }
            if !aspects.insert((str_(r, "pane"), r.kind.as_str(), str_(r, "slot"))) {
                return Err(invalid("duplicate pane aspect"));
            }
        }
        if v.records().filter(|r| r.kind == "panes-history").count() > 1
            || v.records().filter(|r| r.kind == "panes-settings").count() > 1
        {
            return Err(invalid("duplicate pane document metadata"));
        }
        for r in v.records().filter(|r| r.kind == "pane") {
            if aspect(v, &r.id, "pane-layout", "").is_none() {
                return Err(invalid("pane requires a layout"));
            }
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        let human = c.principal.caller == CallerKind::Human;
        let import = matches!(c.origin,Origin::HostAction{capability,action} if capability==ID&&action=="panes_import")
            && !c.before.records().any(|r| r.kind == "panes-history");
        for id in &c.delta.deleted {
            if let Some(r) = c.before.get(id) {
                if r.kind == "panes-history" {
                    return Err(invalid("legacy history is immutable"));
                }
                if r.kind == "pane"
                    && aspect(c.before, id, "pane-layout", "")
                        .is_some_and(|r| r.props.get("pinned") == Some(&Scalar::Bool(true)))
                {
                    return Err(invalid("pane is pinned"));
                }
                if r.kind == "pane-annotation" && !human && c.after.contains(str_(r, "pane")) {
                    return Err(invalid("only humans remove annotations"));
                }
            }
        }
        for (id, keys) in c.attempted {
            let Some(r) = c.after.get(id) else { continue };
            let old = c.before.get(id);
            if r.kind == "panes-history" && !import {
                return Err(invalid("legacy history is import-only and immutable"));
            }
            if keys.contains("historical") && !str_(r, "historical").is_empty() && !import {
                return Err(invalid("historical bylines are import-only"));
            }
            if old.is_some_and(|o| !str_(o, "historical").is_empty())
                && keys.iter().any(|k| k != "historical")
                && !str_(r, "historical").is_empty()
            {
                return Err(invalid("new writes must retire the historical byline"));
            }
            if r.kind == "pane-annotation" && !human && !import {
                return Err(invalid("only the human can annotate a pane"));
            }
            if r.kind == "pane-layout" && !human && !import {
                let allowed = matches!(c.origin,Origin::HostAction{capability,action} if capability==ID&&matches!(action.as_str(),"pane_put"|"panes_arrange"));
                if !allowed
                    || str_(r, "method") != "relational"
                    || old.is_some() && keys.contains("stack")
                {
                    return Err(invalid(
                        "the agent cannot send a rectangle or raise; use relations",
                    ));
                }
            }
            if old.is_some() && keys.contains("pane") {
                return Err(invalid("pane aspects cannot change their pane"));
            }
            if old.is_some()
                && r.kind == "pane"
                && keys.contains("pane_id")
                && str_(old.unwrap(), "pane_id") != str_(r, "pane_id")
            {
                return Err(invalid("pane identity is stable"));
            }
        }
        self.validate(c.delta, c.after)
    }
    fn cascade(&self, deleted: &[String], v: &View) -> Result<Plan, CapError> {
        Ok(Plan {
            ops: v
                .records()
                .filter(|r| {
                    matches!(r.kind.as_str(), "pane-layout" | "pane-annotation")
                        && deleted.iter().any(|id| id == str_(r, "pane"))
                })
                .map(|r| delete(&r.id))
                .collect(),
            readback: String::new(),
        })
    }
    fn changes(&self, b: &Json, a: &Json, _reader: &str) -> Result<Vec<String>, CapError> {
        Ok(changes(b, a))
    }
}
fn geometry_verb(a: Spot, b: Spot) -> &'static str {
    match (a.x != b.x || a.y != b.y, a.w != b.w || a.h != b.h) {
        (true, true) => "moved and resized",
        (false, true) => "resized",
        (true, false) => "moved",
        _ => "unchanged",
    }
}
pub fn import(original: &Json, v: &View) -> Result<Plan, CapError> {
    if v.records().any(|r| r.kind == "panes-history") {
        return Ok(Plan {
            ops: vec![],
            readback: "Legacy room already imported; nothing changed.".into(),
        });
    }
    if original.to_string().len() > 4 * 1024 * 1024 {
        return Err(invalid("legacy room exceeds 4 MiB"));
    }
    let mut migrated = original.clone();
    model::migrate(&mut migrated).map_err(invalid)?;
    let doc: model::RoomDoc =
        serde_json::from_value(migrated.clone()).map_err(|e| invalid(e.to_string()))?;
    model::validate_doc(&doc).map_err(invalid)?;
    if doc.schema_version != 3 {
        return Err(invalid("unsupported legacy room schema"));
    }
    if v.records().any(|r| r.kind == "panes-settings")
        || v.records().filter(|r| r.kind == "pane").count() + doc.panes.len() > 24
    {
        return Err(invalid(
            "legacy import conflicts with current pane settings or pane limit",
        ));
    }
    let mut ops = vec![
        create(
            "history",
            "panes-history",
            [
                ("original".into(), encode_json(original).into()),
                ("migrated".into(), encode_json(&migrated).into()),
            ]
            .into(),
        ),
        create(
            "settings",
            "panes-settings",
            [
                ("intent".into(), doc.intent.into()),
                (
                    "theme".into(),
                    serde_json::to_string(&doc.theme).unwrap().into(),
                ),
            ]
            .into(),
        ),
    ];
    for (i, p) in doc.panes.iter().enumerate() {
        model::pane_id(&p.id).map_err(invalid)?;
        if pane(v, &p.id).is_ok() {
            return Err(invalid("legacy pane id already exists"));
        }
        let historical = serde_json::to_string(p).unwrap();
        let reference = format!("import-{i}");
        ops.push(create(
            &reference,
            "pane",
            [
                ("pane_id".into(), p.id.clone().into()),
                ("title".into(), p.title.clone().into()),
                (
                    "tree".into(),
                    serde_json::to_string(&p.view).unwrap().into(),
                ),
                ("historical".into(), historical.clone().into()),
            ]
            .into(),
        ));
        ops.push(create(
            &format!("layout-{i}"),
            "pane-layout",
            [
                ("pane".into(), Value::reference(&reference)),
                (
                    "spot".into(),
                    serde_json::to_string(&p.spot.clamped()).unwrap().into(),
                ),
                ("stack".into(), (p.stack as f64).into()),
                ("pinned".into(), p.pinned.into()),
                ("method".into(), "import".into()),
                ("historical".into(), historical.clone().into()),
            ]
            .into(),
        ));
        for (slot, value) in [
            ("mark", json!(p.mark)),
            ("note", json!(p.note)),
            ("pointed", json!({"text":p.pointed,"path":p.pointed_path})),
        ] {
            ops.push(create(
                &format!("{slot}-{i}"),
                "pane-annotation",
                [
                    ("pane".into(), Value::reference(&reference)),
                    ("slot".into(), slot.into()),
                    ("value".into(), encode_json(&value).into()),
                    ("historical".into(), historical.clone().into()),
                ]
                .into(),
            ));
        }
    }
    Ok(Plan{ops,readback:"Imported legacy panes, independent bylines, fields and log. Historical authorship is retained as history.".into()})
}
fn historical(r: &Record) -> Json {
    serde_json::from_str(str_(r, "historical")).unwrap_or(Json::Null)
}
fn byline(r: &Record, slot: &str) -> (Json, Json, Json) {
    let h = historical(r);
    if !h.is_null() {
        if slot.is_empty() {
            return (
                h["by_id"].clone(),
                h["by_name"].clone(),
                h["author"].clone(),
            );
        }
        let key = if slot == "pointed" { "pointed" } else { slot };
        return (
            h[format!("{key}_by")].clone(),
            h[format!("{key}_by_name")].clone(),
            json!("human"),
        );
    }
    (
        json!(str_(r, "touched_by_participant")),
        json!(str_(r, "touched_by")),
        json!("participant"),
    )
}
pub fn projection(v: &View, facts: &BTreeMap<String, Json>) -> Result<Json, CapError> {
    let mut panes = vec![];
    for p in v.records().filter(|r| r.kind == "pane") {
        let layout =
            aspect(v, &p.id, "pane-layout", "").ok_or_else(|| invalid("pane has no layout"))?;
        let node: view::Node = parse(str_(p, "tree"))?;
        let (by_id, by_name, author) = byline(p, "");
        let mut projected = json!({"id":str_(p,"pane_id"),"record_id":p.id,"title":str_(p,"title"),"view":view::resolve(&node,facts),"spot":spot(v,&p.id)?,"stack":layout.props.get("stack"),"pinned":layout.props.get("pinned"),"by_id":by_id,"by_name":by_name,"author":author,"historical":!historical(p).is_null(),"imported_by":if historical(p).is_null(){Json::Null}else{json!(str_(p,"touched_by"))},"mark":"none","note":"","pointed":"","pointed_path":[],"revision":historical(p)["revision"]});
        let (id, name, _) = byline(layout, "");
        projected["layout_by_id"] = id;
        projected["layout_by_name"] = name;
        for slot in ["mark", "note", "pointed"] {
            if let Some(r) = aspect(v, &p.id, "pane-annotation", slot) {
                let value: Json = parse(str_(r, "value"))?;
                let (id, name, _) = byline(r, slot);
                projected[format!("{slot}_historical")] = json!(!historical(r).is_null());
                projected[format!("{slot}_by")] = id;
                projected[format!("{slot}_by_name")] = name;
                if slot == "pointed" {
                    projected["pointed"] = value["text"].clone();
                    projected["pointed_path"] = value["path"].clone();
                } else {
                    projected[slot] = value;
                }
            }
        }
        panes.push(projected);
    }
    panes.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let settings = v.records().find(|r| r.kind == "panes-settings");
    let history = v.records().find(|r| r.kind == "panes-history");
    Ok(
        json!({"schema_version":3,"revision":v.revision(),"panes":panes,"intent":settings.map(|r|str_(r,"intent")).unwrap_or(""),"theme":settings.map(|r|parse::<Json>(str_(r,"theme"))).transpose()?.unwrap_or(json!(model::Theme::default())),"history":history.map(|r|parse::<Json>(str_(r,"migrated"))).transpose()?,"original":history.map(|r|parse::<Json>(str_(r,"original"))).transpose()?,"legacy_revision":history.map(|r|parse::<Json>(str_(r,"migrated")).map(|h|h["revision"].clone())).transpose()?,"log":history.map(|r|parse::<Json>(str_(r,"migrated")).map(|h|h["log"].clone())).transpose()?.unwrap_or(json!([]))}),
    )
}
pub fn describe(v: &View) -> Result<String, CapError> {
    let projected = Panes.project(v)?;
    describe_projection(&projected)
}
pub fn describe_projection(p: &Json) -> Result<String, CapError> {
    let panes = p["panes"]
        .as_array()
        .ok_or_else(|| invalid("projection has no panes"))?;
    let mut out = format!("PANES\nIntent: {}\n", p["intent"].as_str().unwrap_or(""));
    for pane in panes {
        let node: view::Node =
            serde_json::from_value(pane["view"].clone()).map_err(|e| invalid(e.to_string()))?;
        out.push_str(&format!(
            "- [{}] “{}”: {} (by {})\n",
            pane["id"].as_str().unwrap_or(""),
            pane["title"].as_str().unwrap_or(""),
            view::summarize(&node),
            format_args!(
                "{}{}",
                pane["by_name"].as_str().unwrap_or("unnamed author"),
                if pane["historical"] == true {
                    " (historical)"
                } else {
                    ""
                }
            )
        ));
        for slot in ["mark", "note", "pointed"] {
            let value = pane[slot].as_str().unwrap_or("");
            if !value.is_empty() && value != "none" {
                out.push_str(&format!(
                    "  {slot}: {value} (by {})\n",
                    format_args!(
                        "{}{}",
                        pane[format!("{slot}_by_name")]
                            .as_str()
                            .unwrap_or("someone"),
                        if pane[format!("{slot}_historical")] == true {
                            " (historical)"
                        } else {
                            ""
                        }
                    )
                ));
            }
        }
        fn errors(n: &Json, out: &mut String) {
            if let Some(e) = n["error"].as_str() {
                out.push_str(&format!("  source/view unavailable: {e}\n"));
            }
            for child in n["children"].as_array().into_iter().flatten() {
                errors(child, out);
            }
        }
        errors(&pane["view"], &mut out);
    }
    let sites: Vec<_> = panes
        .iter()
        .map(|p| {
            Ok(Sited {
                id: p["id"].as_str().unwrap_or(""),
                title: p["title"].as_str().unwrap_or(""),
                spot: serde_json::from_value(p["spot"].clone())
                    .map_err(|e| invalid(e.to_string()))?,
            })
        })
        .collect::<Result<_, CapError>>()?;
    out.push_str(&layout::describe(&sites));
    Ok(out)
}
pub fn changes(b: &Json, a: &Json) -> Vec<String> {
    let prior = b["panes"].as_array().cloned().unwrap_or_default();
    let next = a["panes"].as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for field in ["intent", "theme"] {
        if b[field] != a[field] {
            out.push(format!("{field} changed"));
        }
    }
    let sites: Vec<_> = next
        .iter()
        .filter_map(|p| {
            Some(Sited {
                id: p["id"].as_str()?,
                title: p["title"].as_str()?,
                spot: serde_json::from_value(p["spot"].clone()).ok()?,
            })
        })
        .collect();
    for (index, p) in next.iter().enumerate() {
        let title = p["title"].as_str().unwrap_or("");
        if let Some(old) = prior.iter().find(|x| x["id"] == p["id"]) {
            if old["spot"] != p["spot"]
                && let (Ok(before), Ok(after)) = (
                    serde_json::from_value::<Spot>(old["spot"].clone()),
                    serde_json::from_value::<Spot>(p["spot"].clone()),
                ) {
                    out.push(format!(
                        "{} “{title}” by {}. It now sits {}",
                        geometry_verb(before, after),
                        p["layout_by_name"].as_str().unwrap_or("someone"),
                        layout::locate(index, &sites)
                    ));
                }
            for field in ["title", "view", "mark", "note", "pointed", "pinned"] {
                if old[field] != p[field] {
                    out.push(format!("{field} changed on “{title}”"));
                }
            }
        } else {
            out.push(format!("put up “{title}”"));
        }
    }
    for p in prior {
        if !next.iter().any(|x| x["id"] == p["id"]) {
            out.push(format!("took down “{}”", p["title"].as_str().unwrap_or("")));
        }
    }
    out
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Panes, Panes);
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn project_with_facts(view_json: &str, facts_json: &str) -> String {
    let result = (|| {
        let view: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let facts: BTreeMap<String, Json> = serde_json::from_str(facts_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        projection(&view, &facts)
    })();
    match result {
        Ok(ok) => json!({"abi":samepage_capability::ABI,"ok":ok}).to_string(),
        Err(error) => json!({"abi":samepage_capability::ABI,"error":error}).to_string(),
    }
}
