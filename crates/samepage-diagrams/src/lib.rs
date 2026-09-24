//! Typed diagrams over Map records, planned and validated as one ABI v2 batch.
pub mod layout;
pub mod readback;
pub mod routing;

use layout::{DiagramKind, PackedRect, Spec};
use readback::{StateMachineView, StateView, TransitionView};
use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CapError, Capability, Change, Delta,
    Descriptor, Effect, LegendEntry, Op, Plan, Record, Scalar, Target, Trigger, Value, View,
};
use samepage_map::rules as map;
use samepage_map::{prop_num, prop_str};
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};

pub const ID: &str = "diagrams";
pub const VERSION: &str = "0.1.0";
pub const NODE_KEYS: &[&str] = &[
    "diagram_kind",
    "state_initial",
    "state_terminal",
    "state_order",
    "path",
    "lines",
];

pub fn descriptor() -> Descriptor {
    let mut draw = ActionDecl::new("diagram_draw", Audience::Both, Effect::Mutate,
        "Draw a typed hierarchy or state machine atomically. Source references are checked by the host before publication.").alias("atlas_diagram");
    draw.input_schema = json!({"type":"object", "required":["nodes"], "properties":{
        "kind":{"enum":["hierarchy","state_machine"]}, "nodes":{"type":"array","minItems":1,"maxItems":128,"items":{"type":"object"}},
        "edges":{"type":"array","maxItems":512,"items":{"type":"object"}},"containers":{"type":"array","maxItems":128,"items":{"type":"object"}},
        "title":{"type":"string"},"direction":{"type":"string"},"clear":{"type":"boolean","default":false,"description":"Explicitly replace Map nodes and edges in the same atomic batch; omitted means append."},"x":{"type":"number"},"y":{"type":"number"}
    },"additionalProperties":false});
    let mut read = ActionDecl::new(
        "diagram_read",
        Audience::Both,
        Effect::Query,
        "Read diagram roles, transitions, hierarchy and problems.",
    );
    read.input_schema = json!({"type":"object","properties":{},"additionalProperties":false});
    Descriptor::new(ID,VERSION,Artifact::Wasm)
        .depends_on(map_owner(), samepage_map::VERSION,"Map creates and owns nodes, edges and containment")
        .extends("node",NODE_KEYS,"Typed diagram roles and source references verified by the native adapter; source keys transfer to sources in D3")
        .extends("edge",map::TRANSITION_KEYS,"Typed ordered state transitions")
        .reads("node").reads("edge").action(draw).action(read)
        .trigger(Trigger::property("node","diagram_kind"))
        .trigger(Trigger::property("node","state_initial"))
        .trigger(Trigger::property("node","state_terminal"))
        .trigger(Trigger::property("edge","event"))
}
fn map_owner() -> &'static str {
    samepage_map::ID
}
fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("invalid", message)
}
fn string<'a>(record: &'a Record, key: &str) -> &'a str {
    prop_str(record, key).unwrap_or_default()
}
fn flag(record: &Record, key: &str) -> bool {
    matches!(record.props.get(key), Some(Scalar::Bool(true)))
}
fn props(values: impl IntoIterator<Item = (String, Value)>) -> BTreeMap<String, Value> {
    values.into_iter().collect()
}
fn set(reference: &str, values: BTreeMap<String, Value>) -> Op {
    Op::Set {
        target: Target::Ref(reference.into()),
        props: values,
    }
}
fn entry(id: &str, ops: Vec<Op>, readback: String) -> BatchEntry {
    BatchEntry {
        capability: id.into(),
        plan: Plan { ops, readback },
    }
}
fn create(reference: &str, label: &str, rect: PackedRect) -> Op {
    Op::Create {
        reference: reference.into(),
        kind: "node".into(),
        props: props([
            ("label".into(), label.into()),
            ("x".into(), rect.x.into()),
            ("y".into(), rect.y.into()),
            ("w".into(), rect.w.into()),
            ("h_pref".into(), rect.h.into()),
        ]),
    }
}

