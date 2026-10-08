//! The map capability: cards and the links between them.
//!
//! Cards are `node` records and links are `edge` records, the kinds and key
//! names the Atlas already writes, so existing documents project without
//! migration. The capability owns only the keys in [`rules::NODE_KEYS`] and
//! [`rules::EDGE_KEYS`]. Keys other capabilities own on the same records
//! (source bindings, typed-diagram roles, cement state) are never written here
//! and are listed, not interpreted, in the projection.
//!
//! This is the minimal slice of the map: placing, moving, measuring, linking
//! and removing cards, with validated geometry and containment, projection and
//! read-back. Constraints, layout problems beyond the ones reported here, page
//! checks and imports come with the full map later.

pub mod rules;
pub mod validation;

use std::collections::{BTreeMap, BTreeSet};

use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CapError, Capability, Change, Delta, Descriptor, Effect,
    LegendEntry, Op, Plan, Record, Scalar, Target, Trigger, Value, View,
};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use rules::*;

pub const ID: &str = "map";
pub const VERSION: &str = "0.2.0";

pub fn descriptor() -> Descriptor {
    let place_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": { "type": "string" },
            "label": { "type": "string", "maxLength": MAX_LABEL },
            "note": { "type": "string", "maxLength": MAX_NOTE },
            "tone": { "enum": TONES },
            "status": { "enum": STATUSES },
            "x": { "type": "number" }, "y": { "type": "number" },
            "w": { "type": "number" }, "h_pref": { "type": "number" },
            "w_min": { "type": "number" }, "h_min": { "type": "number" },
            "parent": { "type": "string" },
            "color": { "type": "string" },
            "emphasis": { "enum": EMPHASES },
            "size": { "enum": SIZES },
            "kind": { "enum": SHAPES },
            "path": { "type": "string", "description": "Owned by the sources capability; refused here until the composite adapter exists." },
            "lines": { "type": "string", "description": "Owned by the sources capability; refused here until the composite adapter exists." }
        }
    });
    let mut place = ActionDecl::new(
        "card_place",
        Audience::Both,
        Effect::Mutate,
        "Create a card (label required) or update one by id.",
    )
    .alias("atlas_place");
    place.input_schema = place_schema;
    let mut moving = ActionDecl::new("card_move", Audience::Both, Effect::Mutate, "Move a card.");
    moving.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["id", "x", "y"],
        "properties": { "id": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" } }
    });
    let mut resize = ActionDecl::new(
        "card_resize",
        Audience::Both,
        Effect::Mutate,
        "Set a card's authored size: preferred width and height, and minimums.",
    );
    resize.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["id"],
        "properties": {
            "id": { "type": "string" },
            "w": { "type": "number" }, "h_pref": { "type": "number" },
            "w_min": { "type": "number" }, "h_min": { "type": "number" }
        }
    });
    let mut link = ActionDecl::new(
        "card_link",
        Audience::Both,
        Effect::Mutate,
        "Link two cards.",
    )
    .alias("atlas_link");
    link.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["from", "to"],
        "properties": { "from": { "type": "string" }, "to": { "type": "string" }, "label": { "type": "string" } }
    });
    let mut remove = ActionDecl::new(
        "object_remove",
        Audience::Both,
        Effect::Mutate,
        "Remove a card (with its links) or a link.",
    )
    .alias("atlas_remove");
    remove.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["id"],
        "properties": { "id": { "type": "string" } }
    });
    let mut read = ActionDecl::new(
        "map_read",
        Audience::Both,
        Effect::Query,
        "Describe the cards and links.",
    );
    read.input_schema =
        json!({ "type": "object", "additionalProperties": false, "properties": {} });

    let mut constrain = ActionDecl::new(
        "constraint_add",
        Audience::Both,
        Effect::Mutate,
        "Constrain cards: group, sequence, attaches, voids or labels.",
    )
    .alias("atlas_constrain");
    constrain.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["op", "members"],
        "properties": {
            "op": { "enum": CONSTRAINT_OPS },
            "members": { "type": "array", "items": { "type": "string" } },
            "axis": { "enum": CONSTRAINT_AXES },
            "text": { "type": "string", "maxLength": MAX_TEXT },
            "salience": { "enum": CONSTRAINT_SALIENCES }
        }
    });
    let mut unconstrain = ActionDecl::new(
        "constraint_remove",
        Audience::Both,
        Effect::Mutate,
        "Remove a constraint and its memberships.",
    )
    .alias("atlas_unconstrain");
    unconstrain.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["id"],
        "properties": { "id": { "type": "string" } }
    });
    let mut retarget = ActionDecl::new(
        "constraint_member_retarget",
        Audience::Both,
        Effect::Mutate,
        "Point one membership at a different object.",
    );
    retarget.input_schema = json!({
        "type": "object", "additionalProperties": false, "required": ["id", "target"],
        "properties": { "id": { "type": "string" }, "target": { "type": "string" } }
    });

    Descriptor::new(ID, VERSION, Artifact::Wasm)
        .owns(
            NODE,
            NODE_KEYS,
            "cards: text, style, geometry and containment",
        )
        .owns(EDGE, EDGE_KEYS, "links between two cards")
        .owns(
            CONSTRAINT,
            CONSTRAINT_KEYS,
            "what must hold between objects: group, sequence, attaches, voids, labels",
        )
        .owns(
            CONSTRAINT_MEMBER,
            CONSTRAINT_MEMBER_KEYS,
            "one membership of one constraint, so each target is a whole property",
        )
        .action(place)
        .action(moving)
        .action(resize)
        .action(link)
        .action(remove)
        .action(constrain)
        .action(unconstrain)
        .action(retarget)
        .action(read)
        .trigger(Trigger::Always)
}

pub struct Map;

