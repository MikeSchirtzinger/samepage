//! Walkthrough graphs, coherent cursors and scoped drawing. All portable
//! functions are shared by the native host and the independent WASM artifact.
pub mod model;
mod validation;
pub use model::*;
use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CallerKind, CapError, Capability,
    Change, Delta, Descriptor, Effect, Op, Plan, Record, Target, Trigger, Value, View,
};
use samepage_map::{prop_str, Map};
use samepage_sketch::Sketch;
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
pub const ID: &str = "explain";
pub const VERSION: &str = "0.1.0";
pub const KIND: &str = "explanation-flow";
pub const EXPLANATION_FLOW_SCHEMA: &str = "atlas-explanation-flow-v1";
pub const EXPLANATION_ACTION_KINDS: &[&str] = &["point", "reveal", "trace-path", "replay-motion"];
pub const EXPLANATION_ADVANCE_MODES: &[&str] = &["continue", "choice", "agent", "terminal"];
pub const EXPLANATION_STATUSES: &[&str] = &["active", "paused", "completed", "stopped"];
const MAX_LABEL: usize = 140;
const MAX_NOTE: usize = 4000;
const MAX_TEXT: usize = 1200;
const MAX_EXPLANATION_BEATS: usize = 32;
const MAX_EXPLANATION_EVIDENCE: usize = 16;
const MAX_EXPLANATION_ACTIONS: usize = 16;
const MAX_EXPLANATION_TRANSITIONS: usize = 8;
fn invalid(s: impl Into<String>) -> CapError {
    CapError::new("invalid", s)
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
    let mut d = ActionDecl::new(name, audience, effect, description);
    if let Some(alias) = alias {
        d = d.alias(alias);
    }
    d.input_schema = json!({"type":"object","required":required,"properties":props,"additionalProperties":false});
    d
}
pub fn descriptor() -> Descriptor {
    let mut d=Descriptor::new(ID,VERSION,Artifact::Wasm)
        .depends_on("map",samepage_map::VERSION,"Walkthrough evidence and scoped cards")
        .depends_on("sketch",samepage_sketch::VERSION,"Scoped ink and drawn connectors")
        .owns(KIND,&["definition","state"],"Finite named graphs and one revisioned cursor")
        .extends("node",&["exploration_id"],"Concurrent scoped card membership")
        .extends("shape",&["exploration_id"],"Concurrent scoped drawing membership")
        .reads("node").reads("edge").reads("shape").reads("constraint")
        .action(action("walkthrough_define",Some("atlas_explanation_define"),Audience::Agent,Effect::Mutate,&["flow"],json!({"flow":{"type":"object"}}),"Define or replace a finite named walkthrough. Replacing retains object identity, scope and marks. replay-motion is preserved and explicitly unsupported."))
        .action(action("walkthrough_advance",Some("atlas_explanation_advance"),Audience::Both,Effect::Mutate,&["flow_id","expected_revision","transition_id"],json!({"flow_id":{"type":"string"},"expected_revision":{"type":"integer","minimum":1,"maximum":4294967295u64},"transition_id":{"type":"string"},"response":{"type":"string","maxLength":1200},"selected_target_ids":{"type":"array","maxItems":32,"items":{"type":"string"}}}),"Advance an exact transition from the observed cursor revision with utterance and selection evidence. Phrases are resolver hints, not hidden grading."))
        .action(action("walkthrough_control",Some("atlas_explanation_control"),Audience::Both,Effect::Mutate,&["flow_id","expected_revision","command"],json!({"flow_id":{"type":"string"},"expected_revision":{"type":"integer","minimum":1,"maximum":4294967295u64},"command":{"enum":["pause","resume","restart","stop"]}}),"Pause, resume, restart or stop one named walkthrough from its observed revision."))
        .action(action("walkthrough_read",None,Audience::Both,Effect::Query,&[],json!({"flow_id":{"type":"string"}}),"Read all named walkthroughs or one flow, its coherent cursor, evidence and named unsupported visual actions."))
        .action(action("walkthrough_draw",None,Audience::Both,Effect::Mutate,&["flow_id"],json!({"flow_id":{"type":"string"},"shapes":{"type":"array","maxItems":2000,"items":{"type":"object"}},"nodes":{"type":"array","maxItems":512,"items":{"type":"object"}}}),"Draw cards and shapes inside an exploration in one Batch without resetting its cursor. An existing object must belong to this scope."))
        .trigger(Trigger::kind(KIND));
    d.visual_actions = EXPLANATION_ACTION_KINDS
        .iter()
        .map(|s| (*s).into())
        .collect();
    for a in &d.actions {
        d.triggers.push(Trigger::tool(&a.name));
        for alias in &a.aliases {
            d.triggers.push(Trigger::tool(alias));
        }
    }
    d
}
fn string<'a>(r: &'a Record, k: &str) -> &'a str {
    prop_str(r, k).unwrap_or("")
}
fn decode(r: &Record) -> Result<ExplanationFlow, CapError> {
    let definition = serde_json::from_str(string(r, "definition"))
        .map_err(|e| invalid(format!("invalid explanation definition: {e}")))?;
    let mut state: ExplanationFlowState = serde_json::from_str(string(r, "state"))
        .map_err(|e| invalid(format!("invalid explanation state: {e}")))?;
    // Only the host's record stamp is a byline. Legacy embedded author text
    // remains readable but never supplies authority or attributed read-back.
    state.touched_by = string(r, "touched_by").into();
    Ok(ExplanationFlow {
        object_id: r.id.clone(),
        definition,
        state,
        created_by: string(r, "created_by").into(),
    })
}
pub fn flows(view: &View) -> Result<Vec<ExplanationFlow>, CapError> {
    let mut flows = view
        .records()
        .filter(|r| r.kind == KIND)
        .map(decode)
        .collect::<Result<Vec<_>, _>>()?;
    let mut names = BTreeSet::new();
    let mut owners = BTreeSet::new();
    for flow in &mut flows {
        if !names.insert(flow.definition.id.clone()) {
            return Err(invalid(
                "conflicting exploration identity; resolve duplicate objects before continuing",
            ));
        }
        for r in view.records().filter(|r| {
            matches!(r.kind.as_str(), "node" | "shape")
                && string(r, "exploration_id") == flow.definition.id
        }) {
            if !flow.definition.scene_ids.contains(&r.id) {
                flow.definition.scene_ids.push(r.id.clone());
            }
        }
        for id in &flow.definition.scene_ids {
            if !owners.insert(id.clone()) {
                return Err(invalid(format!(
                    "conflicting exploration ownership of {id:?}"
                )));
            }
        }
        validate_structure(&flow.definition)?;
        validation::validate_explanation_state(&flow.definition, &flow.state).map_err(invalid)?;
    }
    Ok(flows)
}
fn flow(view: &View, id: &str) -> Result<ExplanationFlow, CapError> {
    flows(view)?
        .into_iter()
        .find(|f| f.definition.id == id)
        .ok_or_else(|| invalid(format!("explanation {id:?} is not on the page")))
}
fn exists(view: &View, id: &str) -> bool {
    view.get(id)
        .is_some_and(|r| matches!(r.kind.as_str(), "node" | "edge" | "shape" | "constraint"))
}
pub fn validate_structure(d: &ExplanationFlowDefinition) -> Result<(), CapError> {
    validation::validate_explanation_structure(d).map_err(invalid)
}
fn targets(d: &ExplanationFlowDefinition) -> impl Iterator<Item = &str> {
    d.about
        .iter()
        .chain(&d.scene_ids)
        .map(String::as_str)
        .chain(d.beats.iter().flat_map(|b| {
            b.evidence
                .iter()
                .map(|e| e.target_id.as_str())
                .chain(b.actions.iter().map(|a| a.target_id.as_str()))
                .chain(
                    b.advance
                        .transitions
                        .iter()
                        .flat_map(|t| t.target_ids.iter().map(String::as_str)),
                )
        }))
}
fn validate_targets(view: &View, d: &ExplanationFlowDefinition) -> Result<(), CapError> {
    for id in targets(d) {
        if !exists(view, id) {
            return Err(invalid(format!(
                "explanation target {id:?} is not on the page"
            )));
        }
    }
    for id in &d.scene_ids {
        if d.about.contains(id) {
            return Err(invalid(format!(
                "subject {id:?} cannot also be owned by its exploration"
            )));
        }
        if flows(view)?
            .iter()
            .any(|f| f.definition.id != d.id && f.definition.scene_ids.contains(id))
        {
            return Err(invalid(format!(
                "visual object {id:?} already belongs to another exploration"
            )));
        }
        if let Some(r) = view.get(id) {
            let owner = string(r, "exploration_id");
            if !owner.is_empty() && owner != d.id {
                return Err(invalid(
                    "visual object already belongs to another exploration",
                ));
            }
        }
    }
    for b in &d.beats {
        for a in &b.actions {
            if !d.scene_ids.is_empty() && !d.scene_ids.contains(&a.target_id) {
                return Err(invalid(format!(
                    "beat {:?} action target must be visible in scene_ids",
                    b.id
                )));
            }
            if a.kind == "trace-path"
                && !view.get(&a.target_id).is_some_and(|r| {
                    r.kind == "edge"
                        || (r.kind == "shape" && matches!(string(r, "form"), "arrow" | "line"))
                })
            {
                return Err(invalid(
                    "trace-path target must be a map link or a drawn arrow or line",
                ));
            }
            // replay-motion retains its stable target but has no removed runtime.
        }
    }
    Ok(())
}
fn status(b: &ExplanationBeat) -> String {
    if b.advance.mode == "terminal" {
        "completed"
    } else {
        "active"
    }
    .into()
}
fn increment(r: u32) -> Result<u32, CapError> {
    r.checked_add(1)
        .ok_or_else(|| invalid("explanation revision is exhausted"))
}
fn initial(d: &ExplanationFlowDefinition, revision: u32) -> ExplanationFlowState {
    ExplanationFlowState {
        current_beat: d.start.clone(),
        status: status(
            d.beats
                .iter()
                .find(|b| b.id == d.start)
                .expect("validated start"),
        ),
        revision,
        entered_revision: revision,
        last_transition: String::new(),
        last_response: String::new(),
        last_selected_target_ids: vec![],
        touched_by: String::new(),
    }
}
fn encoded<T: serde::Serialize>(v: &T) -> Result<Value, CapError> {
    Ok(serde_json::to_string(v)
        .map_err(|e| invalid(e.to_string()))?
        .into())
}
fn encoded_state(s: &ExplanationFlowState) -> Result<Value, CapError> {
    let mut value = serde_json::to_value(s).map_err(|e| invalid(e.to_string()))?;
    value
        .as_object_mut()
        .expect("state object")
        .remove("touched_by");
    encoded(&value)
}
fn state_plan(f: &ExplanationFlow, s: &ExplanationFlowState) -> Result<Plan, CapError> {
    Ok(Plan {
        ops: vec![Op::Set {
            target: Target::Id(f.object_id.clone()),
            props: [("state".into(), encoded_state(s)?)].into(),
        }],
        readback: format!(
            "explanation {:?}: beat {:?}, status={}, revision={}",
            f.definition.id, s.current_beat, s.status, s.revision
        ),
    })
}
fn advance(
    view: &View,
    f: &ExplanationFlow,
    transition_id: &str,
    response: &str,
    selection: &[String],
) -> Result<ExplanationFlowState, CapError> {
    if f.state.status != "active" {
        return Err(invalid(format!(
            "explanation is {}; resume or restart it before advancing",
            f.state.status
        )));
    }
    let beat = f
        .definition
        .beats
        .iter()
        .find(|b| b.id == f.state.current_beat)
        .ok_or_else(|| invalid("current beat is missing"))?;
    let t = beat
        .advance
        .transitions
        .iter()
        .find(|t| t.id == transition_id)
        .ok_or_else(|| {
            invalid(format!(
                "beat {:?} has no transition {transition_id:?}",
                beat.id
            ))
        })?;
    if selection.len() > 32 {
        return Err(invalid(
            "at most 32 selected targets may accompany a transition",
        ));
    }
    let mut selected = vec![];
    for id in selection {
        if !exists(view, id) {
            return Err(invalid(format!(
                "selected target {id:?} is not on the page"
            )));
        }
        if !selected.contains(id) {
            selected.push(id.clone());
        }
    }
    if !selected.is_empty()
        && !t.target_ids.is_empty()
        && !selected.iter().any(|id| t.target_ids.contains(id))
    {
        return Err(invalid("selection does not support transition"));
    }
    let next = t
        .next
        .as_ref()
        .and_then(|id| f.definition.beats.iter().find(|b| b.id == *id));
    if let Some(b) = next {
        for id in b
            .evidence
            .iter()
            .map(|e| &e.target_id)
            .chain(b.actions.iter().map(|a| &a.target_id))
        {
            if !exists(view, id) {
                return Err(invalid(format!(
                    "next beat depends on missing target {id:?}"
                )));
            }
        }
    }
    let revision = increment(f.state.revision)?;
    Ok(ExplanationFlowState {
        current_beat: t
            .next
            .clone()
            .unwrap_or_else(|| f.state.current_beat.clone()),
        status: next.map_or_else(|| "completed".into(), status),
        revision,
        entered_revision: revision,
        last_transition: t.id.clone(),
        last_response: samepage_map::rules::text(response, MAX_TEXT, "explanation response")?,
        last_selected_target_ids: selected,
        touched_by: String::new(),
    })
}
fn control(f: &ExplanationFlow, command: &str) -> Result<ExplanationFlowState, CapError> {
    let mut s = f.state.clone();
    s.touched_by.clear();
    match command {
        "pause" if s.status == "active" => s.status = "paused".into(),
        "resume" if s.status == "paused" => s.status = "active".into(),
        "restart" => s = initial(&f.definition, s.revision),
        "stop" if matches!(s.status.as_str(), "active" | "paused") => s.status = "stopped".into(),
        _ => {
            return Err(invalid(format!(
                "cannot {command} an explanation whose status is {}",
                s.status
            )))
        }
    }
    s.revision = increment(f.state.revision)?;
    if command == "restart" {
        s.entered_revision = s.revision;
    }
    Ok(s)
}
fn checked_flow(view: &View, args: &Json) -> Result<ExplanationFlow, CapError> {
    let f = flow(view, arg(args, "flow_id")?)?;
    let expected = args["expected_revision"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| invalid("expected_revision must be a positive u32"))?;
    if f.state.revision != expected {
        return Err(CapError::new(
            "stale",
            format!(
                "explanation revision changed: expected {expected}, current is {}",
                f.state.revision
            ),
        ));
    }
    Ok(f)
}
fn arg<'a>(v: &'a Json, k: &str) -> Result<&'a str, CapError> {
    v.get(k)
        .and_then(Json::as_str)
        .ok_or_else(|| invalid(format!("{k} must be text")))
}
fn args(call: &Call) -> Result<(), CapError> {
    let d = descriptor()
        .actions
        .into_iter()
        .find(|a| a.name == call.action)
        .ok_or_else(|| CapError::new("unknown_action", &call.action))?;
    let args = call
        .args
        .as_object()
        .ok_or_else(|| invalid("arguments must be an object"))?;
    for key in args.keys() {
        if d.input_schema["properties"].get(key).is_none() {
            return Err(invalid(format!("unknown argument {key}")));
        }
    }
    for key in d.input_schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(Json::as_str)
    {
        if !args.contains_key(key) {
            return Err(invalid(format!("missing argument {key}")));
        }
    }
    Ok(())
}
pub fn read(view: &View, id: Option<&str>) -> Result<Json, CapError> {
    let mut flows = flows(view)?;
    if let Some(id) = id {
        flows.retain(|f| f.definition.id == id);
        if flows.is_empty() {
            return Err(invalid("explanation is missing"));
        }
    }
    let unsupported=flows.iter().flat_map(|f|f.definition.beats.iter().flat_map(move|b|b.actions.iter().filter(|a|a.kind=="replay-motion").map(move|a|json!({"flow_id":f.definition.id,"beat_id":b.id,"kind":a.kind,"target_id":a.target_id,"status":"unsupported","reason":"replay-motion is unsupported"})))).collect::<Vec<_>>();
    let mut text = String::from("EXPLANATION FLOWS\n");
    for f in &flows {
        text.push_str(&format!("EXPLANATION FLOW {:?} [{}] {:?}: beat {:?}, status={}, revision={}, entered_revision={}, response={:?}, selected=[{}] (created by {}; last by {})\n",f.definition.id,f.object_id,f.definition.title,f.state.current_beat,f.state.status,f.state.revision,f.state.entered_revision,f.state.last_response,f.state.last_selected_target_ids.join(", "),f.created_by,f.state.touched_by));
        for b in &f.definition.beats {
            text.push_str(&format!("- {:?}: {}\n", b.id, b.cue));
            for a in &b.actions {
                text.push_str(&format!(
                    "  {} [{}]{}\n",
                    a.kind,
                    a.target_id,
                    if a.kind == "replay-motion" {
                        " UNSUPPORTED: replay-motion"
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    Ok(json!({"flows":flows,"unsupported":unsupported,"readback":text}))
}
pub struct Explain;
impl Capability for Explain {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, v: &View) -> Result<Json, CapError> {
        read(v, None)
    }
    fn digest(&self, v: &View) -> Result<Json, CapError> {
        read(v, None)
    }
    fn plan(&self, call: &Call, v: &View) -> Result<Plan, CapError> {
        args(call)?;
        let a = &call.args;
        match call.action.as_str() {
            "walkthrough_define" => {
                let mut d: ExplanationFlowDefinition = serde_json::from_value(a["flow"].clone())
                    .map_err(|e| invalid(e.to_string()))?;
                validate_structure(&d)?;
                let old = flows(v)?.into_iter().find(|f| f.definition.id == d.id);
                if let Some(f) = &old {
                    for id in &f.definition.scene_ids {
                        if v.get(id)
                            .is_some_and(|r| string(r, "exploration_id") == d.id)
                            && !d.scene_ids.contains(id)
                        {
                            d.scene_ids.push(id.clone());
                        }
                    }
                }
                validate_targets(v, &d)?;
                let revision = old
                    .as_ref()
                    .map_or(Ok(1), |f| increment(f.state.revision))?;
                let s = initial(&d, revision);
                let props = [
                    ("definition".into(), encoded(&d)?),
                    ("state".into(), encoded_state(&s)?),
                ]
                .into();
                let op = if let Some(f) = old {
                    Op::Set {
                        target: Target::Id(f.object_id),
                        props,
                    }
                } else {
                    Op::Create {
                        reference: "flow".into(),
                        kind: KIND.into(),
                        props,
                    }
                };
                Ok(Plan {
                    ops: vec![op],
                    readback: format!(
                        "explanation {:?} defined at {:?}, revision={revision}",
                        d.id, s.current_beat
                    ),
                })
            }
            "walkthrough_advance" => {
                let f = checked_flow(v, a)?;
                let selection: Vec<String> = serde_json::from_value(
                    a.get("selected_target_ids").cloned().unwrap_or(json!([])),
                )
                .map_err(|e| invalid(e.to_string()))?;
                let response = match a.get("response") {
                    None => "",
                    Some(_) => arg(a, "response")?,
                };
                state_plan(
                    &f,
                    &advance(v, &f, arg(a, "transition_id")?, response, &selection)?,
                )
            }
            "walkthrough_control" => {
                let f = checked_flow(v, a)?;
                state_plan(&f, &control(&f, arg(a, "command")?)?)
            }
            "walkthrough_read" => {
                let id = if a.get("flow_id").is_some() {
                    Some(arg(a, "flow_id")?)
                } else {
                    None
                };
                Ok(Plan {
                    ops: vec![],
                    readback: read(v, id)?["readback"].as_str().unwrap_or_default().into(),
                })
            }
            "walkthrough_draw" => Err(invalid("scoped drawing requires plan_batch")),
            _ => Err(CapError::new("unknown_action", &call.action)),
        }
    }
    fn plan_batch(&self, call: &Call, v: &View) -> Result<Batch, CapError> {
        args(call)?;
        let entries = if call.action == "walkthrough_draw" {
            draw(v, &call.args)?
        } else {
            vec![entry(ID, self.plan(call, v)?)]
        };
        Ok(Batch {
            base_revision: v.revision().into(),
            entries,
        })
    }
    fn validate(&self, delta: &Delta, v: &View) -> Result<(), CapError> {
        let all = flows(v)?;
        for f in &all {
            if delta.created.contains(&f.object_id) || delta.modified.contains_key(&f.object_id) {
                validate_targets(v, &f.definition)?;
            }
        }
        for r in v
            .records()
            .filter(|r| matches!(r.kind.as_str(), "node" | "shape"))
        {
            if r.props.contains_key("exploration_id") {
                let owner = prop_str(r, "exploration_id")
                    .ok_or_else(|| invalid("exploration_id must be text"))?;
                if !owner.is_empty()
                    && all
                        .iter()
                        .find(|f| f.definition.id == owner)
                        .is_none_or(|f| {
                            f.definition.scene_ids.is_empty() || f.definition.about.contains(&r.id)
                        })
                {
                    return Err(invalid(
                        "scoped object needs an existing exploration and cannot own its subject",
                    ));
                }
            }
        }
        for id in &delta.deleted {
            for f in &all {
                if targets(&f.definition).any(|t| t == id) {
                    return Err(invalid(format!(
                        "explain retains target {id}; revise or remove its flow in the same batch"
                    )));
                }
            }
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        self.validate(c.delta, c.after)?;
        for (id, keys) in c.attempted {
            let Some(r) = c.after.get(id) else { continue };
            if r.kind == KIND {
                let next = decode(r)?;
                let mut proposed = next.state.clone();
                proposed.touched_by.clear();
                let raw: ExplanationFlowState =
                    serde_json::from_str(string(r, "state")).map_err(|e| invalid(e.to_string()))?;
                if !raw.touched_by.is_empty() {
                    return Err(invalid(
                        "cursor byline belongs to the host, not state input",
                    ));
                }
                if let Some(old) = c.before.get(id) {
                    let prior = flow(c.before, &decode(old)?.definition.id)?;
                    if next.definition.id != prior.definition.id {
                        return Err(invalid("explanation identity cannot change"));
                    }
                    if keys.contains("definition") {
                        if c.principal.caller != CallerKind::Agent {
                            return Err(invalid("only the agent defines explanations"));
                        }
                        if proposed != initial(&next.definition, increment(prior.state.revision)?) {
                            return Err(invalid(
                                "definition replacement needs one coherent reset cursor",
                            ));
                        }
                    } else if keys.contains("state") {
                        let advanced = advance(
                            c.after,
                            &prior,
                            &proposed.last_transition,
                            &proposed.last_response,
                            &proposed.last_selected_target_ids,
                        )
                        .ok();
                        let controlled = ["pause", "resume", "restart", "stop"]
                            .iter()
                            .any(|cmd| control(&prior, cmd).is_ok_and(|s| s == proposed));
                        if advanced.as_ref() != Some(&proposed) && !controlled {
                            return Err(CapError::new(
                                "stale",
                                "cursor transition is stale or invalid; read its current revision",
                            ));
                        }
                    }
                } else if c.principal.caller != CallerKind::Agent
                    || proposed != initial(&next.definition, 1)
                {
                    return Err(invalid(
                        "only the agent creates a flow with its initial cursor",
                    ));
                }
            } else if keys.contains("exploration_id") {
                let owner = string(r, "exploration_id");
                if let Some(old) = c.before.get(id) {
                    let before = string(old, "exploration_id");
                    if !before.is_empty() && before != owner {
                        return Err(invalid("cannot move an object outside its exploration"));
                    }
                }
                let f = flow(c.after, owner)?;
                if c.before.contains(id)
                    && !flow(c.before, owner)?.definition.scene_ids.contains(id)
                {
                    return Err(invalid("cannot edit an object outside this exploration"));
                }
                if f.definition.scene_ids.len() > 512 {
                    return Err(invalid(
                        "exploration has reached its 512 visual object limit",
                    ));
                }
            }
        }
        Ok(())
    }
    fn changes(&self, before: &Json, after: &Json, _reader: &str) -> Result<Vec<String>, CapError> {
        let mut out = vec![];
        for f in after["flows"].as_array().into_iter().flatten() {
            let old = before["flows"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|old| old["object_id"] == f["object_id"]);
            if old != Some(f) {
                out.push(format!(
                    "Walkthrough {} is at {}, {}, revision {}",
                    f["definition"]["id"],
                    f["state"]["current_beat"],
                    f["state"]["status"],
                    f["state"]["revision"]
                ));
            }
        }
        Ok(out)
    }
}
fn entry(id: &str, plan: Plan) -> BatchEntry {
    BatchEntry {
        capability: id.into(),
        plan,
    }
}
fn draw(v: &View, a: &Json) -> Result<Vec<BatchEntry>, CapError> {
    let f = flow(v, arg(a, "flow_id")?)?;
    let scoped = !f.definition.scene_ids.is_empty();
    let mut entries = vec![];
    let mut ownership = vec![];
    let mut count = f.definition.scene_ids.len();
    let mut fresh_nodes = BTreeMap::new();
    let mut virtual_records = v.records().cloned().collect::<Vec<_>>();
    if let Some(nodes) = a.get("nodes") {
        let nodes = nodes
            .as_array()
            .ok_or_else(|| invalid("nodes must be an array"))?;
        for (i, node) in nodes.iter().enumerate() {
            if let Some(id) = node.get("id").and_then(Json::as_str) {
                if scoped && !f.definition.scene_ids.iter().any(|s| s == id) {
                    return Err(invalid("cannot edit an object outside this exploration"));
                }
            }
            let virtual_view = View::new(v.revision(), virtual_records.clone())
                .map_err(|e| invalid(e.to_string()))?;
            let mut plan = Map.plan(
                &Call {
                    action: "card_place".into(),
                    args: {
                        let mut patch = node.clone();
                        patch
                            .as_object_mut()
                            .ok_or_else(|| invalid("node patch must be an object"))?
                            .remove("ref");
                        patch
                    },
                },
                &virtual_view,
            )?;
            for op in &mut plan.ops {
                if let Op::Create {
                    reference,
                    kind,
                    props,
                } = op
                {
                    *reference = format!("exploration-card-{i}");
                    let temp = format!("@{reference}");
                    fresh_nodes.insert(reference.clone(), temp.clone());
                    if let Some(name) = node["ref"].as_str() {
                        fresh_nodes.insert(name.to_string(), temp.clone());
                    }
                    let r = Record::new(&temp, kind.clone());
                    virtual_records.push(props.iter().fold(r, |mut r, (k, val)| {
                        if let Value::Scalar(s) = val {
                            r.props.insert(k.clone(), s.clone());
                        }
                        r
                    }));
                    if scoped {
                        ownership.push(Op::Set {
                            target: Target::Ref(reference.clone()),
                            props: [("exploration_id".into(), f.definition.id.clone().into())]
                                .into(),
                        });
                        count += 1;
                    }
                }
            }
            entries.push(entry("map", plan));
        }
    }
    if let Some(shapes) = a.get("shapes") {
        let shapes = shapes
            .as_array()
            .ok_or_else(|| invalid("shapes must be an array"))?;
        for shape in shapes {
            if let Some(id) = shape.get("id").and_then(Json::as_str) {
                if scoped && !f.definition.scene_ids.contains(&id.to_string()) {
                    return Err(invalid("cannot edit an object outside this exploration"));
                }
            }
        }
        let virtual_view =
            View::new(v.revision(), virtual_records).map_err(|e| invalid(e.to_string()))?;
        let mut patches = shapes.clone();
        for patch in &mut patches {
            for key in ["from", "to"] {
                if let Some(id) = patch[key]
                    .as_str()
                    .and_then(|id| fresh_nodes.get(id))
                    .cloned()
                {
                    patch[key] = json!(id);
                }
            }
        }
        let mut plan = Sketch.plan(
            &Call {
                action: "sketch".into(),
                args: json!({"shapes":patches}),
            },
            &virtual_view,
        )?;
        for op in &mut plan.ops {
            if let Op::Create { props, .. } | Op::Set { props, .. } = op {
                for key in ["from", "to"] {
                    if let Some(Value::Scalar(samepage_capability::Scalar::Str(id))) =
                        props.get(key)
                    {
                        if let Some(reference) = id.strip_prefix("@exploration-card-") {
                            props.insert(
                                key.into(),
                                Value::reference(&format!("exploration-card-{reference}")),
                            );
                        }
                    }
                }
            }
            if let Op::Create { reference, .. } = op {
                if scoped {
                    ownership.push(Op::Set {
                        target: Target::Ref(reference.clone()),
                        props: [("exploration_id".into(), f.definition.id.clone().into())].into(),
                    });
                    count += 1;
                }
            }
        }
        entries.push(entry("sketch", plan));
    }
    if entries.is_empty() {
        return Err(invalid("walkthrough_draw needs nodes or shapes"));
    }
    if count > 512 {
        return Err(invalid(
            "exploration has reached its 512 visual object limit",
        ));
    }
    entries.push(entry(
        ID,
        Plan {
            ops: ownership,
            readback: format!(
                "Drawing retained exploration {:?} and cursor revision {}",
                f.definition.id, f.state.revision
            ),
        },
    ));
    Ok(entries)
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Explain, Explain);
