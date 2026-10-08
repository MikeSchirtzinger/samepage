//! Portable drawing plans, geometry and relational read-back. The host alone
//! stamps authors and composes optional owners' lift contributions.
pub mod excalidraw;
pub mod model;
use model::{Drawing, Shape};
use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CapError, Capability, Change, Delta,
    Descriptor, LegendEntry, Op, Plan, Record, Scalar, Target, Trigger, Value, View,
};
use samepage_map::{prop_num, prop_str, rules, Map};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};
pub const ID: &str = "sketch";
pub const VERSION: &str = "0.1.0";
pub const FORMS: &[&str] = &[
    "ink", "rect", "ellipse", "diamond", "line", "arrow", "text", "frame",
];
pub const MAX_POINTS: usize = 400;
pub const KEYS: &[&str] = &[
    "form",
    "x",
    "y",
    "w",
    "h",
    "points",
    "from",
    "to",
    "head",
    "ink",
    "fill",
    "label",
    "stroke_width",
    "stroke_style",
    "opacity",
    "roundness",
    "font_size",
    "angle",
    "groups",
    "frame",
    "z",
];
pub const INK_HEX: &[(&str, &str)] = &[
    ("slate", "#93a1bd"),
    ("blue", "#6ea8ff"),
    ("green", "#4ade80"),
    ("amber", "#fbbf24"),
    ("red", "#f87171"),
    ("violet", "#a78bfa"),
];
pub fn ink_hex(name: &str) -> &'static str {
    INK_HEX
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or("#93a1bd")
}
pub fn nearest_ink(hex: &str) -> &'static str {
    let parse = |s: &str| -> Option<[u8; 3]> {
        let s = s.trim().trim_start_matches('#');
        if !s.is_ascii() {
            return None;
        }
        let full = match s.len() {
            3 => s.chars().flat_map(|c| [c, c]).collect::<String>(),
            6 | 8 => s[..6].to_owned(),
            _ => return None,
        };
        Some([
            u8::from_str_radix(&full[..2], 16).ok()?,
            u8::from_str_radix(&full[2..4], 16).ok()?,
            u8::from_str_radix(&full[4..6], 16).ok()?,
        ])
    };
    let Some(want) = parse(hex) else {
        return "slate";
    };
    INK_HEX
        .iter()
        .map(|(n, h)| {
            let rgb = parse(h).expect("constant palette");
            (
                *n,
                (0..3)
                    .map(|i| (f64::from(rgb[i]) - f64::from(want[i])).powi(2))
                    .sum::<f64>(),
            )
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| n)
        .unwrap_or("slate")
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShapePatch {
    pub id: Option<String>,
    pub form: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub points: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub head: Option<String>,
    pub ink: Option<String>,
    pub fill: Option<String>,
    pub label: Option<String>,
    pub stroke_width: Option<f64>,
    pub stroke_style: Option<String>,
    pub opacity: Option<f64>,
    pub roundness: Option<String>,
    pub font_size: Option<f64>,
    pub angle: Option<f64>,
    pub groups: Option<String>,
    pub frame: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}
fn invalid(s: impl Into<String>) -> CapError {
    CapError::new("invalid", s)
}
fn decl(
    name: &str,
    alias: Option<&str>,
    effect: samepage_capability::Effect,
    schema: JsonValue,
    description: &str,
) -> ActionDecl {
    let mut d = ActionDecl::new(name, Audience::Both, effect, description);
    if let Some(a) = alias {
        d = d.alias(a);
    }
    d.input_schema = schema;
    d
}
pub fn descriptor() -> Descriptor {
    use samepage_capability::Effect::{Mutate, Query};
    let empty = json!({"type":"object","properties":{},"additionalProperties":false});
    let mut d=Descriptor::new(ID,VERSION,Artifact::Wasm).depends_on("map",samepage_map::VERSION,"Drawings relate to cards; lift creates a card in one batch").owns("shape",KEYS,"Ordinary drawn shapes, ink, text, styles and bindings").reads("node").reads("constraint").reads("constraint-member")
 .action(decl("sketch",Some("atlas_sketch"),Mutate,json!({"type":"object","required":["shapes"],"properties":{"shapes":{"type":"array","minItems":1,"maxItems":2000,"items":{"type":"object"}}},"additionalProperties":false}),"Draw or revise shapes in paint order. A refused element stops the batch's successful prefix; the result names every skipped element. Arrows bind to cards or earlier shape refs."))
 .action(decl("sketch_import",Some("atlas_import"),Mutate,json!({"type":"object","required":["document"],"properties":{"document":{"type":"string","maxLength":4000000},"dx":{"type":"number"},"dy":{"type":"number"}},"additionalProperties":false}),"Import Excalidraw shapes, reconnect bindings and frames in one batch, and name unsupported elements."))
 .action(decl("sketch_export",None,Query,empty.clone(),"Export the drawn layer as an Excalidraw document. Captions, styles, groups and frames survive."))
 .action(decl("sketch_read",None,Query,empty,"Read drawings as ENCLOSES, CROSSES-OUT, SAYS and bound-to relations, without coordinates."))
 .action(decl("shape_lift",None,Mutate,json!({"type":"object","required":["id"],"properties":{"id":{"type":"string"}},"additionalProperties":false}),"Lift labeled drawn matter or a region holding exactly one text into a card. References and marks follow in the same commit."))
 .action(decl("shape_remove",None,Mutate,json!({"type":"object","required":["id"],"properties":{"id":{"type":"string"}},"additionalProperties":false}),"Remove a drawing; its owners detach bindings and cascade marks in one commit."))
 .trigger(Trigger::kind("shape"));
    for action in &d.actions {
        d.triggers.push(Trigger::tool(&action.name));
        for a in &action.aliases {
            d.triggers.push(Trigger::tool(a));
        }
    }
    for tool in ["select", "rectangle", "ellipse", "arrow", "pen", "text"] {
        d.triggers.push(Trigger::tool(&format!("sketch-{tool}")));
    }
    d
}
pub struct Sketch;
impl Capability for Sketch {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, view: &View) -> Result<JsonValue, CapError> {
        project(view)
    }
    fn digest(&self, view: &View) -> Result<JsonValue, CapError> {
        let drawing = drawing(view);
        Ok(
            json!({"readback":describe(&drawing),"shapes":drawing.shapes.iter().map(|s|json!({"id":s.id,"form":s.form,"label":s.label,"created_by":s.created_by,"touched_by":s.touched_by})).collect::<Vec<_>>() }),
        )
    }
    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        match call.action.as_str() {
            "sketch" => sketch(
                call.args
                    .get("shapes")
                    .and_then(JsonValue::as_array)
                    .ok_or_else(|| invalid("shapes must be an array"))?,
                view,
            ),
            "sketch_import" => import(&call.args, view),
            "sketch_export" => Ok(Plan {
                ops: vec![],
                readback: excalidraw::to_excalidraw(&drawing(view)),
            }),
            "sketch_read" => Ok(Plan {
                ops: vec![],
                readback: describe(&drawing(view)),
            }),
            "shape_remove" => {
                let id = call.args["id"]
                    .as_str()
                    .ok_or_else(|| invalid("id required"))?;
                shape_record(view, id)?;
                Ok(Plan {
                    ops: vec![delete(id)],
                    readback: "Drawing removed.".into(),
                })
            }
            "shape_lift" => Err(invalid("lift requires plan_batch")),
            _ => Err(CapError::new(
                "unknown_action",
                format!("sketch has no action {}", call.action),
            )),
        }
    }
    fn plan_batch(&self, call: &Call, view: &View) -> Result<Batch, CapError> {
        let entries = if call.action == "shape_lift" {
            lift(
                view,
                call.args["id"]
                    .as_str()
                    .ok_or_else(|| invalid("id required"))?,
            )?
        } else {
            vec![entry(ID, self.plan(call, view)?)]
        };
        Ok(Batch {
            base_revision: view.revision().into(),
            entries,
        })
    }
    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        let created = delta
            .created
            .iter()
            .filter(|id| view.get(id).is_some_and(|r| r.kind == "shape"))
            .count();
        rules::check_growth(
            view.records().filter(|r| r.kind == "shape").count(),
            created,
        )?;
        for id in delta.created.iter().chain(delta.modified.keys()) {
            if let Some(r) = view.get(id).filter(|r| r.kind == "shape") {
                check_shape(r, view)?;
            }
        }
        for r in view.records().filter(|r| r.kind == "shape") {
            for key in ["from", "to", "frame"] {
                if delta.deleted.contains(prop_str(r, key).unwrap_or("")) {
                    return Err(invalid(format!("sketch: {} still refers to a removed {key}; detach or retarget it in the same batch",r.id)));
                }
            }
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        for (id, keys) in c.attempted {
            if c.before.get(id).is_some_and(|r| {
                r.kind == "shape" && !FORMS.contains(&prop_str(r, "form").unwrap_or(""))
            }) {
                return Err(invalid(format!(
                    "unsupported sketch form on {id} is read-only"
                )));
            }
            if c.after
                .get(id)
                .is_some_and(|r| r.kind == "shape" && prop_str(r, "form") == Some("text"))
                && keys.contains("h")
            {
                return Err(invalid(
                    "text height is derived from its words; set w instead",
                ));
            }
        }
        self.validate(c.delta, c.after)
    }
    fn cascade(&self, deleted: &[String], view: &View) -> Result<Plan, CapError> {
        let mut ops = vec![];
        for r in view.records().filter(|r| r.kind == "shape") {
            let mut props = BTreeMap::new();
            for key in ["from", "to", "frame"] {
                if deleted
                    .iter()
                    .any(|id| Some(id.as_str()) == prop_str(r, key))
                {
                    props.insert(key.into(), "".into());
                }
            }
            if !props.is_empty() {
                let s = shape(r);
                if s.points.len() < 2 && matches!(s.form.as_str(), "line" | "arrow") {
                    if let Some(path) = drawing(view).shape_path(&s) {
                        props.insert(
                            "points".into(),
                            format_points(
                                &path
                                    .iter()
                                    .map(|(x, y)| (x - s.x, y - s.y))
                                    .collect::<Vec<_>>(),
                            )
                            .into(),
                        );
                    }
                }
                ops.push(set(&r.id, props));
            }
        }
        Ok(Plan {
            ops,
            readback: String::new(),
        })
    }
    fn cascade_with_before(
        &self,
        deleted: &[String],
        before: &View,
        view: &View,
    ) -> Result<Plan, CapError> {
        let mut plan = self.cascade(deleted, view)?;
        let d = drawing(before);
        for op in &mut plan.ops {
            if let Op::Set {
                target: Target::Id(id),
                props,
            } = op
            {
                if let Some(s) = d
                    .shape(id)
                    .filter(|s| matches!(s.form.as_str(), "line" | "arrow"))
                {
                    if let Some(path) = d.shape_path(s) {
                        props.insert(
                            "points".into(),
                            format_points(
                                &path
                                    .iter()
                                    .map(|(x, y)| (x - s.x, y - s.y))
                                    .collect::<Vec<_>>(),
                            )
                            .into(),
                        );
                    }
                }
            }
        }
        Ok(plan)
    }
    fn changes(
        &self,
        before: &JsonValue,
        after: &JsonValue,
        _: &str,
    ) -> Result<Vec<String>, CapError> {
        if before == after {
            Ok(vec![])
        } else {
            Ok(vec![after["readback"].as_str().unwrap_or("").into()])
        }
    }
    fn legend(&self, view: &View) -> Result<Vec<LegendEntry>, CapError> {
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        for shape in drawing(view).shapes {
            for (attribute, value, label) in [
                (
                    "form",
                    shape.form.clone(),
                    format!("{} drawing", shape.form),
                ),
                (
                    "ink",
                    shape.ink.clone(),
                    format!("{} ink; meaning assigned by its author", shape.ink),
                ),
                ("fill", shape.fill.clone(), format!("{} fill", shape.fill)),
                (
                    "stroke_style",
                    shape.stroke_style.clone(),
                    format!("{} stroke", shape.stroke_style),
                ),
                (
                    "head",
                    shape.head.clone(),
                    format!("{} arrowhead", shape.head),
                ),
                (
                    "bound",
                    (!shape.from.is_empty() || !shape.to.is_empty()).to_string(),
                    "Bound arrow follows its target".into(),
                ),
            ] {
                if (attribute == "bound" && value == "false")
                    || (attribute == "head" && shape.form != "arrow")
                    || !seen.insert((attribute, value.clone()))
                {
                    continue;
                }
                entries.push(LegendEntry {
                    capability: ID.into(),
                    attribute: attribute.into(),
                    value,
                    label,
                    class: Some(
                        if ["head", "bound"].contains(&attribute) {
                            "sketch-shape sketch-arrow"
                        } else {
                            "sketch-shape"
                        }
                        .into(),
                    ),
                    symbol: Some("✎".into()),
                });
            }
        }
        entries.push(LegendEntry {
            capability: ID.into(),
            attribute: "element".into(),
            value: "byline".into(),
            label: "Host-stamped drawing byline (selected shape)".into(),
            class: Some("sketch-byline".into()),
            symbol: Some("by".into()),
        });
        Ok(entries)
    }
}
pub fn entry(cap: &str, plan: Plan) -> BatchEntry {
    BatchEntry {
        capability: cap.into(),
        plan,
    }
}
fn set(id: &str, props: BTreeMap<String, Value>) -> Op {
    Op::Set {
        target: Target::Id(id.into()),
        props,
    }
}
fn delete(id: &str) -> Op {
    Op::Delete {
        target: Target::Id(id.into()),
    }
}
fn shape_record<'a>(view: &'a View, id: &str) -> Result<&'a Record, CapError> {
    view.get(id)
        .filter(|r| r.kind == "shape")
        .ok_or_else(|| invalid(format!("no shape {id:?}")))
}
pub fn parse_points(raw: &str) -> Vec<(f64, f64)> {
    raw.split_whitespace()
        .filter_map(|s| {
            let (x, y) = s.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .collect()
}
pub fn format_points(points: &[(f64, f64)]) -> String {
    points
        .iter()
        .map(|(x, y)| format!("{x:.1},{y:.1}"))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn shape(r: &Record) -> Shape {
    let string = |key| prop_str(r, key).unwrap_or("").to_string();
    let number = |key, default| prop_num(r, key).unwrap_or(default);
    let mut s = Shape {
        id: r.id.clone(),
        form: string("form"),
        x: number("x", 0.0),
        y: number("y", 0.0),
        w: number("w", 240.0),
        h: number("h", 160.0),
        points: parse_points(&string("points")),
        from: string("from"),
        to: string("to"),
        head: prop_str(r, "head").unwrap_or("none").into(),
        ink: prop_str(r, "ink").unwrap_or("slate").into(),
        fill: prop_str(r, "fill").unwrap_or("none").into(),
        label: string("label"),
        stroke_width: number("stroke_width", 2.0),
        stroke_style: prop_str(r, "stroke_style").unwrap_or("solid").into(),
        opacity: number("opacity", 100.0),
        roundness: prop_str(r, "roundness").unwrap_or("round").into(),
        font_size: number("font_size", 0.0),
        angle: number("angle", 0.0),
        groups: string("groups")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        frame: string("frame"),
        z: number("z", 0.0),
        measured: true,
        created_by: string("created_by"),
        touched_by: string("touched_by"),
    };
    if s.form == "text" {
        let scale = if s.font_size > 0.0 {
            s.font_size / 13.0
        } else {
            1.0
        };
        s.h = s
            .label
            .split('\n')
            .map(|line| {
                (line.chars().count() as f64 / (38.0 * s.w / 240.0 / scale).max(4.0))
                    .ceil()
                    .max(1.0)
            })
            .sum::<f64>()
            * 17.0
            * scale;
        s.measured = false;
    }
    s
}
pub fn drawing(view: &View) -> Drawing {
    let nodes = Map.project(view).expect("infallible map projection")["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| model::Node {
            id: n["id"].as_str().unwrap().into(),
            label: n["label"].as_str().unwrap().into(),
            x: n["x"].as_f64().unwrap(),
            y: n["y"].as_f64().unwrap(),
            w: n["width"].as_f64().unwrap(),
            h: n["height"].as_f64().unwrap(),
        })
        .collect();
    let mut shapes: Vec<_> = view
        .records()
        .filter(|r| r.kind == "shape")
        .map(shape)
        .collect();
    shapes.sort_by(|a, b| a.z.total_cmp(&b.z).then(a.id.cmp(&b.id)));
    Drawing { nodes, shapes }
}
pub fn describe(d: &Drawing) -> String {
    let mut out = format!("SKETCH: {} drawings.\n", d.shapes.len());
    for (s, r) in d.shapes.iter().zip(model::readings(d)) {
        let targets = r
            .targets
            .iter()
            .map(|id| {
                d.node(id)
                    .map(|n| n.label.as_str())
                    .or_else(|| {
                        d.shape(id).map(|s| {
                            if s.label.is_empty() {
                                s.form.as_str()
                            } else {
                                s.label.as_str()
                            }
                        })
                    })
                    .unwrap_or(id)
            })
            .map(|label| format!("{label:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let words = match r.relation.as_str() {
            "says" => format!(
                "SAYS {:?}{}",
                s.label,
                if targets.is_empty() {
                    String::new()
                } else {
                    format!(" NEAR {targets}")
                }
            ),
            "encloses" => format!("ENCLOSES {targets}"),
            "crosses-out" => format!("CROSSES-OUT {targets}"),
            "connects" => format!("CONNECTS {targets}; bound-to {} -> {}", s.from, s.to),
            "points-at" => format!(
                "POINTS-AT {targets}{}",
                if s.to.is_empty() {
                    String::new()
                } else {
                    format!("; bound-to {}", s.to)
                }
            ),
            "starts-at" => format!("STARTS-AT {targets}; bound-to {}", s.from),
            _ => "touches no object".into(),
        };
        out.push_str(&format!(
            "[{}] {} {:?}: {words}{} (created by {}; last by {})\n",
            s.id,
            s.form,
            s.label,
            s.style_note(d),
            s.created_by,
            s.touched_by
        ));
        if !FORMS.contains(&s.form.as_str()) {
            out.push_str(&format!(
                "unsupported form {:?}; preserved, read-only\n",
                s.form
            ));
        }
        if !r.partial.is_empty() {
            out.push_str(&format!(
                "ambiguous partial enclosure: {}\n",
                r.partial.join(", ")
            ));
        }
    }
    out
}
pub fn project(view: &View) -> Result<JsonValue, CapError> {
    project_drawing(&drawing(view))
}
fn project_drawing(d: &Drawing) -> Result<JsonValue, CapError> {
    Ok(
        json!({"palette":INK_HEX.iter().copied().collect::<BTreeMap<_,_>>(),"shapes":d.shapes.iter().zip(model::readings(d)).map(|(s,r)|json!({"shape":s,"ends":d.shape_endpoints(s).map(|(a,b)|[a,b]),"path":d.shape_path(s).unwrap_or_default(),"outline":if s.form=="ink"{model::outline(&s.world_points(),7.0)}else{vec![]},"reading":r,"supported":FORMS.contains(&s.form.as_str())})).collect::<Vec<_>>(),"readback":describe(d)}),
    )
}
fn check_shape(r: &Record, view: &View) -> Result<(), CapError> {
    let s = shape(r);
    rules::one_of(&s.form, FORMS, "form")?;
    for (key, allowed) in [
        ("head", &["none", "triangle", "dot"][..]),
        ("stroke_style", &["solid", "dashed", "dotted"][..]),
        ("roundness", &["round", "sharp"][..]),
        ("ink", rules::INKS),
    ] {
        if let Some(value) = r.props.get(key) {
            rules::one_of(
                value
                    .as_str()
                    .ok_or_else(|| invalid(format!("{key} must be text")))?,
                allowed,
                key,
            )?;
        }
    }
    if s.fill != "none" {
        rules::one_of(&s.fill, rules::INKS, "fill")?;
    }
    for key in KEYS.iter().filter(|key| {
        ![
            "x",
            "y",
            "w",
            "h",
            "stroke_width",
            "opacity",
            "font_size",
            "angle",
            "z",
        ]
        .contains(key)
    }) {
        if r.props.get(*key).is_some_and(|v| v.as_str().is_none()) {
            return Err(invalid(format!("{key} must be text")));
        }
    }
    for key in [
        "x",
        "y",
        "w",
        "h",
        "stroke_width",
        "opacity",
        "font_size",
        "angle",
        "z",
    ] {
        if let Some(v) = r.props.get(key) {
            if v.as_f64().is_none() {
                return Err(invalid(format!("{key} must be numeric")));
            }
        }
    }
    for (key, value) in [("x", s.x), ("y", s.y)] {
        rules::coordinate(value, key)?;
    }
    rules::within(s.stroke_width, 0.5, 12.0, "stroke_width")?;
    rules::within(s.opacity, 0.0, 100.0, "opacity")?;
    if s.font_size != 0.0 {
        rules::within(s.font_size, 6.0, 120.0, "font_size")?;
    }
    if s.groups.len() > 8 {
        return Err(invalid("a shape may belong to at most 8 groups"));
    }
    rules::text(&s.label, rules::MAX_LABEL, "shape label")?;
    if s.form == "text" && s.label.is_empty() {
        return Err(invalid("a text needs words"));
    }
    if s.is_sized() {
        rules::within(s.w, 8.0, 8000.0, "w")?;
        if s.form != "text" {
            rules::within(s.h, 8.0, 8000.0, "h")?;
        }
        if !s.points.is_empty() {
            return Err(invalid("sized forms have no points"));
        }
    } else {
        if s.angle != 0.0 {
            return Err(invalid("a stroke rotates through its points"));
        }
        let raw = prop_str(r, "points").unwrap_or("");
        if s.points.len() != raw.split_whitespace().count() {
            return Err(invalid("points must be x,y pairs"));
        }
        if !s.points.is_empty() && s.points.len() < 2 {
            return Err(invalid("a stroke needs two points"));
        }
        if s.points.len() > MAX_POINTS {
            return Err(invalid("too many points"));
        }
        if s.points.is_empty() && (s.from.is_empty() || s.to.is_empty()) {
            return Err(invalid("a stroke needs points or both bindings"));
        }
        for (x, y) in &s.points {
            rules::coordinate(*x, "point x")?;
            rules::coordinate(*y, "point y")?;
        }
    }
    let d = drawing(view);
    for end in [&s.from, &s.to] {
        if !end.is_empty() {
            if !matches!(s.form.as_str(), "line" | "arrow") {
                return Err(invalid("only connectors bind to another object"));
            }
            if end == &s.id || d.binding_bounds(end).is_none() {
                return Err(invalid(format!("no bindable card or shape {end:?}")));
            }
        }
    }
    if !s.frame.is_empty()
        && (s.form == "frame" || d.shape(&s.frame).is_none_or(|f| f.form != "frame"))
    {
        return Err(invalid(format!(
            "no frame {:?}; frames never nest",
            s.frame
        )));
    }
    Ok(())
}
fn place(p: &ShapePatch, view: &View, reference: &str) -> Result<(Op, Record), CapError> {
    let existing =
        p.id.as_deref()
            .map(|id| shape_record(view, id))
            .transpose()?;
    if existing.is_some_and(|r| !FORMS.contains(&prop_str(r, "form").unwrap_or(""))) {
        return Err(invalid("unsupported historical form is read-only"));
    }
    let form = p
        .form
        .as_deref()
        .or_else(|| existing.and_then(|r| prop_str(r, "form")))
        .ok_or_else(|| invalid("a new shape needs a form"))?;
    rules::one_of(form, FORMS, "form")?;
    let sized = matches!(form, "rect" | "ellipse" | "diamond" | "text" | "frame");
    let mut props: BTreeMap<String, Value> = BTreeMap::new();
    props.insert("form".into(), form.into());
    for (key, value) in [
        ("x", p.x),
        ("y", p.y),
        ("w", p.w),
        ("h", p.h),
        ("stroke_width", p.stroke_width),
        ("opacity", p.opacity),
        ("font_size", p.font_size),
        ("angle", p.angle),
    ] {
        if let Some(v) = value {
            if !v.is_finite() {
                return Err(invalid(format!("{key} must be finite")));
            }
            let v = match key {
                "x" | "y" => rules::coordinate(v, key)?,
                "w" | "h" => {
                    if !sized {
                        return Err(invalid("a stroke is sized by its points"));
                    }
                    if key == "h" && form == "text" {
                        return Err(invalid("text height comes from its words; set w instead"));
                    }
                    v.clamp(8.0, 8000.0)
                }
                "stroke_width" => v.clamp(0.5, 12.0),
                "opacity" => v.clamp(0.0, 100.0),
                "font_size" => {
                    if v <= 0.0 {
                        0.0
                    } else {
                        v.clamp(6.0, 120.0)
                    }
                }
                "angle" => {
                    if !sized {
                        return Err(invalid("a stroke rotates through its points"));
                    }
                    let mut a = v % std::f64::consts::TAU;
                    if a > std::f64::consts::PI {
                        a -= std::f64::consts::TAU;
                    } else if a <= -std::f64::consts::PI {
                        a += std::f64::consts::TAU;
                    }
                    a
                }
                _ => v,
            };
            props.insert(key.into(), v.into());
        }
    }
    for (key, value) in [
        ("from", &p.from),
        ("to", &p.to),
        ("head", &p.head),
        ("ink", &p.ink),
        ("fill", &p.fill),
        ("label", &p.label),
        ("stroke_style", &p.stroke_style),
        ("roundness", &p.roundness),
        ("groups", &p.groups),
        ("frame", &p.frame),
    ] {
        if let Some(v) = value {
            let v = rules::text(
                v,
                if key == "groups" {
                    1128
                } else {
                    rules::MAX_LABEL
                },
                key,
            )?;
            props.insert(key.into(), v.into());
        }
    }
    if let Some(raw) = &p.points {
        if sized {
            return Err(invalid("a sized form uses w/h, not points"));
        }
        let points = parse_points(raw);
        if points.len() != raw.split_whitespace().count() {
            return Err(invalid("points must be finite x,y pairs"));
        }
        for (x, y) in &points {
            rules::coordinate(*x, "point x")?;
            rules::coordinate(*y, "point y")?;
        }
        if points.len() > 10000 {
            return Err(invalid("too many input points"));
        }
        let points = model::simplify(&points, 0.9);
        if points.len() < 2 || points.len() > MAX_POINTS {
            return Err(invalid("a stroke needs 2..400 simplified points"));
        }
        props.insert("points".into(), format_points(&points).into());
    }
    let mut record = existing
        .cloned()
        .unwrap_or_else(|| Record::new(format!("sketch-pending-{reference}"), "shape"));
    if existing.is_none() {
        let top = drawing(view).shapes.iter().map(|s| s.z).fold(0.0, f64::max);
        for (key, v) in [
            ("x", 0.0),
            ("y", 0.0),
            ("stroke_width", 2.0),
            ("opacity", 100.0),
            ("font_size", 0.0),
            ("angle", 0.0),
            ("z", top + 1.0),
        ] {
            props.entry(key.into()).or_insert(v.into());
        }
        for (key, v) in [
            ("head", if form == "arrow" { "triangle" } else { "none" }),
            ("ink", "slate"),
            ("fill", "none"),
            ("label", ""),
            ("stroke_style", "solid"),
            (
                "roundness",
                if matches!(form, "rect" | "frame") {
                    "round"
                } else {
                    "sharp"
                },
            ),
            ("groups", ""),
            ("frame", ""),
            ("from", ""),
            ("to", ""),
            ("points", ""),
        ] {
            props.entry(key.into()).or_insert(v.into());
        }
        if sized {
            props.entry("w".into()).or_insert(240.0.into());
            if form != "text" {
                props.entry("h".into()).or_insert(160.0.into());
            }
        }
        rules::check_growth(drawing(view).shapes.len() + 1, 1)?;
    }
    for (key, value) in &props {
        if let Value::Scalar(v) = value {
            record.props.insert(key.clone(), v.clone());
        }
    }
    let candidate = View::new(
        view.revision(),
        view.records()
            .filter(|r| r.id != record.id)
            .cloned()
            .chain(std::iter::once(record.clone())),
    )
    .map_err(|e| invalid(e.to_string()))?;
    check_shape(&record, &candidate)?;
    let op = if let Some(old) = existing {
        set(&old.id, props)
    } else {
        Op::Create {
            reference: reference.into(),
            kind: "shape".into(),
            props,
        }
    };
    Ok((op, record))
}
fn rewrite_refs(op: &mut Op, refs: &BTreeMap<String, String>) {
    if let Op::Create { props, .. } | Op::Set { props, .. } = op {
        for key in ["from", "to", "frame"] {
            if let Some(Value::Scalar(Scalar::Str(id))) = props.get(key) {
                if let Some(reference) = refs.get(id) {
                    props.insert(key.into(), Value::reference(reference));
                }
            }
        }
    }
}
fn sketch(patches: &[JsonValue], view: &View) -> Result<Plan, CapError> {
    if patches.is_empty() {
        return Err(invalid("a sketch needs at least one shape"));
    }
    if view.len() + patches.iter().filter(|p| p["id"].is_null()).count() > 2000 {
        return Err(invalid("this sketch exceeds 2000 objects"));
    }
    let mut working = view.clone();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut refs = BTreeMap::new();
    let mut ops = vec![];
    let mut skipped = vec![];
    let mut drawn = 0;
    for (index, value) in patches.iter().enumerate() {
        let result = (|| {
            let mut patch: ShapePatch =
                serde_json::from_value(value.clone()).map_err(|e| invalid(e.to_string()))?;
            for target in [&mut patch.from, &mut patch.to, &mut patch.frame] {
                if let Some(id) = target.as_ref().and_then(|id| names.get(id)) {
                    *target = Some(id.clone());
                }
            }
            let reference = format!("shape-{index}");
            let (mut op, record) = place(&patch, &working, &reference)?;
            rewrite_refs(&mut op, &refs);
            if let Some(name) = patch.reference.filter(|s| !s.is_empty()) {
                names.insert(name, record.id.clone());
            }
            if patch.id.is_none() {
                refs.insert(record.id.clone(), reference);
            }
            working = View::new(
                view.revision(),
                working
                    .records()
                    .filter(|r| r.id != record.id)
                    .cloned()
                    .chain(std::iter::once(record.clone())),
            )
            .map_err(|e| invalid(e.to_string()))?;
            ops.push(op);
            drawn += 1;
            Ok::<_, CapError>(())
        })();
        if let Err(e) = result {
            if drawn == 0 {
                return Err(invalid(format!("shapes[{index}]: {}", e.message)));
            }
            skipped.push(format!(
                "shapes[{index}] {}: {}",
                value["ref"]
                    .as_str()
                    .or_else(|| value["form"].as_str())
                    .unwrap_or("unnamed"),
                e.message
            ));
            for (j, p) in patches.iter().enumerate().skip(index + 1) {
                skipped.push(format!(
                    "shapes[{j}] {}: not attempted after the refused element",
                    p["ref"]
                        .as_str()
                        .or_else(|| p["form"].as_str())
                        .unwrap_or("unnamed")
                ));
            }
            break;
        }
    }
    Ok(Plan {
        ops,
        readback: json!({"drawn":drawn,"skipped":skipped,"reading":describe(&drawing(&working))})
            .to_string(),
    })
}
fn import(args: &JsonValue, view: &View) -> Result<Plan, CapError> {
    let document = args["document"]
        .as_str()
        .ok_or_else(|| invalid("document must be Excalidraw JSON text"))?;
    if document.len() > 4_000_000 {
        return Err(invalid("import exceeds 4MB"));
    }
    let parsed = excalidraw::from_excalidraw(document).map_err(invalid)?;
    let mut skipped = parsed.skipped;
    let mut notes = parsed.notes;
    let dx = args["dx"].as_f64().unwrap_or(0.0);
    let dy = args["dy"].as_f64().unwrap_or(0.0);
    let mut working = view.clone();
    let mut source_to_local = BTreeMap::new();
    let mut refs = BTreeMap::new();
    let mut locals = vec![];
    let mut ops = vec![];
    for (index, p) in parsed.shapes.iter().enumerate() {
        let mut p = p.clone();
        p.x = Some(p.x.unwrap_or(0.0) + dx);
        p.y = Some(p.y.unwrap_or(0.0) + dy);
        p.from = None;
        p.to = None;
        p.frame = None;
        let reference = format!("shape-{index}");
        match place(&p, &working, &reference) {
            Ok((op, r)) => {
                source_to_local.insert(parsed.source_ids[index].clone(), r.id.clone());
                refs.insert(r.id.clone(), reference);
                locals.push(Some(r.id.clone()));
                working = View::new(
                    view.revision(),
                    working.records().cloned().chain(std::iter::once(r)),
                )
                .map_err(|e| invalid(e.to_string()))?;
                ops.push(op);
            }
            Err(e) => {
                skipped.push(format!(
                    "{} {:?}: {}",
                    p.form.unwrap_or_default(),
                    parsed.source_ids[index],
                    e.message
                ));
                locals.push(None);
            }
        }
    }
    let mut bound = 0;
    for binding in &parsed.bindings {
        let Some(Some(id)) = locals.get(binding.shape_index) else {
            continue;
        };
        let mut props = BTreeMap::new();
        for (key, source) in [("from", &binding.from), ("to", &binding.to)] {
            if let Some(source) = source {
                if let Some(local) = source_to_local.get(source) {
                    props.insert(key.into(), Value::reference(&refs[local]));
                } else {
                    notes.push(format!("connector endpoint {source:?} landed loose because its target did not import"));
                }
            }
        }
        if !props.is_empty() {
            bound += 1;
            ops.push(Op::Set {
                target: Target::Ref(refs[id].clone()),
                props,
            });
        }
    }
    for (index, source) in &parsed.frames {
        let Some(Some(id)) = locals.get(*index) else {
            continue;
        };
        if let Some(frame) = source_to_local.get(source) {
            if drawing(&working)
                .shape(frame)
                .is_some_and(|s| s.form == "frame")
            {
                ops.push(Op::Set {
                    target: Target::Ref(refs[id].clone()),
                    props: [("frame".into(), Value::reference(&refs[frame]))].into(),
                });
            } else {
                notes.push(format!(
                    "frame {source:?} is not a frame; member landed free"
                ));
            }
        } else {
            notes.push(format!(
                "frame {source:?} did not import; member landed free"
            ));
        }
    }
    if refs.is_empty() {
        return Err(invalid(format!(
            "nothing imported; skipped: {}",
            skipped.join("; ")
        )));
    }
    Ok(Plan {
        ops,
        readback:
            json!({"imported":refs.len(),"bound_connectors":bound,"skipped":skipped,"notes":notes})
                .to_string(),
    })
}
pub fn lift_consumed(view: &View, id: &str) -> Result<(Shape, String, Vec<String>), CapError> {
    let d = drawing(view);
    let s = d
        .shape(id)
        .ok_or_else(|| invalid(format!("no shape {id:?}")))?;
    if matches!(s.form.as_str(), "arrow" | "line") {
        return Err(invalid(
            "an arrow relates things rather than being one; lift what it points at",
        ));
    }
    if !FORMS.contains(&s.form.as_str()) {
        return Err(invalid("unsupported sketch form cannot lift"));
    }
    let mut consumed = vec![id.to_string()];
    let title = if !s.label.is_empty() {
        s.label.clone()
    } else if s.is_closed() {
        let texts: Vec<_> = d
            .shapes
            .iter()
            .filter(|t| {
                t.form == "text"
                    && t.id != s.id
                    && s.covers_bounds(t.bounds()) == model::Coverage::Inside
            })
            .collect();
        match texts.len() {
            1 => {
                consumed.push(texts[0].id.clone());
                texts[0].label.clone()
            }
            0 => return Err(invalid("this shape has no words; give it a label first")),
            n => {
                return Err(invalid(format!(
                    "this {} holds {n} texts and a card needs one title",
                    s.form
                )))
            }
        }
    } else {
        return Err(invalid("this shape has no words; give it a label first"));
    };
    Ok((s.clone(), title, consumed))
}
fn lift(view: &View, id: &str) -> Result<Vec<BatchEntry>, CapError> {
    let (s, title, consumed) = lift_consumed(view, id)?;
    let (l, t, r, b) = s.bounds();
    let mut card=Map.plan(&Call{action:"card_place".into(),args:json!({"label":title,"x":l,"y":t,"w":(r-l).clamp(rules::MIN_W,rules::MAX_W),"h_pref":(b-t).clamp(rules::MIN_H,rules::MAX_H)})},view)?;
    for op in &mut card.ops {
        if let Op::Create { reference, .. } = op {
            *reference = "lifted-card".into();
        }
    }
    card.ops.extend(
        samepage_map::retarget_members(view, &consumed, Target::Ref("lifted-card".into()))?.ops,
    );
    let mut ops = vec![];
    for shape in view
        .records()
        .filter(|r| r.kind == "shape" && !consumed.contains(&r.id))
    {
        let mut props = BTreeMap::new();
        for key in ["from", "to"] {
            if consumed
                .iter()
                .any(|id| Some(id.as_str()) == prop_str(shape, key))
            {
                props.insert(key.into(), Value::reference("lifted-card"));
            }
        }
        if consumed
            .iter()
            .any(|id| Some(id.as_str()) == prop_str(shape, "frame"))
        {
            props.insert("frame".into(), "".into());
        }
        if !props.is_empty() {
            ops.push(set(&shape.id, props));
        }
    }
    ops.extend(consumed.iter().map(|id| delete(id)));
    Ok(vec![
        entry("map", card),
        entry(
            ID,
            Plan {
                ops,
                readback: format!("Lifted {title:?} into a card; references follow in this batch."),
            },
        ),
    ])
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Sketch, Sketch);
/// Same projection with real local card bounds supplied as data by the browser.
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn project_with_bounds(view_json: &str, bounds_json: &str) -> String {
    let result = (|| {
        let view: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let bounds: BTreeMap<String, [f64; 4]> = serde_json::from_str(bounds_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let mut d = drawing(&view);
        for n in &mut d.nodes {
            if let Some([l, t, r, b]) = bounds.get(&n.id) {
                if [l, t, r, b].iter().any(|v| !v.is_finite()) || r <= l || b <= t {
                    return Err(invalid("invalid measured card bounds"));
                }
                n.x = *l;
                n.y = *t;
                n.w = r - l;
                n.h = b - t;
            }
        }
        project_drawing(&d)
    })();
    match result {
        Ok(v) => json!({"abi":samepage_capability::ABI,"ok":v}).to_string(),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}).to_string(),
    }
}

/// Detach bindings using the final painted positions from the prior view.
/// The native document uses the same pure rule while committing a deletion.
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn detach_with_before(deleted_json: &str, before_json: &str, view_json: &str) -> String {
    let result = (|| {
        let deleted: Vec<String> = serde_json::from_str(deleted_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let before: View = serde_json::from_str(before_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let view: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        Sketch.cascade_with_before(&deleted, &before, &view)
    })();
    match result {
        Ok(plan) => json!({"abi":samepage_capability::ABI,"ok":plan}).to_string(),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}).to_string(),
    }
}