impl Capability for Map {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }

    fn project(&self, view: &View) -> Result<JsonValue, CapError> {
        Ok(projection(view))
    }

    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        let args = arguments(&call.args)?;
        match call.action.as_str() {
            "card_place" => place(&args, view),
            "card_move" => {
                allow_only(&args, &["id", "x", "y"])?;
                let id = card_id(&args, view)?;
                let x = coordinate(required_number(&args, "x")?, "x")?;
                let y = coordinate(required_number(&args, "y")?, "y")?;
                Ok(plan(
                    vec![set(&id, vec![(K_X, x.into()), (K_Y, y.into())])],
                    format!("Moved {} to ({x}, {y}).", label_of(view, &id)),
                ))
            }
            "card_resize" => resize(&args, view),
            "card_link" => link(&args, view),
            "constraint_add" => constrain(&args, view),
            "constraint_remove" => unconstrain(&args, view),
            "constraint_member_retarget" => retarget(&args, view),
            "object_remove" => remove(&args, view),
            "map_read" => {
                allow_only(&args, &[])?;
                Ok(plan(Vec::new(), describe(view)))
            }
            other => Err(err(
                "unknown_action",
                format!("map has no action {other:?}"),
            )),
        }
    }

    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        let all: BTreeSet<String> = BTreeSet::new();
        let created = delta
            .created
            .iter()
            .filter(|id| {
                view.get(id)
                    .is_some_and(|r| r.kind == NODE || r.kind == EDGE)
            })
            .count();
        check_growth(map_count(view), created)?;
        for id in &delta.created {
            if let Some(record) = view.get(id) {
                check(record, &all, true, view)?;
            }
        }
        for (id, keys) in &delta.modified {
            if let Some(record) = view.get(id) {
                check(record, keys, false, view)?;
            }
        }
        // A card may not leave links or children pointing at nothing.
        for id in &delta.deleted {
            for record in view.records() {
                let refers = match record.kind.as_str() {
                    EDGE => [K_FROM, K_TO]
                        .iter()
                        .any(|key| prop_str(record, key) == Some(id.as_str())),
                    NODE => prop_str(record, K_PARENT) == Some(id.as_str()),
                    _ => false,
                };
                if refers {
                    return Err(err(
                        "dangling",
                        format!("{} still refers to removed card {id}; remove or detach it in the same change", record.id),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The authority hook. It sees what the change *attempted*, which includes
    /// writes whose value equals the old one and which therefore leave no
    /// trace in the delta. A same-value write to the legacy `h` of an imported
    /// card is still a write: it would carry a fresh byline and would teach a
    /// peer that the key is writable. Both are refused here, on every route.
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        for (id, keys) in change.attempted {
            if !keys.contains(K_H) {
                continue;
            }
            let kind = change
                .after
                .get(id)
                .or_else(|| change.before.get(id))
                .map(|record| record.kind.as_str());
            if kind == Some(NODE) {
                return Err(refuse_h_write(id));
            }
        }
        // The frozen scalar and the one-way marker. Both are checked against
        // what the change *attempted*, so an empty or same-value write is
        // refused too: each is an attempt to own history, and a writer that
        // believes it owns the key is a writer that will change it.
        for (id, keys) in change.attempted {
            let before = change.before.get(id);
            let after = change.after.get(id);
            let is_constraint = after
                .or(before)
                .is_some_and(|record| record.kind == CONSTRAINT);
            if !is_constraint {
                continue;
            }
            if keys.contains(K_MEMBERS) && before.is_some_and(converted) {
                return Err(err(
                    "read_only",
                    format!(
                        "{id}.{K_MEMBERS} is the frozen legacy membership of a converted \
                         constraint and is read-only; write its memberships instead"
                    ),
                ));
            }
            if keys.contains(K_MEMBER_SCHEMA) && before.is_some_and(converted) && after.is_some() {
                let still = after.is_some_and(converted);
                if !still {
                    return Err(err(
                        "read_only",
                        format!(
                            "{id}.{K_MEMBER_SCHEMA} cannot be cleared while the constraint \
                             lives; clearing it would reinstate the frozen membership"
                        ),
                    ));
                }
            }
        }
        // Membership is differential: refuse what this change introduces,
        // never what it inherits. Derived from before, after and the deleted
        // set; no action name or entry label is consulted, because a replica
        // frame carries neither.
        let deleted: BTreeSet<String> = change.delta.deleted.iter().cloned().collect();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for id in change
            .delta
            .created
            .iter()
            .chain(change.delta.modified.keys())
            .chain(change.attempted.keys())
        {
            match change.after.get(id).or_else(|| change.before.get(id)) {
                Some(record) if record.kind == CONSTRAINT => {
                    touched.insert(id.clone());
                }
                Some(record) if record.kind == CONSTRAINT_MEMBER => {
                    for view in [change.before, change.after] {
                        if let Some(owner) = view
                            .get(id)
                            .and_then(|member| member.props.get(K_CONSTRAINT))
                            .and_then(Scalar::as_str)
                        {
                            touched.insert(owner.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        for id in &deleted {
            if let Some(record) = change.before.get(id) {
                if record.kind == CONSTRAINT_MEMBER {
                    if let Some(owner) = record.props.get(K_CONSTRAINT).and_then(Scalar::as_str) {
                        touched.insert(owner.to_string());
                    }
                }
            }
            // A deleted target may leave a constraint short: that shortfall is
            // inherited from the deletion and stays a diagnostic.
            for record in change.before.records() {
                if record.kind == CONSTRAINT_MEMBER
                    && record.props.get(K_TARGET).and_then(Scalar::as_str) == Some(id.as_str())
                {
                    if let Some(owner) = record.props.get(K_CONSTRAINT).and_then(Scalar::as_str) {
                        touched.insert(owner.to_string());
                    }
                }
            }
        }
        for id in touched {
            let Some(record) = change.after.get(&id) else {
                continue;
            };
            let keys = change.delta.modified.get(&id).cloned().unwrap_or_default();
            let created = change.delta.created.contains(&id);
            let op = check_constraint(record, &keys, created)?;
            let membership = membership_change(&id, change.before, change.after, &deleted);
            check_membership(&id, &op, &membership, change.after)?;
        }
        self.validate(change.delta, change.after)
    }

    fn cascade(&self, deleted: &[String], view: &View) -> Result<Plan, CapError> {
        Ok(plan(cascade_ops(deleted, view), String::new()))
    }

    fn digest(&self, view: &View) -> Result<JsonValue, CapError> {
        Ok(projection(view))
    }

    fn changes(
        &self,
        before: &JsonValue,
        after: &JsonValue,
        _reader: &str,
    ) -> Result<Vec<String>, CapError> {
        Ok(changes(before, after))
    }

    fn legend(&self, view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok(legend(view))
    }
}

fn check(
    record: &Record,
    keys: &BTreeSet<String>,
    created: bool,
    view: &View,
) -> Result<(), CapError> {
    match record.kind.as_str() {
        NODE => check_node(record, keys, created, view),
        EDGE if created || keys.iter().any(|k| EDGE_KEYS.contains(&k.as_str())) => {
            check_edge(record, view)
        }
        _ => Ok(()),
    }
}

// ── plans ────────────────────────────────────────────────────────────────

fn arguments(args: &JsonValue) -> Result<JsonMap<String, JsonValue>, CapError> {
    match args {
        JsonValue::Object(map) => Ok(map.clone()),
        JsonValue::Null => Ok(JsonMap::new()),
        _ => Err(err("invalid_args", "arguments must be an object")),
    }
}

fn allow_only(args: &JsonMap<String, JsonValue>, allowed: &[&str]) -> Result<(), CapError> {
    match args.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(err("invalid_args", format!("unknown argument {key:?}"))),
        None => Ok(()),
    }
}

fn optional_string(
    args: &JsonMap<String, JsonValue>,
    key: &str,
) -> Result<Option<String>, CapError> {
    match args.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(err("invalid_args", format!("{key} must be a string"))),
    }
}

fn optional_number(args: &JsonMap<String, JsonValue>, key: &str) -> Result<Option<f64>, CapError> {
    match args.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::Number(value)) => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| err("invalid_args", format!("{key} must be a finite number"))),
        Some(_) => Err(err("invalid_args", format!("{key} must be a number"))),
    }
}

fn required_number(args: &JsonMap<String, JsonValue>, key: &str) -> Result<f64, CapError> {
    optional_number(args, key)?.ok_or_else(|| err("invalid_args", format!("{key} is required")))
}

fn card_id(args: &JsonMap<String, JsonValue>, view: &View) -> Result<String, CapError> {
    let id = optional_string(args, "id")?.ok_or_else(|| err("invalid_args", "id is required"))?;
    match view.get(&id) {
        Some(record) if record.kind == NODE => Ok(id),
        Some(_) => Err(err("not_map_object", format!("{id} is not a card"))),
        None => Err(err("not_found", format!("no card {id:?}"))),
    }
}

fn plan(ops: Vec<Op>, readback: String) -> Plan {
    Plan { ops, readback }
}

fn set(id: &str, props: Vec<(&str, Value)>) -> Op {
    Op::Set {
        target: Target::Id(id.to_string()),
        props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn place(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    for foreign in ["path", "lines"] {
        if args.contains_key(foreign) {
            return Err(err(
                "needs_capability",
                format!("{foreign} belongs to the sources capability; placing a card with a source binding needs the composite adapter, which is not built yet"),
            ));
        }
    }
    allow_only(
        args,
        &[
            "id", "label", "note", "tone", "status", "x", "y", "w", "h_pref", "w_min", "h_min",
            "parent", "color", "emphasis", "size", "kind",
        ],
    )?;
    let existing = optional_string(args, "id")?;
    if let Some(id) = &existing {
        card_id(args, view)?;
        let _ = id;
    }
    let mut props: Vec<(&str, Value)> = Vec::new();
    if let Some(label) = optional_string(args, "label")? {
        let label = text(&label, MAX_LABEL, "label")?;
        if label.is_empty() {
            return Err(err("invalid", "label must not be empty"));
        }
        props.push((K_LABEL, label.into()));
    }
    if let Some(note) = optional_string(args, "note")? {
        props.push((K_NOTE, text(&note, MAX_NOTE, "note")?.into()));
    }
    for (arg, key, allowed) in [
        ("tone", K_TONE, TONES),
        ("status", K_STATUS, STATUSES),
        ("emphasis", K_EMPHASIS, EMPHASES),
        ("size", K_SIZE, SIZES),
        ("kind", K_SHAPE, SHAPES),
    ] {
        if let Some(value) = optional_string(args, arg)? {
            props.push((key, one_of(&value, allowed, arg)?.into()));
        }
    }
    if let Some(color) = optional_string(args, "color")? {
        let color = color.trim().to_string();
        props.push((
            K_COLOR,
            if color.is_empty() {
                color
            } else {
                one_of(&color, INKS, "color")?
            }
            .into(),
        ));
    }
    for (arg, key) in [("x", K_X), ("y", K_Y)] {
        if let Some(value) = optional_number(args, arg)? {
            props.push((key, coordinate(value, arg)?.into()));
        }
    }
    if let Some(w) = optional_number(args, "w")? {
        props.push((K_W, within(w, MIN_W, MAX_W, "w")?.into()));
    }
    if let Some(h_pref) = optional_number(args, "h_pref")? {
        props.push((K_H_PREF, preferred_height(h_pref)?.into()));
    }
    if let Some(w_min) = optional_number(args, "w_min")? {
        props.push((K_W_MIN, within(w_min, MIN_W, MAX_W, "w_min")?.into()));
    }
    if let Some(h_min) = optional_number(args, "h_min")? {
        props.push((K_H_MIN, within(h_min, MIN_H, MAX_H, "h_min")?.into()));
    }
    let parent = optional_string(args, "parent")?.map(|p| p.trim().to_string());
    if let Some(parent) = &parent {
        check_parent(existing.as_deref().unwrap_or(""), parent, view)?;
        props.push((K_PARENT, parent.clone().into()));
    }

    match existing {
        Some(id) => {
            if props.is_empty() {
                return Ok(plan(
                    Vec::new(),
                    format!("Nothing to change on {}.", label_of(view, &id)),
                ));
            }
            let readback = format!("Updated {}.", label_of(view, &id));
            Ok(plan(vec![set(&id, props)], readback))
        }
        None => {
            if !props.iter().any(|(k, _)| *k == K_LABEL) {
                return Err(err("invalid_args", "a new card needs a label"));
            }
            check_growth(map_count(view) + 1, 1)?;
            if !props.iter().any(|(k, _)| *k == K_X) || !props.iter().any(|(k, _)| *k == K_Y) {
                let (x, y) = free_slot(view);
                if !props.iter().any(|(k, _)| *k == K_X) {
                    props.push((K_X, x.into()));
                }
                if !props.iter().any(|(k, _)| *k == K_Y) {
                    props.push((K_Y, y.into()));
                }
            }
            for (key, default) in [(K_TONE, "concept"), (K_STATUS, "open")] {
                if !props.iter().any(|(k, _)| *k == key) {
                    props.push((key, default.into()));
                }
            }
            if !props.iter().any(|(k, _)| *k == K_W) {
                props.push((K_W, DEFAULT_WIDTH.into()));
            }
            let label = props
                .iter()
                .find(|(k, _)| *k == K_LABEL)
                .and_then(|(_, v)| match v {
                    Value::Scalar(Scalar::Str(s)) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Ok(plan(
                vec![Op::Create {
                    reference: "card".into(),
                    kind: NODE.into(),
                    props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
                }],
                format!("Placed {label:?}."),
            ))
        }
    }
}

// ── constraints ──────────────────────────────────────────────────────────

fn constrain(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    allow_only(args, &["op", "members", K_AXIS, K_TEXT, K_SALIENCE])?;
    let op = one_of(
        &optional_string(args, "op")?.unwrap_or_default(),
        CONSTRAINT_OPS,
        "constraint op",
    )?;
    let members: Vec<String> = match args.get("members") {
        Some(JsonValue::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| err("invalid_args", "each member must be a string"))
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(err("invalid_args", "members must be an array of ids")),
    };
    let (least, most) = arity(&op);
    if members.len() < least || most.is_some_and(|most| members.len() > most) {
        return Err(err(
            "invalid",
            format!(
                "constraint op {op:?} needs {}; got {}",
                arity_words(&op),
                members.len()
            ),
        ));
    }
    let unique: BTreeSet<&String> = members.iter().collect();
    if unique.len() != members.len() {
        return Err(err("invalid", "a constraint cannot hold a member twice"));
    }
    for member in &members {
        match view.get(member) {
            None => {
                return Err(err(
                    "not_found",
                    format!("constraint member {member:?} does not resolve to an object"),
                ))
            }
            Some(record) if record.kind == CONSTRAINT || record.kind == CONSTRAINT_MEMBER => {
                return Err(err(
                    "invalid",
                    format!("constraint member {member:?} names another constraint"),
                ))
            }
            Some(_) => {}
        }
    }
    let axis = match optional_string(args, K_AXIS)? {
        Some(axis) if op == "sequence" => one_of(&axis, CONSTRAINT_AXES, "constraint axis")?,
        Some(axis) if !axis.is_empty() => {
            return Err(err(
                "invalid",
                format!("only a sequence has an axis, not {op:?}"),
            ))
        }
        _ if op == "sequence" => "x".to_string(),
        _ => String::new(),
    };
    let body = text(
        &optional_string(args, K_TEXT)?.unwrap_or_default(),
        MAX_TEXT,
        "constraint text",
    )?;
    if op != "labels" && !body.is_empty() {
        return Err(err(
            "invalid",
            format!("only a labels constraint carries text, not {op:?}"),
        ));
    }
    if op == "labels" && body.is_empty() {
        return Err(err("invalid_args", "a labels constraint needs text"));
    }
    let salience = one_of(
        &optional_string(args, K_SALIENCE)?.unwrap_or_else(|| "back".to_string()),
        CONSTRAINT_SALIENCES,
        "constraint salience",
    )?;
    check_growth(map_count(view) + 1 + members.len(), 1)?;

    // The constraint and its memberships are one batch: created together,
    // marked converted from the start, and the frozen scalar is never written
    // on a constraint authored here.
    let mut ops = vec![Op::Create {
        reference: "constraint".into(),
        kind: CONSTRAINT.into(),
        props: [
            (K_OP.to_string(), Value::from(op.clone())),
            (K_AXIS.to_string(), Value::from(axis)),
            (K_TEXT.to_string(), Value::from(body)),
            (K_SALIENCE.to_string(), Value::from(salience)),
            (
                K_MEMBER_SCHEMA.to_string(),
                Value::from(SCHEMA_RECORDS.to_string()),
            ),
        ]
        .into(),
    }];
    for (index, member) in members.iter().enumerate() {
        ops.push(Op::Create {
            reference: format!("member-{index}"),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::reference("constraint")),
                (K_TARGET.to_string(), Value::from(member.clone())),
                (K_ORDER.to_string(), Value::from(index as f64)),
            ]
            .into(),
        });
    }
    let readback = format!(
        "Constrained {}: {op}.",
        members
            .iter()
            .map(|member| label_of(view, member))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(plan(ops, readback))
}

fn unconstrain(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    allow_only(args, &["id"])?;
    let id = optional_string(args, "id")?.ok_or_else(|| err("invalid_args", "id is required"))?;
    let record = view
        .get(&id)
        .filter(|record| record.kind == CONSTRAINT)
        .ok_or_else(|| err("not_found", format!("no constraint {id:?}")))?;
    let op = prop_str(record, K_OP).unwrap_or_default().to_string();
    let mut ops = vec![Op::Delete {
        target: Target::Id(id.clone()),
    }];
    // The memberships go with it; a membership without its constraint is
    // unreachable structure, not a diagnostic.
    ops.extend(
        member_records(view, &id)
            .into_iter()
            .map(|member| Op::Delete {
                target: Target::Id(member.id.clone()),
            }),
    );
    Ok(plan(ops, format!("Removed the {op} constraint.")))
}

fn retarget(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    allow_only(args, &["id", K_TARGET])?;
    let id = optional_string(args, "id")?.ok_or_else(|| err("invalid_args", "id is required"))?;
    let target = optional_string(args, K_TARGET)?
        .ok_or_else(|| err("invalid_args", "target is required"))?;
    let member = view
        .get(&id)
        .filter(|record| record.kind == CONSTRAINT_MEMBER)
        .ok_or_else(|| err("not_found", format!("no membership {id:?}")))?;
    match view.get(&target) {
        None => {
            return Err(err(
                "not_found",
                format!("no object {target:?} to point {id} at"),
            ))
        }
        Some(record) if record.kind == CONSTRAINT || record.kind == CONSTRAINT_MEMBER => {
            return Err(err(
                "invalid",
                format!("{target:?} is a constraint, which cannot be a member"),
            ))
        }
        Some(_) => {}
    }
    let constraint = prop_str(member, K_CONSTRAINT)
        .unwrap_or_default()
        .to_string();
    Ok(plan(
        vec![set(&id, vec![(K_TARGET, target.clone().into())])],
        format!(
            "Pointed a member of {constraint} at {}.",
            label_of(view, &target)
        ),
    ))
}

/// Authored size. Shared, attributed and persisted, so every peer draws the
/// card the same way. Measurement is the browser's own business and never
/// reaches the document.
fn resize(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    allow_only(args, &["id", K_W, K_H_PREF, K_W_MIN, K_H_MIN])?;
    let id = card_id(args, view)?;
    let record = view
        .get(&id)
        .ok_or_else(|| err("not_found", format!("no card {id:?}")))?;
    let current = |key: &str| prop_num(record, key);

    let w = match optional_number(args, K_W)? {
        Some(value) => Some(within(value, MIN_W, MAX_W, K_W)?),
        None => None,
    };
    let h_pref = match optional_number(args, K_H_PREF)? {
        Some(value) => Some(preferred_height(value)?),
        None => None,
    };
    let w_min = match optional_number(args, K_W_MIN)? {
        Some(value) => Some(within(value, MIN_W, MAX_W, K_W_MIN)?),
        None => None,
    };
    let h_min = match optional_number(args, K_H_MIN)? {
        Some(value) => Some(within(value, MIN_H, MAX_H, K_H_MIN)?),
        None => None,
    };
    if w.is_none() && h_pref.is_none() && w_min.is_none() && h_min.is_none() {
        return Err(err(
            "invalid_args",
            format!("card_resize needs at least one of {K_W}, {K_H_PREF}, {K_W_MIN}, {K_H_MIN}"),
        ));
    }
    // The pair rules hold against what the card will have, not only against
    // what this call carries, so one-sided resizes cannot slip past them.
    let after_w = w.or_else(|| current(K_W)).unwrap_or(DEFAULT_WIDTH);
    let after_w_min = w_min.or_else(|| current(K_W_MIN)).unwrap_or(0.0);
    if after_w_min > after_w {
        return Err(err("invalid", "w_min is above the preferred width w"));
    }
    let after_h_pref = h_pref.or_else(|| current(K_H_PREF)).unwrap_or(0.0);
    let after_h_min = h_min.or_else(|| current(K_H_MIN)).unwrap_or(0.0);
    if after_h_pref > 0.0 && after_h_min > after_h_pref {
        return Err(err("invalid", "h_min is above the preferred height h_pref"));
    }

    let mut props: Vec<(&str, Value)> = Vec::new();
    for (key, value) in [
        (K_W, w),
        (K_H_PREF, h_pref),
        (K_W_MIN, w_min),
        (K_H_MIN, h_min),
    ] {
        if let Some(value) = value {
            props.push((key, value.into()));
        }
    }
    let readback = format!(
        "Resized {} to {} by {}.",
        label_of(view, &id),
        after_w,
        if after_h_pref > 0.0 {
            format!("{after_h_pref}")
        } else {
            "fit".to_string()
        }
    );
    Ok(plan(vec![set(&id, props)], readback))
}

fn link(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    allow_only(args, &["from", "to", "label"])?;
    let from =
        optional_string(args, "from")?.ok_or_else(|| err("invalid_args", "from is required"))?;
    let to = optional_string(args, "to")?.ok_or_else(|| err("invalid_args", "to is required"))?;
    let label = text(
        &optional_string(args, "label")?.unwrap_or_default(),
        MAX_LABEL,
        "label",
    )?;
    for id in [&from, &to] {
        if !matches!(view.get(id), Some(record) if record.kind == NODE) {
            return Err(err("not_found", format!("no card {id:?}")));
        }
    }
    if from == to {
        return Err(err("invalid", "a link needs two different cards"));
    }
    let readback = format!(
        "Linked {} to {}.",
        label_of(view, &from),
        label_of(view, &to)
    );
    let existing = view.records().find(|record| {
        record.kind == EDGE
            && prop_str(record, K_FROM) == Some(from.as_str())
            && prop_str(record, K_TO) == Some(to.as_str())
            && !is_transition(record)
    });
    if existing.is_none() {
        check_growth(map_count(view) + 1, 1)?;
    }
    Ok(match existing {
        Some(edge) => plan(vec![set(&edge.id, vec![(K_LABEL, label.into())])], readback),
        None => plan(
            vec![Op::Create {
                reference: "link".into(),
                kind: EDGE.into(),
                props: [
                    (K_FROM.to_string(), Value::from(from)),
                    (K_TO.to_string(), Value::from(to)),
                    (K_LABEL.to_string(), Value::from(label)),
                ]
                .into(),
            }],
            readback,
        ),
    })
}

fn remove(args: &JsonMap<String, JsonValue>, view: &View) -> Result<Plan, CapError> {
    allow_only(args, &["id"])?;
    let id = optional_string(args, "id")?.ok_or_else(|| err("invalid_args", "id is required"))?;
    let record = view
        .get(&id)
        .ok_or_else(|| err("not_found", format!("no card or link {id:?}")))?;
    if record.kind != NODE && record.kind != EDGE {
        return Err(err(
            "not_map_object",
            format!(
                "{id} is a {:?}, not a card or link; its own capability removes it",
                record.kind
            ),
        ));
    }
    let readback = format!("Removed {}.", label_of(view, &id));
    let mut ops = vec![Op::Delete {
        target: Target::Id(id.clone()),
    }];
    let remaining: Vec<Record> = view.records().filter(|r| r.id != id).cloned().collect();
    let after =
        View::new(view.revision(), remaining).map_err(|e| err("internal", e.to_string()))?;
    ops.extend(cascade_ops(std::slice::from_ref(&id), &after));
    Ok(plan(ops, readback))
}

/// Links to a removed card go with it; its children move to the top level.
fn cascade_ops(deleted: &[String], view: &View) -> Vec<Op> {
    let gone = |value: Option<&str>| value.is_some_and(|v| deleted.iter().any(|d| d == v));
    let mut ops = Vec::new();
    for record in view.records() {
        match record.kind.as_str() {
            EDGE if gone(prop_str(record, K_FROM)) || gone(prop_str(record, K_TO)) => {
                ops.push(Op::Delete {
                    target: Target::Id(record.id.clone()),
                });
            }
            NODE if gone(prop_str(record, K_PARENT)) => {
                ops.push(set(&record.id, vec![(K_PARENT, "".into())]));
            }
            // A membership without its constraint is unreachable structure,
            // so it goes with it. A membership whose *target* was deleted is
            // not cascaded: the constraint keeps it and reports the broken
            // membership, which is the retained diagnostic behaviour.
            CONSTRAINT_MEMBER if gone(prop_str(record, K_CONSTRAINT)) => {
                ops.push(Op::Delete {
                    target: Target::Id(record.id.clone()),
                });
            }
            _ => {}
        }
    }
    ops
}

fn map_count(view: &View) -> usize {
    view.records()
        .filter(|r| r.kind == NODE || r.kind == EDGE)
        .count()
}

fn free_slot(view: &View) -> (f64, f64) {
    let cards: Vec<(f64, f64)> = view
        .records()
        .filter(|r| r.kind == NODE)
        .map(|r| {
            (
                prop_num(r, K_X).unwrap_or(0.0),
                prop_num(r, K_Y).unwrap_or(0.0),
            )
        })
        .collect();
    for index in 0..MAX_MAP_OBJECTS {
        let (x, y) = grid_slot(index);
        if !cards
            .iter()
            .any(|(cx, cy)| (cx - x).abs() < 40.0 && (cy - y).abs() < 40.0)
        {
            return (x, y);
        }
    }
    GRID_ORIGIN
}

// ── projection and read-back ─────────────────────────────────────────────

pub fn prop_str<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    record.props.get(key).and_then(|v| v.as_str())
}

pub fn prop_num(record: &Record, key: &str) -> Option<f64> {
    record.props.get(key).and_then(|v| v.as_f64())
}

fn label_of(view: &View, id: &str) -> String {
    view.get(id)
        .and_then(|r| prop_str(r, K_LABEL))
        .filter(|l| !l.is_empty())
        .map(|l| format!("{l:?}"))
        .unwrap_or_else(|| id.to_string())
}

/// A rough height for a card with no authored `h_pref`: title lines plus note
/// lines at the card's width. Always marked `estimated` in the projection; it
/// is arithmetic, not a measurement, and never claims to be one.
pub fn estimated_height(label: &str, note: &str, width: f64) -> f64 {
    let per_line = ((width - 32.0) / 7.5).max(8.0);
    let lines = |text: &str, chars: f64| -> f64 {
        text.split('\n')
            .map(|line| (line.chars().count() as f64 / chars).ceil().max(1.0))
            .sum::<f64>()
    };
    let title = lines(label, per_line * 0.85);
    let body = if note.is_empty() {
        0.0
    } else {
        lines(note, per_line)
    };
    (40.0 + 20.0 * title + 18.0 * body).min(MAX_H)
}

fn projection(view: &View) -> JsonValue {
    let mut problems = Vec::new();
    let nodes: BTreeMap<&str, &Record> = view
        .records()
        .filter(|r| r.kind == NODE)
        .map(|r| (r.id.as_str(), r))
        .collect();
    let cards: Vec<JsonValue> = nodes
        .values()
        .map(|record| {
            let label = prop_str(record, K_LABEL).unwrap_or_default();
            let note = prop_str(record, K_NOTE).unwrap_or_default();
            let w = prop_num(record, K_W)
                .filter(|w| *w > 0.0)
                .unwrap_or(DEFAULT_WIDTH);
            let w_min = prop_num(record, K_W_MIN).unwrap_or(0.0);
            let h = prop_num(record, K_H).unwrap_or(0.0);
            let h_pref = prop_num(record, K_H_PREF).unwrap_or(0.0);
            let h_min = prop_num(record, K_H_MIN).unwrap_or(0.0);
            let width = w.max(w_min);
            // Three separate things, never merged. A legacy `h` is history and
            // is not this render's height; nothing native was measured, so the
            // portable answer is `preferred` or `estimated` and says so. A
            // browser that measured the card locally overlays its own height
            // and sets `measured_known` there, in that browser only.
            // No clipping floor: the height is the greatest of the authored
            // preference, the authored minimum and what the content needs.
            // `height_source` says whose number governs. A browser puts its
            // own local measurement in place of the estimate, in the same
            // maximum, in that browser only.
            let estimate = estimated_height(label, note, width);
            let authored = h_pref.max(h_min);
            let height = authored.max(estimate);
            let source = if authored > 0.0 && authored >= estimate {
                "preferred"
            } else {
                "estimated"
            };
            let parent = prop_str(record, K_PARENT).unwrap_or_default();
            if !parent.is_empty() && !nodes.contains_key(parent) {
                problems.push(format!(
                    "card {} is inside {parent}, which is not a card",
                    record.id
                ));
            }
            for key in [K_X, K_Y] {
                if prop_num(record, key).is_some_and(|v| v.abs() > WORLD_LIMIT) {
                    problems.push(format!(
                        "card {} has {key} outside ±{WORLD_LIMIT}",
                        record.id
                    ));
                }
            }
            let other_keys: Vec<&str> = record
                .props
                .keys()
                .map(String::as_str)
                .filter(|k| {
                    !NODE_KEYS.contains(k) && !samepage_capability::AUTHORSHIP_KEYS.contains(k)
                })
                .collect();
            json!({
                "id": record.id,
                "label": label,
                "note": note,
                "tone": prop_str(record, K_TONE).unwrap_or("concept"),
                "status": prop_str(record, K_STATUS).unwrap_or("open"),
                "x": prop_num(record, K_X).unwrap_or(0.0),
                "y": prop_num(record, K_Y).unwrap_or(0.0),
                "w": w, "h_pref": h_pref, "w_min": w_min, "h_min": h_min,
                "width": width, "height": height, "height_source": source,
                "legacy_h": if record.props.contains_key(K_H) { json!(h) } else { JsonValue::Null },
                "measured_known": false,
                "parent": parent,
                "color": prop_str(record, K_COLOR).unwrap_or_default(),
                "emphasis": prop_str(record, K_EMPHASIS).unwrap_or("normal"),
                "size": prop_str(record, K_SIZE).unwrap_or("normal"),
                "shape": prop_str(record, K_SHAPE).unwrap_or("rect"),
                "created_by": prop_str(record, "created_by").unwrap_or_default(),
                "touched_by": prop_str(record, "touched_by").unwrap_or_default(),
                "other_keys": other_keys,
            })
        })
        .collect();
    let links: Vec<JsonValue> = view
        .records()
        .filter(|r| r.kind == EDGE)
        .map(|record| {
            let from = prop_str(record, K_FROM).unwrap_or_default();
            let to = prop_str(record, K_TO).unwrap_or_default();
            for (end, id) in [("from", from), ("to", to)] {
                if !nodes.contains_key(id) {
                    problems.push(format!("link {} {end} {id:?} is not a card", record.id));
                }
            }
            json!({
                "id": record.id,
                "from": from,
                "to": to,
                "label": prop_str(record, K_LABEL).unwrap_or_default(),
                "transition": is_transition(record),
            })
        })
        .collect();
    let constraints: Vec<JsonValue> = view
        .records()
        .filter(|record| record.kind == CONSTRAINT)
        .map(|record| {
            let op = prop_str(record, K_OP).unwrap_or_default();
            let members: Vec<JsonValue> = if converted(record) {
                member_records(view, &record.id)
                    .into_iter()
                    .map(|member| {
                        let target = prop_str(member, K_TARGET).unwrap_or_default();
                        json!({
                            "id": member.id,
                            "target": target,
                            "order": prop_num(member, K_ORDER).unwrap_or(0.0),
                            // `resolved` or `unresolved`, never "missing": an
                            // id absent from this view does not reveal its
                            // kind, so the map cannot tell a deleted card from
                            // an object owned by a capability it does not read.
                            "state": if resolves(view, target) { "resolved" } else { "unresolved" },
                        })
                    })
                    .collect()
            } else {
                membership(view, &record.id)
                    .into_iter()
                    .enumerate()
                    .map(|(index, target)| {
                        json!({
                            "id": JsonValue::Null,
                            "target": target,
                            "order": index as f64,
                            "state": if resolves(view, &target) { "resolved" } else { "unresolved" },
                        })
                    })
                    .collect()
            };
            let (least, most) = arity(op);
            let count = members.len();
            let unresolved = members.iter().any(|member| member["state"] == "unresolved");
            let arity_holds = count >= least && !most.is_some_and(|most| count > most);
            let status = if !arity_holds {
                "arity"
            } else if unresolved {
                "unresolved"
            } else {
                "sat"
            };
            if !arity_holds {
                problems.push(format!(
                    "constraint {} is a {op} with {count} member(s); it needs {}",
                    record.id,
                    arity_words(op)
                ));
            }
            json!({
                "id": record.id,
                "op": op,
                "members": members,
                "membership": if converted(record) { SCHEMA_RECORDS } else { "legacy" },
                "legacy_members": prop_str(record, K_MEMBERS).unwrap_or_default(),
                "axis": prop_str(record, K_AXIS).unwrap_or_default(),
                "text": prop_str(record, K_TEXT).unwrap_or_default(),
                "salience": prop_str(record, K_SALIENCE).unwrap_or("back"),
                "status": status,
            })
        })
        .collect();
    let constraint_state = validation::constraint_state(&constraints);
    if constraint_state["status"] == "unsat" {
        problems.push(format!(
            "UNSAT sequence cycle: {}",
            constraint_state["core"]
        ));
    }
    json!({
        "cards": cards,
        "links": links,
        "constraint_state": constraint_state,
        "constraints": constraints,
        "problems": problems,
    })
}

fn describe(view: &View) -> String {
    let projected = projection(view);
    let cards = projected["cards"].as_array().cloned().unwrap_or_default();
    let links = projected["links"].as_array().cloned().unwrap_or_default();
    let mut out = format!("MAP: {} cards, {} links.", cards.len(), links.len());
    for card in &cards {
        out.push_str(&format!(
            "\n- [{}] {:?} at ({}, {}) {}x{} ({}), {} / {}{}{}",
            card["id"].as_str().unwrap_or_default(),
            card["label"].as_str().unwrap_or_default(),
            card["x"],
            card["y"],
            card["width"],
            card["height"],
            card["height_source"].as_str().unwrap_or_default(),
            card["tone"].as_str().unwrap_or_default(),
            card["status"].as_str().unwrap_or_default(),
            match card["parent"].as_str() {
                Some(p) if !p.is_empty() => format!(", inside {p}"),
                _ => String::new(),
            },
            match card["touched_by"].as_str() {
                Some(t) if !t.is_empty() => format!(", last by {t}"),
                _ => String::new(),
            },
        ));
    }
    for link in &links {
        out.push_str(&format!(
            "\n- link [{}] {} -> {}{}",
            link["id"].as_str().unwrap_or_default(),
            link["from"].as_str().unwrap_or_default(),
            link["to"].as_str().unwrap_or_default(),
            match link["label"].as_str() {
                Some(l) if !l.is_empty() => format!(" {l:?}"),
                _ => String::new(),
            },
        ));
    }
    for problem in projected["problems"].as_array().into_iter().flatten() {
        out.push_str(&format!("\n! {}", problem.as_str().unwrap_or_default()));
    }
    out
}

fn by_id(value: &JsonValue, field: &str) -> BTreeMap<String, JsonValue> {
    value[field]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| Some((item["id"].as_str()?.to_string(), item.clone())))
        .collect()
}

fn changes(before: &JsonValue, after: &JsonValue) -> Vec<String> {
    let mut out = Vec::new();
    let (old, new) = (by_id(before, "cards"), by_id(after, "cards"));
    for (id, card) in &new {
        let label = card["label"].as_str().unwrap_or_default();
        match old.get(id) {
            None => out.push(format!("card {label:?} [{id}] added")),
            Some(prior) => {
                if prior["x"] != card["x"] || prior["y"] != card["y"] {
                    out.push(format!(
                        "card {label:?} [{id}] moved to ({}, {})",
                        card["x"], card["y"]
                    ));
                }
                if prior["width"] != card["width"] || prior["height"] != card["height"] {
                    out.push(format!(
                        "card {label:?} [{id}] resized to {}x{}",
                        card["width"], card["height"]
                    ));
                }
                if prior["label"] != card["label"] {
                    out.push(format!(
                        "card [{id}] renamed from {} to {label:?}",
                        prior["label"]
                    ));
                }
                for field in [
                    "note", "tone", "status", "parent", "color", "emphasis", "size", "shape",
                ] {
                    if prior[field] != card[field] {
                        out.push(format!(
                            "card {label:?} [{id}] {field} changed to {}",
                            card[field]
                        ));
                    }
                }
            }
        }
    }
    for (id, card) in &old {
        if !new.contains_key(id) {
            out.push(format!(
                "card {:?} [{id}] removed",
                card["label"].as_str().unwrap_or_default()
            ));
        }
    }
    let (old, new) = (by_id(before, "links"), by_id(after, "links"));
    for (id, link) in &new {
        match old.get(id) {
            None => out.push(format!(
                "link [{id}] {} -> {} added",
                link["from"], link["to"]
            )),
            Some(prior) if prior["label"] != link["label"] => {
                out.push(format!("link [{id}] relabelled {}", link["label"]))
            }
            _ => {}
        }
    }
    for id in old.keys() {
        if !new.contains_key(id) {
            out.push(format!("link [{id}] removed"));
        }
    }
    out
}

/// One key per value present in the view, for each visual attribute a card
/// carries. The renderer's classes are not defined yet (C4b), so `class` is
/// left empty rather than guessed.
fn legend(view: &View) -> Vec<LegendEntry> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for record in view.records().filter(|r| r.kind == NODE) {
        for (attribute, key, default) in [
            ("tone", K_TONE, "concept"),
            ("status", K_STATUS, "open"),
            ("color", K_COLOR, ""),
            ("emphasis", K_EMPHASIS, "normal"),
            ("size", K_SIZE, "normal"),
            ("shape", K_SHAPE, "rect"),
        ] {
            let value = prop_str(record, key).unwrap_or(default).to_string();
            if value.is_empty() || !seen.insert((attribute, value.clone())) {
                continue;
            }
            entries.push(LegendEntry {
                capability: ID.to_string(),
                attribute: attribute.to_string(),
                label: format!("{attribute}: {value}"),
                value,
                class: None,
                symbol: None,
            });
        }
    }
    entries
}

#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Map, Map);

/// Retarget and deduplicate memberships during an atomic drawing-to-card lift.
/// Legacy comma lists are converted rather than encoding a temporary id inside
/// a scalar. The frozen legacy value remains history.
pub fn retarget_members(view: &View, from: &[String], to: Target) -> Result<Plan, CapError> {
    let value = match to {
        Target::Id(id) => Value::from(id),
        Target::Ref(reference) => Value::reference(&reference),
    };
    let mut ops = Vec::new();
    for constraint in view.records().filter(|r| r.kind == CONSTRAINT) {
        let members = membership(view, &constraint.id);
        if !members.iter().any(|m| from.contains(m)) {
            continue;
        }
        let mut seen = BTreeSet::new();
        if converted(constraint) {
            for member in member_records(view, &constraint.id) {
                let target = prop_str(member, K_TARGET).unwrap_or_default();
                let changed = from.iter().any(|id| id == target);
                let key = if changed { "\0lifted-card" } else { target };
                if !seen.insert(key.to_string()) {
                    ops.push(Op::Delete {
                        target: Target::Id(member.id.clone()),
                    });
                } else if changed {
                    ops.push(set(&member.id, vec![(K_TARGET, value.clone())]));
                }
            }
        } else {
            ops.push(set(
                &constraint.id,
                vec![(K_MEMBER_SCHEMA, SCHEMA_RECORDS.into())],
            ));
            for (index, target) in members.iter().enumerate() {
                let changed = from.contains(target);
                let key = if changed { "\0lifted-card" } else { target };
                if !seen.insert(key.to_string()) {
                    continue;
                }
                ops.push(Op::Create {
                    reference: format!("lift-member-{}-{index}", constraint.id),
                    kind: CONSTRAINT_MEMBER.into(),
                    props: [
                        (K_CONSTRAINT.into(), constraint.id.clone().into()),
                        (
                            K_TARGET.into(),
                            if changed {
                                value.clone()
                            } else {
                                target.clone().into()
                            },
                        ),
                        (K_ORDER.into(), (index as f64).into()),
                    ]
                    .into(),
                });
            }
        }
    }
    Ok(Plan {
        ops,
        readback: String::new(),
    })
}
