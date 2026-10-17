//! Portable architecture proposals. The Map is captured, never mutated.
mod graph;
pub use graph::*;

use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CallerKind, CapError, Capability, Change, Delta,
    Descriptor, Effect, LegendEntry, Op, Origin, Plan, Record, Target, Trigger, Value, View,
};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::collections::BTreeSet;

pub const ID: &str = "architecture";
pub const VERSION: &str = "0.1.0";
pub(crate) fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("architecture_invalid", message)
}
pub fn string<'a>(record: &'a Record, key: &str) -> &'a str {
    samepage_map::prop_str(record, key).unwrap_or_default()
}

pub fn descriptor() -> Descriptor {
    let edge = json!({"type":"object","required":["from","to","label"],"properties":{"from":{"type":"string"},"to":{"type":"string"},"label":{"type":"string","maxLength":500}},"additionalProperties":false});
    let mut propose = ActionDecl::new("architecture_propose", Audience::Both, Effect::Mutate,
        "Record a relationship proposal without changing the map. Read architecture_read first; each revision retains its captured baseline and alternatives.").alias("atlas_architecture_propose");
    propose.input_schema = json!({"type":"object","required":["title","baseline_revision"],"properties":{
        "title":{"type":"string","minLength":1,"maxLength":160},"baseline_revision":{"type":"string"},"previous":{"type":"string"},
        "remove":{"type":"array","maxItems":4096,"items":edge},"add":{"type":"array","maxItems":4096,"items":edge}},"additionalProperties":false});
    let mut read = ActionDecl::new("architecture_read", Audience::Both, Effect::Query,
        "Read the semantic graph revision, proposal comparisons, potential upstream paths and human verdicts. Layout changes do not stale a baseline.");
    read.input_schema = json!({"type":"object","properties":{},"additionalProperties":false});
    let mut decide = ActionDecl::new("architecture_decide", Audience::Human, Effect::Mutate,
        "Accept or reject one retained proposal revision. This human verdict records agreement, without applying relationships or cementing code.");
    decide.input_schema = json!({"type":"object","required":["id","verdict"],"properties":{"id":{"type":"string"},"verdict":{"enum":["accepted","rejected"]}},"additionalProperties":false});
    let mut d = Descriptor::new(ID, VERSION, Artifact::Wasm)
        .depends_on(
            "map",
            samepage_map::VERSION,
            "Capture the authored Map graph",
        )
        .owns(
            KIND,
            &["definition", "verdict"],
            "Retained proposal revisions and human verdicts",
        )
        .reads("node")
        .reads("edge")
        .action(propose)
        .action(read)
        .action(decide)
        .trigger(Trigger::kind(KIND));
    for a in &d.actions {
        d.triggers.push(Trigger::tool(&a.name));
        for alias in &a.aliases {
            d.triggers.push(Trigger::tool(alias));
        }
    }
    d
}

pub fn proposals(view: &View) -> Result<Vec<Proposal>, CapError> {
    view.records()
        .filter(|r| r.kind == KIND)
        .map(|r| {
            let mut p = graph::project(
                &r.id,
                string(r, "definition"),
                string(r, "created_by").into(),
            )?;
            p.verdict = match r.props.get("verdict") {
                None => "open".into(),
                Some(v) if matches!(v.as_str(), Some("accepted" | "rejected")) => {
                    v.as_str().unwrap().into()
                }
                _ => return Err(invalid("proposal verdict must be accepted or rejected")),
            };
            if p.verdict != "open" {
                p.decided_by = string(r, "touched_by").into();
            }
            Ok(p)
        })
        .collect()
}

