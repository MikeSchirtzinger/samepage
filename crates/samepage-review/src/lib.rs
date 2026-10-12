//! Portable review rules. Plans have no side effects; the native authority
//! hook protects the same rules on actions, batches and replica frames.
pub mod context;
pub mod report;

use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CallerKind, CapError, Capability, Change, Delta,
    Descriptor, LegendEntry, Op, Origin, Plan, Record, Scalar, Target, Trigger, Value, View,
};
use samepage_map::{prop_str, rules::text};
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};

pub const ID: &str = "review";
pub const VERSION: &str = "0.1.0";
pub const MARK_KEYS: &[&str] = &["target", "glyph", "text", "answer", "context_ids"];
pub const CLAIM_KEYS: &[&str] = &[
    "about",
    "text",
    "basis",
    "path",
    "lines",
    "revision",
    "verdict",
    "withdrawn",
];
const CONTENT_KEYS: &[&str] = &["about", "text", "basis", "path", "lines", "revision"];
const MAX_TEXT: usize = 1200;
const MAX_CLAIMS_PER_NODE: usize = 24;

pub(crate) fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("invalid", message)
}
pub fn string<'a>(record: &'a Record, key: &str) -> &'a str {
    prop_str(record, key).unwrap_or_default()
}
pub fn flag(record: &Record, key: &str) -> bool {
    matches!(record.props.get(key), Some(Scalar::Bool(true)))
}
pub(crate) fn create(reference: &str, kind: &str, props: BTreeMap<String, Value>) -> Op {
    Op::Create {
        reference: reference.into(),
        kind: kind.into(),
        props,
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
fn record<'a>(view: &'a View, id: &str, kind: &str) -> Result<&'a Record, CapError> {
    view.get(id)
        .filter(|r| r.kind == kind)
        .ok_or_else(|| invalid(format!("no {kind} {id:?} on this page")))
}
fn arg<'a>(args: &'a JsonValue, key: &str) -> Result<&'a str, CapError> {
    match args.get(key) {
        None => Ok(""),
        Some(JsonValue::String(s)) => Ok(s),
        _ => Err(invalid(format!("{key} must be text"))),
    }
}
fn action(
    name: &str,
    alias: Option<&str>,
    audience: Audience,
    effect: Effect,
    required: &[&str],
    properties: JsonValue,
    description: &str,
) -> ActionDecl {
    let mut a = ActionDecl::new(name, audience, effect, description);
    if let Some(alias) = alias {
        a = a.alias(alias);
    }
    a.input_schema = json!({"type":"object", "required":required, "properties":properties, "additionalProperties":false});
    a
}
use samepage_capability::Effect;
pub fn descriptor() -> Descriptor {
    let mut descriptor = Descriptor::new(ID, VERSION, Artifact::Wasm)
        .depends_on("map", samepage_map::VERSION, "Review targets Map records")
        .owns("mark", MARK_KEYS, "Human questions and objections, with answers in place")
        .owns("claim", CLAIM_KEYS, "Agent claims and human verdicts")
        .owns(context::KIND, &["definition"], "Append-only conversation context history")
        .reads("node").reads("edge").reads("constraint")
        .action(action("mark_add", Some("atlas_point"), Audience::Human, Effect::Mutate, &["target","glyph"], json!({"target":{"type":"string"},"glyph":{"enum":["?","!","*"]},"text":{"type":"string"}}), "Place a human question, objection or emphasis on a card or claim."))
        .action(action("mark_answer", Some("atlas_answer"), Audience::Both, Effect::Mutate, &["id","answer"], json!({"id":{"type":"string"},"answer":{"type":"string"}}), "Answer a person's question mark in place: the question and its author stay, and your answer is printed beside it. Mark ids are in review_read and page_read under MARKS. To answer with a claim a person can accept, use claim_add as well."))
        .action(action("claim_add", Some("atlas_claim"), Audience::Agent, Effect::Mutate, &["text","basis"], json!({"id":{"type":"string"},"about":{"type":"string"},"text":{"type":"string"},"basis":{"enum":["verified","inferred","assumed","unknown"]},"path":{"type":"string"},"lines":{"type":"string"},"revision":{"type":"string"}}), "State one thing you understand about a card (about), with how you know it (basis), so a person can accept or reject it in place. verified needs path and lines that the host checks, optionally pinned to a full commit in revision; inferred may cite a path; assumed and unknown carry no source. Pass id to restate a claim; restating an accepted claim reopens it. You never set a verdict."))
        .action(action("claim_withdraw", Some("atlas_withdraw_claim"), Audience::Agent, Effect::Mutate, &["id"], json!({"id":{"type":"string"}}), "Withdraw a claim while retaining its history."))
        .action(action("claim_verdict", None, Audience::Human, Effect::Mutate, &["id","verdict"], json!({"id":{"type":"string"},"verdict":{"enum":["open","accepted","rejected"]}}), "Give the human verdict on an agent claim."))
        .action(action("review_read", Some("atlas_review"), Audience::Both, Effect::Query, &[], json!({}), "Read every card's claims and their standing, the people's marks with any answers, the shared question, and what changed since your last read."))
        .action(action("context_set", Some("atlas_context_set"), Audience::Both, Effect::Mutate, &["question","altitude"], json!({"question":{"type":"string"},"altitude":{"enum":["purpose","system","component","implementation"]},"subjects":{"type":"array","items":{"type":"string"}},"assumptions":{"type":"string"},"replaces":{"type":"array","items":{"type":"string"}}}), "Set the shared question this page is answering, its altitude (purpose, system, component or implementation), the subject card ids and assumptions. Pass every current context id (the bracketed id under CONVERSATION CONTEXT in review_read) as replaces. This records your proposed scope, never a person's agreement."))
        .trigger(Trigger::kind("mark")).trigger(Trigger::kind("claim")).trigger(Trigger::kind(context::KIND));
    for action in &descriptor.actions {
        descriptor.triggers.push(Trigger::tool(&action.name));
        for alias in &action.aliases {
            descriptor.triggers.push(Trigger::tool(alias));
        }
    }
    descriptor
}