pub struct Diagrams;
impl Capability for Diagrams {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, view: &View) -> Result<JsonValue, CapError> {
        projection(view)
    }
    fn digest(&self, view: &View) -> Result<JsonValue, CapError> {
        projection(view)
    }
    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        if call.action == "diagram_read" {
            if call.args.as_object().is_none_or(|v| !v.is_empty()) {
                return Err(invalid("diagram_read takes no arguments"));
            }
            return Ok(Plan {
                ops: vec![],
                readback: describe(view)?,
            });
        }
        Err(CapError::new(
            "composed_batch",
            "diagram_draw requires plan_batch",
        ))
    }
    fn plan_batch(&self, call: &Call, view: &View) -> Result<Batch, CapError> {
        if call.action == "diagram_read" {
            return Ok(Batch {
                base_revision: view.revision().into(),
                entries: vec![BatchEntry {
                    capability: ID.into(),
                    plan: self.plan(call, view)?,
                }],
            });
        }
        if call.action != "diagram_draw" {
            return Err(CapError::new("unknown_action", &call.action));
        }
        draw(&call.args, view)
    }
    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        // Include incident edges when their endpoint or containing machine changes.
        let changed: BTreeSet<_> = delta
            .created
            .iter()
            .chain(delta.modified.keys())
            .chain(delta.deleted.iter())
            .collect();
        for record in view.records() {
            if record.kind == "node"
                && (changed.contains(&record.id)
                    || changed
                        .iter()
                        .any(|id| string(record, "parent") == id.as_str()))
            {
                if let Some(kind) = record.props.get("diagram_kind") {
                    if !matches!(kind.as_str(), Some("hierarchy" | "state_machine" | "")) {
                        return Err(invalid("diagram_kind must be hierarchy or state_machine"));
                    }
                }
                for key in ["state_initial", "state_terminal"] {
                    if record
                        .props
                        .get(key)
                        .is_some_and(|v| !matches!(v, Scalar::Bool(_)))
                    {
                        return Err(invalid(format!("{key} must be boolean")));
                    }
                    if record.props.contains_key(key)
                        && view
                            .get(string(record, "parent"))
                            .is_none_or(|p| string(p, "diagram_kind") != "state_machine")
                    {
                        return Err(invalid("state roles require a state-machine parent"));
                    }
                }
                check_order(record, "state_order")?;
                check_binding(record)?;
            }
            if record.kind == "edge" && map::is_transition(record) {
                let from = view.get(string(record, "from"));
                let to = view.get(string(record, "to"));
                if !changed.contains(&record.id)
                    && !changed.iter().any(|id| {
                        from.is_some_and(|r| &r.id == *id || string(r, "parent") == id.as_str())
                            || to
                                .is_some_and(|r| &r.id == *id || string(r, "parent") == id.as_str())
                    })
                {
                    continue;
                }
                let (Some(from), Some(to)) = (from, to) else {
                    return Err(invalid("transition endpoints must exist"));
                };
                if from.kind != "node" || to.kind != "node" {
                    return Err(invalid("transition endpoints must be cards"));
                }
                for state in [from, to] {
                    if view
                        .get(string(state, "parent"))
                        .is_none_or(|p| string(p, "diagram_kind") != "state_machine")
                    {
                        return Err(invalid(
                            "transition endpoints require a state-machine parent",
                        ));
                    }
                }
                let event = record
                    .props
                    .get("event")
                    .and_then(Scalar::as_str)
                    .ok_or_else(|| invalid("transition event is required"))?;
                if event.is_empty() || map::text(event, map::MAX_LABEL, "event")? != event {
                    return Err(invalid("transition event must be nonempty trimmed text"));
                }
                if let Some(guard) = record.props.get("guard") {
                    let guard = guard
                        .as_str()
                        .ok_or_else(|| invalid("guard must be text"))?;
                    if map::text(guard, map::MAX_NOTE, "guard")? != guard {
                        return Err(invalid("guard must be trimmed"));
                    }
                }
                if !record.props.contains_key("transition_order") {
                    return Err(invalid("transition_order is required"));
                }
                check_order(record, "transition_order")?;
            }
        }
        Ok(())
    }
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        self.validate(change.delta, change.after)?;
        for (id, keys) in change.attempted {
            let Some(record) = change.after.get(id) else {
                continue;
            };
            if record.kind == "node"
                && keys.iter().any(|k| k == "path" || k == "lines")
                && !string(record, "path").is_empty()
            {
                let expected = json!({"path":string(record,"path"),"lines":string(record,"lines")});
                if change.facts.and_then(|facts| facts.get(id)) != Some(&expected) {
                    return Err(CapError::new(
                        "source_unverified",
                        format!("{id} source reference needs the native verifier"),
                    ));
                }
            }
        }
        Ok(())
    }
    fn changes(
        &self,
        before: &JsonValue,
        after: &JsonValue,
        _reader: &str,
    ) -> Result<Vec<String>, CapError> {
        let mut lines = vec![];
        for edge in after["transitions"].as_array().into_iter().flatten() {
            if let Some(old) = before["transitions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|old| old["id"] == edge["id"])
            {
                if old["sentence"] != edge["sentence"] {
                    lines.push(format!(
                        "- [{}] the transition now reads {}; it was {}",
                        edge["id"].as_str().unwrap_or_default(),
                        edge["sentence"].as_str().unwrap_or_default(),
                        old["sentence"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        Ok(lines)
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            ("initial", "Initial state", "●"),
            ("terminal", "Terminal state", "◎"),
        ]
        .into_iter()
        .map(|(value, label, symbol)| LegendEntry {
            capability: ID.into(),
            attribute: "state_role".into(),
            value: value.into(),
            label: label.into(),
            class: Some(format!("diagram-{value}")),
            symbol: Some(symbol.into()),
        })
        .collect())
    }
}
fn check_order(record: &Record, key: &str) -> Result<(), CapError> {
    if let Some(value) = record.props.get(key) {
        let n = value
            .as_f64()
            .ok_or_else(|| invalid(format!("{key} must be an integer")))?;
        if n < 0.0 || n.fract() != 0.0 || n > 9_007_199_254_740_991.0 {
            return Err(invalid(format!("{key} must be a nonnegative safe integer")));
        }
    }
    Ok(())
}
pub fn check_binding(record: &Record) -> Result<(), CapError> {
    for key in ["path", "lines"] {
        if record.props.get(key).is_some_and(|v| v.as_str().is_none()) {
            return Err(invalid(format!("{key} must be text")));
        }
    }
    if string(record, "path").is_empty() != string(record, "lines").is_empty() {
        return Err(invalid("source references require both path and lines"));
    }
    Ok(())
}

fn draw(args: &JsonValue, view: &View) -> Result<Batch, CapError> {
    let object = args
        .as_object()
        .ok_or_else(|| invalid("diagram requires an object"))?;
    for key in object.keys() {
        if ![
            "kind",
            "nodes",
            "edges",
            "containers",
            "title",
            "direction",
            "clear",
            "x",
            "y",
        ]
        .contains(&key.as_str())
        {
            return Err(invalid(format!("unknown diagram field {key}")));
        }
    }
    if args["nodes"].as_array().is_some_and(|n| n.len() > 128)
        || args["edges"].as_array().is_some_and(|e| e.len() > 512)
        || args["containers"].as_array().is_some_and(|c| c.len() > 128)
    {
        return Err(invalid(
            "diagram exceeds bounded node, container or edge limit",
        ));
    }
    let mut spec = Spec::parse(args).map_err(invalid)?;
    // Like the retained editable Map action, append unless clearing is explicit.
    spec.clear = args
        .get("clear")
        .map(|v| v.as_bool().ok_or_else(|| invalid("clear must be boolean")))
        .transpose()?
        .unwrap_or(false);
    let raw = args["nodes"]
        .as_array()
        .ok_or_else(|| invalid("nodes must be an array"))?;
    for node in raw {
        for key in node.as_object().into_iter().flat_map(|o| o.keys()) {
            if ![
                "id", "label", "color", "group", "size", "initial", "terminal", "note", "path",
                "lines", "shape", "filled", "emphasis", "tone", "status",
            ]
            .contains(&key.as_str())
            {
                return Err(invalid(format!("unknown node field {key}")));
            }
        }
    }
    let mut sizes = Vec::new();
    let mut styles = Vec::new();
    for (node, raw) in spec.nodes.iter().zip(raw) {
        let mut style = BTreeMap::new();
        let note = optional_text(raw, "note")?.unwrap_or_default();
        let scale = match node.size {
            layout::NodeSize::Normal => 1.0,
            layout::NodeSize::Primary => 1.35,
            layout::NodeSize::Hero => 1.8,
        };
        let width = 232.0 * scale;
        let height = samepage_map::estimated_height(&node.label, &note, width).max(150.0) * scale;
        sizes.push((width, height));
        style.insert("note".into(), note.into());
        style.insert(
            "size".into(),
            match node.size {
                layout::NodeSize::Normal => "normal",
                layout::NodeSize::Primary => "primary",
                layout::NodeSize::Hero => "hero",
            }
            .into(),
        );
        for key in ["tone", "status", "emphasis"] {
            if let Some(value) = optional_text(raw, key)? {
                style.insert(key.into(), value.into());
            }
        }
        if let Some(shape) = optional_text(raw, "shape")? {
            style.insert("node_kind".into(), shape.into());
        }
        if let Some(color) = node.color.as_deref() {
            style.insert("node_color".into(), normalize_color(color)?.into());
        }
        if let Some(filled) = raw.get("filled") {
            let filled = filled
                .as_bool()
                .ok_or_else(|| invalid("filled must be boolean"))?;
            if filled {
                style.insert("emphasis".into(), "strong".into());
            }
        }
        styles.push(style);
    }
    let ox = match args.get("x") {
        Some(v) => v.as_f64().ok_or_else(|| invalid("x must be a number"))?,
        None => 48.0,
    };
    let oy = match args.get("y") {
        Some(v) => v.as_f64().ok_or_else(|| invalid("y must be a number"))?,
        None if spec.clear => 48.0,
        None => view
            .records()
            .filter(|r| r.kind == "node")
            .map(|r| {
                prop_num(r, "y").unwrap_or(0.0) + prop_num(r, "h_pref").unwrap_or(150.0) + 96.0
            })
            .fold(48.0, f64::max),
    };
    let mut map_ops = vec![];
    let mut typed = vec![];
    if spec.clear {
        for kind in ["edge", "node"] {
            for record in view.records().filter(|r| r.kind == kind) {
                map_ops.push(Op::Delete {
                    target: Target::Id(record.id.clone()),
                });
            }
        }
    }
    let shifted = |r: PackedRect| PackedRect {
        x: r.x + ox,
        y: r.y + oy,
        ..r
    };
    let node_ref = |i: usize| format!("node-{i}");
    let container_ref = |i: usize| format!("container-{i}");
    let (nodes, containers) = if spec.kind == DiagramKind::StateMachine {
        let laid = layout::layout_state_machine(&sizes, &spec.edge_pairs(), spec.direction, 96.0)
            .map_err(invalid)?;
        let root = PackedRect {
            x: 0.0,
            y: 0.0,
            w: laid.width + 96.0,
            h: laid.height + 140.0,
        };
        let nodes = laid
            .nodes
            .into_iter()
            .map(|r| PackedRect {
                x: r.x + 48.0,
                y: r.y + 92.0,
                ..r
            })
            .collect();
        (nodes, vec![root])
    } else {
        let index = |id: &str| {
            spec.containers
                .iter()
                .position(|c| c.id == id)
                .ok_or_else(|| invalid(format!("unknown container {id:?}")))
        };
        let parents = spec
            .containers
            .iter()
            .map(|c| c.parent.as_deref().map(index).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        let node_parents = spec
            .nodes
            .iter()
            .map(|n| n.group.as_deref().map(index).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        let viewport = if spec.direction == layout::Direction::Down {
            (520.0, 1200.0)
        } else {
            (1000.0, 640.0)
        };
        let laid = layout::layout_hierarchy(
            &sizes,
            &node_parents,
            &parents,
            viewport,
            (700.0, 300.0),
            56.0,
            96.0,
            32.0,
            68.0,
        )
        .map_err(invalid)?;
        (laid.nodes, laid.containers)
    };
    // Create every container first. Parent refs are assigned after all creates,
    // so input order need not be topological and no forward reference escapes.
    for (i, rect) in containers.iter().enumerate() {
        let label = if spec.kind == DiagramKind::StateMachine {
            spec.title.as_deref().unwrap_or("State machine")
        } else {
            &spec.containers[i].label
        };
        map_ops.push(create(&container_ref(i), label, shifted(*rect)));
        typed.push(set(
            &container_ref(i),
            props([("diagram_kind".into(), spec.kind.as_str().into())]),
        ));
    }
    for (i, (node, rect)) in spec.nodes.iter().zip(nodes).enumerate() {
        map_ops.push(create(&node_ref(i), &node.label, shifted(rect)));
        map_ops.push(set(&node_ref(i), styles[i].clone()));
        let mut p = BTreeMap::new();
        if spec.kind == DiagramKind::StateMachine {
            map_ops.push(set(
                &node_ref(i),
                props([("parent".into(), Value::reference(&container_ref(0)))]),
            ));
            p.insert("state_initial".into(), node.initial.into());
            p.insert("state_terminal".into(), node.terminal.into());
            p.insert("state_order".into(), (i as f64).into());
        } else {
            p.insert("diagram_kind".into(), "hierarchy".into());
            if let Some(group) = &node.group {
                let index = spec
                    .containers
                    .iter()
                    .position(|c| &c.id == group)
                    .ok_or_else(|| invalid("unknown node group"))?;
                map_ops.push(set(
                    &node_ref(i),
                    props([("parent".into(), Value::reference(&container_ref(index)))]),
                ));
            }
        }
        for key in ["path", "lines"] {
            if let Some(value) = optional_text(&raw[i], key)? {
                p.insert(key.into(), value.into());
            }
        }
        typed.push(set(&node_ref(i), p));
    }
    for (i, container) in spec.containers.iter().enumerate() {
        if let Some(parent) = &container.parent {
            let index = spec
                .containers
                .iter()
                .position(|c| &c.id == parent)
                .ok_or_else(|| invalid("unknown parent"))?;
            map_ops.push(set(
                &container_ref(i),
                props([("parent".into(), Value::reference(&container_ref(index)))]),
            ));
        }
    }
    for (i, edge) in spec.edges.iter().enumerate() {
        let reference = format!("edge-{i}");
        map_ops.push(Op::Create {
            reference: reference.clone(),
            kind: "edge".into(),
            props: props([
                ("from".into(), Value::reference(&node_ref(edge.from))),
                ("to".into(), Value::reference(&node_ref(edge.to))),
                (
                    "label".into(),
                    edge.label.as_deref().unwrap_or_default().into(),
                ),
            ]),
        });
        if spec.kind == DiagramKind::StateMachine {
            typed.push(set(
                &reference,
                props([
                    (
                        "event".into(),
                        edge.event.as_deref().unwrap_or_default().into(),
                    ),
                    (
                        "guard".into(),
                        edge.guard.as_deref().unwrap_or_default().into(),
                    ),
                    ("transition_order".into(), (i as f64 + 1.0).into()),
                ]),
            ));
        }
    }
    Ok(Batch {
        base_revision: view.revision().into(),
        entries: vec![
            entry(map_owner(), map_ops, String::new()),
            entry(
                ID,
                typed,
                format!(
                    "Drew {}: {} nodes, {} edges, {} containers.",
                    spec.kind.as_str(),
                    spec.nodes.len(),
                    spec.edges.len(),
                    containers.len()
                ),
            ),
        ],
    })
}
fn optional_text(raw: &JsonValue, key: &str) -> Result<Option<String>, CapError> {
    raw.get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| invalid(format!("{key} must be text")))
        })
        .transpose()
}
fn normalize_color(value: &str) -> Result<String, CapError> {
    if map::INKS.contains(&value) {
        return Ok(value.into());
    }
    if value.len() == 7 && value.starts_with('#') {
        let rgb = u32::from_str_radix(&value[1..], 16).map_err(|_| invalid("invalid color"))?;
        let colors = [
            ("slate", 0x94a3b8u32),
            ("blue", 0x60a5fa),
            ("green", 0x4ade80),
            ("amber", 0xfbbf24),
            ("red", 0xf87171),
            ("violet", 0xa78bfa),
        ];
        let nearest = colors.iter().min_by_key(|(_, color)| {
            [0, 8, 16]
                .into_iter()
                .map(|shift| {
                    let d = ((rgb >> shift) & 255) as i32 - ((color >> shift) & 255) as i32;
                    d * d
                })
                .sum::<i32>()
        });
        if let Some((name, _)) = nearest {
            return Ok((*name).into());
        }
    }
    Err(invalid("color must be a palette name or six-digit hex"))
}

pub fn machines(view: &View) -> Vec<StateMachineView> {
    view.records()
        .filter(|r| r.kind == "node" && string(r, "diagram_kind") == "state_machine")
        .map(|root| {
            let mut children = view
                .records()
                .filter(|r| r.kind == "node" && string(r, "parent") == root.id)
                .collect::<Vec<_>>();
            children.sort_by(|a, b| {
                prop_num(a, "state_order")
                    .unwrap_or(0.0)
                    .total_cmp(&prop_num(b, "state_order").unwrap_or(0.0))
                    .then(a.id.cmp(&b.id))
            });
            let states = children
                .iter()
                .map(|r| StateView {
                    id: r.id.clone(),
                    label: string(r, "label").into(),
                    initial: flag(r, "state_initial"),
                    terminal: flag(r, "state_terminal"),
                })
                .collect();
            let mut transitions = view
                .records()
                .filter(|r| {
                    r.kind == "edge"
                        && map::is_transition(r)
                        && children.iter().any(|n| n.id == string(r, "from"))
                })
                .map(|r| {
                    let from = view.get(string(r, "from"));
                    let to = view.get(string(r, "to"));
                    let parent = to.and_then(|n| view.get(string(n, "parent")));
                    TransitionView {
                        id: r.id.clone(),
                        from: string(r, "from").into(),
                        to: string(r, "to").into(),
                        from_label: from.map(|n| string(n, "label")).unwrap_or_default().into(),
                        to_label: to.map(|n| string(n, "label")).unwrap_or_default().into(),
                        to_machine: parent.map(|p| p.id.as_str()).unwrap_or_default().into(),
                        to_machine_label: parent
                            .map(|p| string(p, "label"))
                            .unwrap_or_default()
                            .into(),
                        event: string(r, "event").into(),
                        guard: string(r, "guard").into(),
                        order: prop_num(r, "transition_order").unwrap_or(0.0) as u64,
                    }
                })
                .collect::<Vec<_>>();
            transitions.sort_by(|a, b| a.order.cmp(&b.order).then(a.id.cmp(&b.id)));
            StateMachineView {
                id: root.id.clone(),
                title: string(root, "label").into(),
                states,
                transitions,
                current_state: None,
            }
        })
        .collect()
}
pub fn describe(view: &View) -> Result<String, CapError> {
    let mut text = String::new();
    for machine in machines(view) {
        text.push_str(&machine.describe());
        for problem in machine.problems() {
            text.push_str(&format!("- PROBLEM: {}\n", problem.detail));
        }
    }
    fn branch(view: &View, record: &Record, depth: usize, text: &mut String) {
        if depth > map::MAX_DEPTH {
            return;
        }
        text.push_str(&format!(
            "{}- {}\n",
            "  ".repeat(depth),
            string(record, "label")
        ));
        for child in view
            .records()
            .filter(|r| r.kind == "node" && string(r, "parent") == record.id)
        {
            branch(view, child, depth + 1, text);
        }
    }
    let roots = view.records().filter(|r| {
        r.kind == "node"
            && string(r, "diagram_kind") == "hierarchy"
            && view
                .get(string(r, "parent"))
                .is_none_or(|p| string(p, "diagram_kind") != "hierarchy")
    });
    for root in roots {
        text.push_str("\nHIERARCHY\n");
        branch(view, root, 0, &mut text);
    }
    Ok(text)
}
type RenderBounds = BTreeMap<String, (f64, f64, f64, f64)>;

fn projection(view: &View) -> Result<JsonValue, CapError> {
    projection_with_bounds(view, None)
}

/// Additional rendering export. The bounds are local observations, never a
/// plan or a shared-document write. Routing still runs in the portable crate.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn project_with_bounds(view_json: &str, bounds_json: &str) -> String {
    let result = (|| {
        let view: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", format!("view: {e}")))?;
        let bounds: RenderBounds = serde_json::from_str(bounds_json)
            .map_err(|e| CapError::new("bad_input", format!("bounds: {e}")))?;
        projection_with_bounds(&view, Some(&bounds))
    })();
    match result {
        Ok(value) => json!({"abi":samepage_capability::ABI,"ok":value}).to_string(),
        Err(error) => json!({"abi":samepage_capability::ABI,"error":error}).to_string(),
    }
}

fn projection_with_bounds(
    view: &View,
    bounds: Option<&RenderBounds>,
) -> Result<JsonValue, CapError> {
    let map = samepage_map::Map.project(view)?;
    let cards = map["cards"]
        .as_array()
        .ok_or_else(|| invalid("Map projection omitted cards"))?;
    if let Some(bounds) = bounds {
        if bounds.len() != cards.len()
            || cards
                .iter()
                .any(|c| c["id"].as_str().is_none_or(|id| !bounds.contains_key(id)))
        {
            return Err(invalid(
                "local rendering bounds must name every Map card exactly once",
            ));
        }
        for &(left, top, right, bottom) in bounds.values() {
            if ![left, top, right, bottom]
                .iter()
                .all(|n| n.is_finite() && n.abs() <= 1_000_000.0)
                || left >= right
                || top >= bottom
            {
                return Err(invalid(
                    "local rendering bounds must be finite positive rectangles",
                ));
            }
        }
    }
    let box_of = |id: &str| {
        if let Some(bounds) = bounds {
            return bounds.get(id).copied();
        }
        cards.iter().find(|r| r["id"] == id).map(|r| {
            let n = |k: &str| r[k].as_f64().unwrap_or(0.0);
            (n("x"), n("y"), n("x") + n("width"), n("y") + n("height"))
        })
    };
    let machines = machines(view);
    let mut transitions = machines
        .iter()
        .flat_map(|m| m.transitions.iter())
        .collect::<Vec<_>>();
    transitions.sort_by(|a, b| a.order.cmp(&b.order).then(a.id.cmp(&b.id)));
    let connections = transitions
        .iter()
        .filter_map(|e| {
            Some(routing::Connection {
                id: &e.id,
                from: &e.from,
                to: &e.to,
                from_box: box_of(&e.from)?,
                to_box: box_of(&e.to)?,
            })
        })
        .collect::<Vec<_>>();
    let obstacles = cards
        .iter()
        .filter(|c| !cards.iter().any(|child| child["parent"] == c["id"]))
        .filter_map(|c| box_of(c["id"].as_str()?))
        .collect::<Vec<_>>();
    let paths = routing::routes(&connections, &obstacles);
    let routes: BTreeMap<_, _> = connections
        .iter()
        .zip(paths)
        .map(|(c, p)| (c.id, p))
        .collect();
    let edges=transitions.iter().map(|e|json!({"id":e.id,"from":e.from,"to":e.to,"event":e.event,"guard":e.guard,"order":e.order,"sentence":e.sentence(),"points":routes.get(e.id.as_str())})).collect::<Vec<_>>();
    let states = machines
        .iter()
        .flat_map(|m| m.states.iter())
        .collect::<Vec<_>>();
    let containers = view
        .records()
        .filter(|r| {
            r.kind == "node"
                && !string(r, "diagram_kind").is_empty()
                && view.records().any(|c| string(c, "parent") == r.id)
        })
        .map(|r| json!({"id":r.id,"kind":string(r,"diagram_kind")}))
        .collect::<Vec<_>>();
    let problems = machines
        .iter()
        .flat_map(StateMachineView::problems)
        .collect::<Vec<_>>();
    Ok(
        json!({"machines":machines,"states":states,"containers":containers,"transitions":edges,"problems":problems,"readback":describe(view)?}),
    )
}
#[cfg(target_arch = "wasm32")]
samepage_capability::export_capability!(Diagrams, Diagrams);