fn propose(input: &ProposalInput, view: &View) -> Result<Plan, CapError> {
    let all = proposals(view)?;
    if all.len() >= 256 {
        return Err(invalid(
            "this page has reached its 256 proposal revision limit",
        ));
    }
    let (baseline, mut proposed) = if input.previous.is_empty() {
        let graph = Graph::capture(view);
        (graph.clone(), graph)
    } else {
        let prev = all
            .iter()
            .find(|p| p.id == input.previous)
            .ok_or_else(|| invalid("the previous proposal does not exist"))?;
        (
            prev.definition.baseline.clone(),
            prev.definition.proposed.clone(),
        )
    };
    if baseline.revision()? != input.baseline_revision {
        return Err(invalid(
            "the comparison baseline changed; read it again before proposing",
        ));
    }
    let mut relationships: BTreeSet<_> = proposed.relationships.into_iter().collect();
    for e in &input.remove {
        if !relationships.remove(e) {
            return Err(invalid(
                "cannot remove a relationship absent from this proposal revision",
            ));
        }
    }
    for e in &input.add {
        if !relationships.insert(e.clone()) {
            return Err(invalid(
                "that relationship already exists in this proposal revision",
            ));
        }
    }
    proposed.relationships = relationships.into_iter().collect();
    let definition = Definition {
        title: input.title.trim().into(),
        previous: input.previous.clone(),
        baseline,
        proposed,
    };
    graph::validate(&definition)?;
    let definition = serde_json::to_string(&definition).map_err(|e| invalid(e.to_string()))?;
    Ok(Plan { ops:vec![Op::Create { reference:"proposal".into(), kind:KIND.into(), props:[("definition".into(), Value::from(definition))].into() }], readback:"Architecture proposal recorded; current relationships unchanged. Read architecture_read to compare.".into() })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: String,
    verdict: String,
}