pub struct Review;
impl Capability for Review {
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
        let declaration = descriptor()
            .actions
            .into_iter()
            .find(|a| a.name == call.action)
            .ok_or_else(|| CapError::new("unknown_action", &call.action))?;
        let args = call
            .args
            .as_object()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        for key in args.keys() {
            if declaration.input_schema["properties"].get(key).is_none() {
                return Err(invalid(format!("unknown argument {key}")));
            }
        }
        if let Some(required) = declaration.input_schema["required"].as_array() {
            for key in required.iter().filter_map(JsonValue::as_str) {
                if !args.contains_key(key) {
                    return Err(invalid(format!("missing argument {key}")));
                }
            }
        }
        let args = &call.args;
        let mut ops = Vec::new();
        let readback = match call.action.as_str() {
            "mark_add" => {
                let target = arg(args, "target")?;
                if view.get(target).is_none_or(|r| {
                    !matches!(r.kind.as_str(), "node" | "constraint" | "claim" | "shape")
                }) {
                    return Err(invalid(
                        "mark target is not a card, constraint, shape or claim on this page",
                    ));
                }
                let glyph = arg(args, "glyph")?;
                choice(glyph, &["?", "!", "*"], "glyph")?;
                let body = text(arg(args, "text")?, MAX_TEXT, "mark text")?;
                ops.push(create(
                    "mark",
                    "mark",
                    [
                        ("target".into(), target.into()),
                        ("glyph".into(), glyph.into()),
                        ("text".into(), body.into()),
                        ("answer".into(), "".into()),
                        (
                            "context_ids".into(),
                            context::project(view)?.heads.join(",").into(),
                        ),
                    ]
                    .into(),
                ));
                "Human mark placed.".into()
            }
            "mark_answer" => {
                let id = arg(args, "id")?;
                record(view, id, "mark")?;
                let answer = text(arg(args, "answer")?, MAX_TEXT, "answer")?;
                ops.push(set(id, [("answer".into(), answer.into())].into()));
                "Answer recorded beside the original mark.".into()
            }
            "claim_add" => {
                let id = arg(args, "id")?;
                let old = if id.is_empty() {
                    None
                } else {
                    Some(record(view, id, "claim")?)
                };
                if old.is_some_and(|r| flag(r, "withdrawn")) {
                    return Err(invalid("claim was withdrawn; state a new one instead"));
                }
                let about = if arg(args, "about")?.is_empty() {
                    old.map(|r| string(r, "about")).unwrap_or_default()
                } else {
                    arg(args, "about")?
                };
                record(view, about, "node")?;
                if old.is_some_and(|r| string(r, "about") != about) {
                    return Err(invalid("a claim cannot change its subject"));
                }
                if old.is_none()
                    && claims_of(view, about)
                        .iter()
                        .filter(|r| !flag(r, "withdrawn"))
                        .count()
                        >= MAX_CLAIMS_PER_NODE
                {
                    return Err(invalid(
                        "a card already carries 24 live claims; split it first",
                    ));
                }
                let body = text(arg(args, "text")?, MAX_TEXT, "claim text")?;
                if body.is_empty() {
                    return Err(invalid("a claim needs text"));
                }
                let basis = arg(args, "basis")?;
                let path = arg(args, "path")?;
                let lines = arg(args, "lines")?;
                let revision = arg(args, "revision")?;
                basis_and_source(basis, path, lines, revision)?;
                let mut props: BTreeMap<String, Value> = [
                    ("about".into(), about.into()),
                    ("text".into(), body.into()),
                    ("basis".into(), basis.into()),
                    ("path".into(), path.into()),
                    ("lines".into(), lines.into()),
                    ("revision".into(), revision.into()),
                ]
                .into();
                if let Some(old) = old {
                    if string(old, "verdict") == "accepted"
                        && props.iter().any(
                            |(k, v)| !matches!(v, Value::Scalar(s) if old.props.get(k) == Some(s)),
                        )
                    {
                        props.insert("verdict".into(), "open".into());
                    }
                    // An identical restatement is a no-op, not a false revision.
                    props.retain(
                        |k, v| !matches!(v, Value::Scalar(s) if old.props.get(k) == Some(s)),
                    );
                    if !props.is_empty() {
                        ops.push(set(id, props));
                    }
                } else {
                    props.insert("verdict".into(), "open".into());
                    props.insert("withdrawn".into(), false.into());
                    ops.push(create("claim", "claim", props));
                }
                "Agent claim recorded for human review.".into()
            }
            "claim_withdraw" => {
                let id = arg(args, "id")?;
                let old = record(view, id, "claim")?;
                if !flag(old, "withdrawn") {
                    ops.push(set(id, [("withdrawn".into(), true.into())].into()));
                }
                "Claim withdrawn; history retained.".into()
            }
            "claim_verdict" => {
                let id = arg(args, "id")?;
                let old = record(view, id, "claim")?;
                let verdict = arg(args, "verdict")?;
                choice(verdict, &["open", "accepted", "rejected"], "verdict")?;
                if flag(old, "withdrawn") {
                    return Err(invalid("claim was withdrawn; nothing to accept or reject"));
                }
                if verdict == "accepted" && string(old, "basis") == "unknown" {
                    return Err(invalid("unknown basis cannot be accepted"));
                }
                if string(old, "verdict") != verdict {
                    ops.push(set(id, [("verdict".into(), verdict.into())].into()));
                }
                format!("Human verdict: {verdict}.")
            }
            "context_set" => {
                return context::plan(
                    &serde_json::from_value(args.clone()).map_err(|e| invalid(e.to_string()))?,
                    view,
                )
            }
            "review_read" => describe(view)?,
            _ => return Err(CapError::new("unknown_action", &call.action)),
        };
        Ok(Plan { ops, readback })
    }
    fn validate(&self, _delta: &Delta, view: &View) -> Result<(), CapError> {
        let context = context::project(view)?;
        for r in view.records() {
            match r.kind.as_str() {
                "claim" => {
                    for key in CLAIM_KEYS.iter().filter(|k| **k != "withdrawn") {
                        if r.props.get(*key).is_none_or(|v| v.as_str().is_none()) {
                            return Err(invalid(format!("claim {key} must be text")));
                        }
                    }
                    record(view, string(r, "about"), "node")?;
                    let body = text(string(r, "text"), MAX_TEXT, "claim text")?;
                    if body.is_empty() || body != string(r, "text") {
                        return Err(invalid("claim needs nonempty trimmed text"));
                    }
                    basis_and_source(
                        string(r, "basis"),
                        string(r, "path"),
                        string(r, "lines"),
                        string(r, "revision"),
                    )?;
                    choice(
                        string(r, "verdict"),
                        &["open", "accepted", "rejected"],
                        "verdict",
                    )?;
                    if !matches!(r.props.get("withdrawn"), Some(Scalar::Bool(_))) {
                        return Err(invalid("withdrawn must be boolean"));
                    }
                    if string(r, "verdict") == "accepted" && string(r, "basis") == "unknown" {
                        return Err(invalid("unknown basis cannot be accepted"));
                    }
                    if claims_of(view, string(r, "about"))
                        .iter()
                        .filter(|c| !flag(c, "withdrawn"))
                        .count()
                        > MAX_CLAIMS_PER_NODE
                    {
                        return Err(invalid("more than 24 live claims on one card"));
                    }
                }
                "mark" => {
                    for key in MARK_KEYS {
                        if r.props.get(*key).is_none_or(|v| v.as_str().is_none()) {
                            return Err(invalid(format!("mark {key} must be text")));
                        }
                    }
                    if view.get(string(r, "target")).is_none_or(|t| {
                        !matches!(t.kind.as_str(), "node" | "constraint" | "claim" | "shape")
                    }) {
                        return Err(invalid("mark target does not exist"));
                    }
                    choice(string(r, "glyph"), &["?", "!", "*"], "glyph")?;
                    for key in ["text", "answer"] {
                        let value = r
                            .props
                            .get(key)
                            .and_then(Scalar::as_str)
                            .ok_or_else(|| invalid(format!("{key} must be text")))?;
                        text(value, MAX_TEXT, key)?;
                    }
                    for id in string(r, "context_ids")
                        .split(',')
                        .filter(|s| !s.is_empty())
                    {
                        if !context.entries.iter().any(|e| e.id == id) {
                            return Err(invalid("mark refers to unknown context"));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        authority(change)?;
        self.validate(change.delta, change.after)
    }
    fn cascade(&self, deleted: &[String], view: &View) -> Result<Plan, CapError> {
        let mut doomed: BTreeSet<String> = deleted.iter().cloned().collect();
        for r in view.records().filter(|r| r.kind == "claim") {
            if doomed.contains(string(r, "about")) {
                doomed.insert(r.id.clone());
            }
        }
        for r in view.records().filter(|r| r.kind == "mark") {
            if doomed.contains(string(r, "target")) {
                doomed.insert(r.id.clone());
            }
        }
        Ok(Plan {
            ops: view
                .records()
                .filter(|r| matches!(r.kind.as_str(), "claim" | "mark") && doomed.contains(&r.id))
                .map(|r| delete(&r.id))
                .collect(),
            readback: String::new(),
        })
    }
    fn changes(
        &self,
        before: &JsonValue,
        after: &JsonValue,
        reader: &str,
    ) -> Result<Vec<String>, CapError> {
        Ok(changes(before, after, reader))
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            (
                "element",
                "mark",
                "Human question, objection or emphasis",
                "review-mark-glyphs",
                "? ! *",
            ),
            (
                "state",
                "SETTLED",
                "All live claims accepted",
                "review-badge",
                "SETTLED",
            ),
            (
                "state",
                "UNDER CHALLENGE",
                "Open or rejected claim",
                "review-badge",
                "UNDER CHALLENGE",
            ),
            (
                "state",
                "NO CLAIMS",
                "No live claims on this card",
                "review-badge",
                "NO CLAIMS",
            ),
            ("status", "agreed", "Agreed card", "map-card", "✓ AGREED"),
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
}

fn choice(value: &str, choices: &[&str], name: &str) -> Result<(), CapError> {
    if choices.contains(&value) {
        Ok(())
    } else {
        Err(invalid(format!("invalid {name}: {value:?}")))
    }
}
pub fn basis_and_source(
    basis: &str,
    path: &str,
    lines: &str,
    revision: &str,
) -> Result<(), CapError> {
    choice(
        basis,
        &["verified", "inferred", "assumed", "unknown"],
        "basis",
    )?;
    if !path.is_empty()
        && (path.starts_with('/')
            || path.starts_with('~')
            || path.contains('\\')
            || path
                .split('/')
                .any(|p| p == ".." || p == "." || p.is_empty())
            || path.chars().any(char::is_control))
    {
        return Err(invalid("source path must be project-relative"));
    }
    if !lines.is_empty() {
        let (a, b) = lines.split_once('-').unwrap_or((lines, lines));
        let a = a
            .parse::<usize>()
            .map_err(|_| invalid("lines must be N or N-M"))?;
        let b = b
            .parse::<usize>()
            .map_err(|_| invalid("lines must be N or N-M"))?;
        if a == 0 || b < a || b - a >= 400 {
            return Err(invalid("invalid line range"));
        }
    }
    if !revision.is_empty()
        && (revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(invalid("revision must be forty hex characters"));
    }
    match basis {
        "verified" if path.is_empty() || lines.is_empty() => {
            Err(invalid("a verified claim needs both path and lines"))
        }
        "assumed" | "unknown" if !path.is_empty() || !lines.is_empty() => {
            Err(invalid("assumed or unknown claims cannot carry source"))
        }
        other if !revision.is_empty() && other != "verified" => {
            Err(invalid("only a verified claim can pin a revision"))
        }
        _ => Ok(()),
    }
}

fn authority(c: &Change<'_>) -> Result<(), CapError> {
    let human = c.principal.caller == CallerKind::Human;
    for id in &c.delta.deleted {
        let Some(old) = c.before.get(id) else {
            continue;
        };
        match old.kind.as_str() {
            "claim" if c.after.contains(string(old, "about")) => {
                return Err(invalid("withdraw claims to preserve their history"))
            }
            "understanding-context" => return Err(invalid("context history cannot be deleted")),
            "mark" if !human && c.after.contains(string(old, "target")) => {
                return Err(invalid("only the human can remove a mark"))
            }
            _ => {}
        }
    }
    for (id, keys) in c.attempted {
        let Some(r) = c.after.get(id) else { continue };
        let old = c.before.get(id);
        match r.kind.as_str() {
            "node" if keys.contains("status") && string(r, "status") == "agreed" => {
                let unsettled: Vec<_> = claims_of(c.after, id)
                    .into_iter()
                    .filter(|r| !flag(r, "withdrawn") && string(r, "verdict") != "accepted")
                    .map(|r| r.id.clone())
                    .collect();
                if !unsettled.is_empty() {
                    return Err(invalid(format!(
                        "card {id} is under challenge: {}",
                        unsettled.join(", ")
                    )));
                }
            }
            "claim" => {
                if let Some(old) = old {
                    if string(old, "about") != string(r, "about") {
                        return Err(invalid("a claim cannot change its subject"));
                    }
                    if flag(old, "withdrawn")
                        && (keys
                            .iter()
                            .any(|k| CONTENT_KEYS.contains(&k.as_str()) || k == "verdict")
                            || !flag(r, "withdrawn"))
                    {
                        return Err(invalid("withdrawn claims are immutable history"));
                    }
                    if human {
                        if keys.iter().any(|k| k != "verdict") {
                            return Err(invalid("only the agent can revise or withdraw a claim"));
                        }
                    } else {
                        let changed = CONTENT_KEYS
                            .iter()
                            .any(|k| r.props.get(*k) != old.props.get(*k));
                        if keys.contains("verdict")
                            && !(changed
                                && string(old, "verdict") == "accepted"
                                && string(r, "verdict") == "open")
                        {
                            return Err(invalid("only the human can give a verdict on a claim"));
                        }
                        if changed
                            && string(old, "verdict") == "accepted"
                            && string(r, "verdict") != "open"
                        {
                            return Err(invalid("revising an accepted claim must reopen it"));
                        }
                    }
                } else if human || string(r, "verdict") != "open" || flag(r, "withdrawn") {
                    return Err(invalid(
                        "only the agent creates a claim, with an open verdict",
                    ));
                }
                if string(r, "basis") == "verified"
                    && keys.iter().any(|k| CONTENT_KEYS.contains(&k.as_str()))
                {
                    let expected = json!({"path":string(r,"path"),"lines":string(r,"lines"),"revision":string(r,"revision")});
                    if c.facts.and_then(|f| f.get(id)) != Some(&expected) {
                        return Err(CapError::new(
                            "source_unverified",
                            "verified claim needs native source facts",
                        ));
                    }
                }
            }
            "mark" => {
                if let Some(old) = old {
                    if keys.contains("context_ids") {
                        return Err(invalid("a mark retains its original conversation context"));
                    }
                    let lift = c
                        .before
                        .get(string(old, "target"))
                        .is_some_and(|r| r.kind == "shape")
                        && keys.len() == 1
                        && keys.contains("target")
                        && !c.after.contains(string(old, "target"))
                        && c.delta.created.contains(string(r, "target"))
                        && c.after
                            .get(string(r, "target"))
                            .is_some_and(|r| r.kind == "node");
                    if !human && !lift && keys.iter().any(|k| k != "answer") {
                        return Err(invalid(
                            "only the human authors a mark; the agent may answer it",
                        ));
                    }
                } else {
                    if !human {
                        return Err(invalid("only the human can create marks"));
                    }
                    let heads = context::project(c.before)?.heads.join(",");
                    if !matches!(c.origin, Origin::ReplicaFrame)
                        && string(r, "context_ids") != heads
                    {
                        return Err(invalid(
                            "mark must retain its observed conversation context",
                        ));
                    }
                }
            }
            "understanding-context" => {
                if old.is_some() {
                    return Err(invalid("context history is append-only"));
                }
                let input: context::ContextInput = serde_json::from_str(string(r, "definition"))
                    .map_err(|e| invalid(e.to_string()))?;
                context::validate_input(&input, c.after)?;
                if matches!(c.origin, Origin::HostAction { .. } | Origin::UntrustedBatch) {
                    let mut replaces = input.replaces.clone();
                    replaces.sort();
                    if replaces != context::project(c.before)?.heads {
                        return Err(invalid(
                            "conversation scope changed; read before replacing it",
                        ));
                    }
                }
                // Replica frames may merge independently authored heads. Structural
                // validation still checks history, predecessors and cycles.
            }
            _ => {}
        }
    }
    Ok(())
}

/// D2 lift composes these edits in the same batch as Map's fresh card and
/// Sketch's deletion. It does not recreate human marks or change their authors.
pub fn retarget_marks(view: &View, from: &str, to: Target) -> Plan {
    let value = match to {
        Target::Id(id) => Value::from(id),
        Target::Ref(reference) => Value::reference(&reference),
    };
    Plan {
        ops: view
            .records()
            .filter(|r| r.kind == "mark" && string(r, "target") == from)
            .map(|r| set(&r.id, [("target".into(), value.clone())].into()))
            .collect(),
        readback: String::new(),
    }
}
pub fn claims_of<'a>(view: &'a View, node: &str) -> Vec<&'a Record> {
    view.records()
        .filter(|r| r.kind == "claim" && string(r, "about") == node)
        .collect()
}
pub fn standing(view: &View, node: &str) -> &'static str {
    let claims: Vec<_> = claims_of(view, node)
        .into_iter()
        .filter(|r| !flag(r, "withdrawn"))
        .collect();
    if claims.is_empty() {
        "NO CLAIMS"
    } else if claims.iter().any(|r| string(r, "verdict") != "accepted") {
        "UNDER CHALLENGE"
    } else {
        "SETTLED"
    }
}
fn object(r: &Record) -> JsonValue {
    let mut props = serde_json::Map::new();
    props.insert("id".into(), json!(r.id));
    for (k, v) in &r.props {
        props.insert(k.clone(), json!(v));
    }
    JsonValue::Object(props)
}
pub fn projection(view: &View) -> Result<JsonValue, CapError> {
    let marks: Vec<_> = view
        .records()
        .filter(|r| r.kind == "mark")
        .map(object)
        .collect();
    let claims: Vec<_> = view
        .records()
        .filter(|r| r.kind == "claim")
        .map(|r| {
            let mut v = object(r);
            v["state"] = json!(if flag(r, "withdrawn") {
                "WITHDRAWN"
            } else if string(r, "verdict") == "accepted" {
                "SETTLED"
            } else {
                "UNDER CHALLENGE"
            });
            v
        })
        .collect();
    let nodes:Vec<_>=view.records().filter(|r|r.kind=="node").map(|r|json!({"id":r.id,"label":string(r,"label"),"status":string(r,"status"),"touched_by":string(r,"touched_by"),"state":standing(view,&r.id)})).collect();
    Ok(json!({"marks":marks,"claims":claims,"context":context::project(view)?,"nodes":nodes}))
}
pub fn describe(view: &View) -> Result<String, CapError> {
    let mut out = String::from("REVIEW\n");
    let ctx = context::project(view)?;
    for node in view.records().filter(|r| r.kind == "node") {
        out.push_str(&format!(
            "[{}] \"{}\": {}\n",
            node.id,
            string(node, "label"),
            standing(view, &node.id)
        ));
        for claim in claims_of(view, &node.id) {
            out.push_str(&format!(
                "  [{}] {} ({}, {}{}){}\n",
                claim.id,
                string(claim, "text"),
                string(claim, "basis"),
                string(claim, "verdict"),
                match (string(claim, "path"), string(claim, "lines")) {
                    ("", _) => String::new(),
                    (path, "") => format!("; source {path}"),
                    (path, lines) => format!("; source {path}:{lines}"),
                } + &if string(claim, "revision").is_empty() {
                    String::new()
                } else {
                    format!("@{}", string(claim, "revision"))
                },
                if flag(claim, "withdrawn") {
                    " WITHDRAWN"
                } else {
                    ""
                }
            ));
        }
    }
    out.push_str("MARKS\n");
    for mark in view.records().filter(|r| r.kind == "mark") {
        let target = view
            .get(string(mark, "target"))
            .map(|r| {
                if r.kind == "claim" {
                    string(r, "text")
                } else {
                    string(r, "label")
                }
            })
            .unwrap_or("missing target");
        out.push_str(&format!(
            "[{}] {} on \"{}\": {} (by {})\n",
            mark.id,
            string(mark, "glyph"),
            target,
            string(mark, "text"),
            string(mark, "created_by")
        ));
        if !string(mark, "answer").is_empty() {
            out.push_str(&format!("  Answer: {}\n", string(mark, "answer")));
        }
        for id in string(mark, "context_ids").split(',') {
            if let Some(entry) = ctx.entries.iter().find(|e| e.id == id) {
                out.push_str(&format!(
                    "  asked at {} scope: {}\n",
                    entry.altitude.label(),
                    entry.question
                ));
            }
        }
    }
    out.push_str(&ctx.describe());
    out.push_str(report::EVIDENCE_BOUNDARY);
    Ok(out)
}
fn changes(before: &JsonValue, after: &JsonValue, _reader: &str) -> Vec<String> {
    let mut out = Vec::new();
    for section in ["marks", "claims", "nodes"] {
        let a = before[section].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let b = after[section].as_array().map(Vec::as_slice).unwrap_or(&[]);
        for item in b {
            let old = a.iter().find(|r| r["id"] == item["id"]);
            if old == Some(item) {
                continue;
            }
            let label = item["text"]
                .as_str()
                .or_else(|| item["label"].as_str())
                .unwrap_or("");
            let detail = match section {
                "marks" => format!(
                    "{}: {}{}",
                    item["glyph"].as_str().unwrap_or(""),
                    label,
                    if item["answer"].as_str().unwrap_or("").is_empty() {
                        String::new()
                    } else {
                        format!("; answer: {}", item["answer"].as_str().unwrap_or(""))
                    }
                ),
                "claims" => format!(
                    "{label}: {} ({})",
                    item["state"].as_str().unwrap_or(""),
                    item["verdict"].as_str().unwrap_or("")
                ),
                _ => format!(
                    "\"{label}\" is now {} (it was {}); {}",
                    item["status"].as_str().unwrap_or("open"),
                    old.and_then(|r| r["status"].as_str()).unwrap_or("absent"),
                    item["state"].as_str().unwrap_or("")
                ),
            };
            out.push(format!(
                "{} {section} [{}]: {detail}",
                if old.is_some() { "CHANGED" } else { "NEW" },
                item["id"].as_str().unwrap_or("")
            ));
        }
        for old in a {
            if !b.iter().any(|r| r["id"] == old["id"]) {
                out.push(format!(
                    "REMOVED {section} [{}]",
                    old["id"].as_str().unwrap_or("")
                ));
            }
        }
    }
    if before["context"] != after["context"] {
        out.push("SHARED SCOPE CHANGED: read the current question and reconcile concurrent heads before acting.".into());
    }
    out
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Review, Review);

/// Build the same evidence report natively and in the independent WASM artifact.
/// Facts are explicit data supplied by the native adapters, never I/O in WASM.
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn report_with_facts(view_json: &str, facts_json: &str) -> String {
    let result = (|| {
        let view: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", format!("view: {e}")))?;
        let facts: report::Facts = serde_json::from_str(facts_json)
            .map_err(|e| CapError::new("bad_input", format!("facts: {e}")))?;
        report::build(&view, &facts)
    })();
    match result {
        Ok(value) => json!({"abi":samepage_capability::ABI,"ok":value}).to_string(),
        Err(error) => json!({"abi":samepage_capability::ABI,"error":error}).to_string(),
    }
}