pub struct Architecture;
impl Capability for Architecture {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, view: &View) -> Result<JsonValue, CapError> {
        inspect(view)
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            ("element", "added", "Added relationship", "edge-added", "+"),
            (
                "element",
                "removed",
                "Removed relationship",
                "edge-removed",
                "−",
            ),
            (
                "element",
                "direct",
                "Directly affected component",
                "comparison-node node-direct",
                "DIRECT",
            ),
            (
                "element",
                "upstream",
                "Upstream component to review",
                "comparison-node node-upstream",
                "UPSTREAM",
            ),
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
    fn digest(&self, view: &View) -> Result<JsonValue, CapError> {
        inspect(view)
    }
    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        match call.action.as_str() {
            "architecture_propose" => propose(
                &serde_json::from_value(call.args.clone()).map_err(|e| invalid(e.to_string()))?,
                view,
            ),
            "architecture_read" => {
                if call.args != json!({}) {
                    return Err(invalid("architecture_read takes an empty object"));
                }
                Ok(Plan {
                    ops: vec![],
                    readback: describe(view)?,
                })
            }
            "architecture_decide" => {
                let d: Decision = serde_json::from_value(call.args.clone())
                    .map_err(|e| invalid(e.to_string()))?;
                if !matches!(d.verdict.as_str(), "accepted" | "rejected") {
                    return Err(invalid("human verdict must be accepted or rejected"));
                }
                view.get(&d.id)
                    .filter(|r| r.kind == KIND)
                    .ok_or_else(|| invalid("proposal revision does not exist"))?;
                Ok(Plan {
                    ops: vec![Op::Set {
                        target: Target::Id(d.id),
                        props: [("verdict".into(), Value::from(d.verdict.clone()))].into(),
                    }],
                    readback: format!(
                        "Human {0} this proposal revision; current relationships unchanged.",
                        d.verdict
                    ),
                })
            }
            _ => Err(CapError::new("unknown_action", &call.action)),
        }
    }
    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        for id in delta.created.iter().chain(delta.modified.keys()) {
            let Some(r) = view.get(id).filter(|r| r.kind == KIND) else {
                continue;
            };
            let p = graph::project(&r.id, string(r, "definition"), String::new())?;
            if let Some(v) = r.props.get("verdict")
                && !matches!(v.as_str(), Some("accepted" | "rejected")) {
                    return Err(invalid("proposal verdict must be accepted or rejected"));
                }
            let mut seen = BTreeSet::from([r.id.as_str()]);
            let mut prev = p.definition.previous;
            let baseline = p.definition.baseline;
            while !prev.is_empty() {
                if !seen.insert(view.get(&prev).map(|r| r.id.as_str()).unwrap_or("")) {
                    return Err(invalid("proposal revision history is cyclic"));
                }
                let prior = view
                    .get(&prev)
                    .filter(|r| r.kind == KIND)
                    .ok_or_else(|| invalid("the previous proposal does not exist"))?;
                let prior = graph::project(&prior.id, string(prior, "definition"), String::new())?;
                if prior.definition.baseline != baseline {
                    return Err(invalid(
                        "a revision must retain its previous captured baseline",
                    ));
                }
                prev = prior.definition.previous;
            }
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        for id in &c.delta.deleted {
            if c.before.get(id).is_some_and(|r| r.kind == KIND) {
                return Err(invalid("proposal history cannot be deleted"));
            }
        }
        for (id, keys) in c.attempted {
            let Some(r) = c.after.get(id).filter(|r| r.kind == KIND) else {
                continue;
            };
            if c.before.contains(id) {
                if keys.contains("definition") {
                    return Err(invalid(
                        "proposal definitions are immutable; create a separate revision",
                    ));
                }
                if keys.contains("verdict") && c.principal.caller != CallerKind::Human {
                    return Err(CapError::new(
                        "architecture_human_only",
                        "only the human can accept or reject an architecture proposal",
                    ));
                }
            } else {
                if keys.contains("verdict") {
                    return Err(invalid(
                        "a new proposal starts open; give its human verdict separately",
                    ));
                }
                let p = graph::project(id, string(r, "definition"), String::new())?;
                if !matches!(c.origin, Origin::ReplicaFrame)
                    && p.definition.previous.is_empty()
                    && p.definition.baseline != Graph::capture(c.before)
                {
                    return Err(invalid(
                        "the comparison baseline changed; read it again before proposing",
                    ));
                }
                if c.after.records().filter(|r| r.kind == KIND).count() > 256 {
                    return Err(invalid(
                        "this page has reached its 256 proposal revision limit",
                    ));
                }
            }
        }
        self.validate(c.delta, c.after)
    }
    fn changes(
        &self,
        before: &JsonValue,
        after: &JsonValue,
        reader: &str,
    ) -> Result<Vec<String>, CapError> {
        let a = before["comparisons"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let b = after["comparisons"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Ok(b.iter()
            .filter(|c| !a.iter().any(|old| old == *c))
            .filter(|c| {
                let p = &c["proposal"];
                let old = a.iter().find(|old| old["proposal"]["id"] == p["id"]);
                let author = if old.is_some() && old.unwrap()["proposal"]["verdict"] != p["verdict"]
                {
                    &p["decided_by"]
                } else {
                    &p["created_by"]
                };
                author.as_str() != Some(reader)
                    || old.is_some_and(|o| o["baseline_changed"] != c["baseline_changed"])
            })
            .map(|c| {
                format!(
                    "ARCHITECTURE [{}] {} by {}: {} added, {} removed; verdict {}; baseline {}",
                    c["proposal"]["id"].as_str().unwrap_or(""),
                    c["proposal"]["definition"]["title"].as_str().unwrap_or(""),
                    c["proposal"]["created_by"].as_str().unwrap_or(""),
                    c["added"].as_array().map_or(0, Vec::len),
                    c["removed"].as_array().map_or(0, Vec::len),
                    c["proposal"]["verdict"].as_str().unwrap_or("open"),
                    if c["baseline_changed"] == true {
                        "changed"
                    } else {
                        "matches map"
                    }
                )
            })
            .collect())
    }
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Architecture, Architecture);
