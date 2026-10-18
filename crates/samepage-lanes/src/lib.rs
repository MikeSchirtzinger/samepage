//! Portable reconciliation of native extraction evidence with the shared Map.
use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CapError, Capability, Change, Delta, Descriptor, Effect,
    LegendEntry, Op, Plan, Record, Target, Trigger, View,
};
use samepage_sources::{Binding, Facts as SourceFacts, effective_bindings, string};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const ID: &str = "lanes";
pub const VERSION: &str = "0.1.0";
pub const KIND: &str = "binding";
pub const KEYS: &[&str] = &[
    "node",
    "lane_id",
    "path",
    "lines",
    "baseline_sha256",
    "scan_sha256",
];
fn invalid(s: impl Into<String>) -> CapError {
    CapError::new("lane_invalid", s)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub path: String,
    pub line: u32,
    pub snippet: String,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lane {
    pub lane_id: String,
    pub kind: String,
    pub label: String,
    pub package: Option<String>,
    pub detail: String,
    pub evidence: Evidence,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scan {
    pub root: String,
    pub lanes: Vec<Lane>,
    pub scanned_files: usize,
    pub languages: Vec<String>,
    pub skipped: Vec<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Removal {
    pub obligation_id: String,
    pub kind: String,
    pub label: String,
    pub path: String,
    pub line: u32,
    pub snippet: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub lane_claims: BTreeMap<String, String>,
    #[serde(default)]
    pub removals: BTreeMap<String, Removal>,
    #[serde(default)]
    pub last_seen: BTreeMap<String, Lane>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Facts {
    pub scan: Scan,
    pub registry: Registry,
    pub sources: SourceFacts,
    #[serde(default)]
    pub clean: BTreeSet<String>,
    #[serde(default)]
    pub excerpt_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub file_hashes: BTreeMap<String, String>,
}
pub fn descriptor() -> Descriptor {
    let mut claim = ActionDecl::new(
        "lane_claim",
        Audience::Both,
        Effect::Mutate,
        "Add a verified current execution lane to a Map card; retain its primary binding.",
    )
    .alias("atlas_claim_lane");
    claim.input_schema = json!({"type":"object","required":["lane_id"],"anyOf":[{"required":["node_id"]},{"required":["card_id"]}],"properties":{"lane_id":{"type":"string"},"node_id":{"type":"string"},"card_id":{"type":"string"},"scan_sha256":{"type":"string"}},"additionalProperties":false});
    let mut read = ActionDecl::new(
        "lanes_read",
        Audience::Both,
        Effect::Query,
        "Scan source and read grouped, unresolved execution-lane evidence. Static extraction is not runtime proof.",
    );
    read.input_schema = json!({"type":"object","properties":{},"additionalProperties":false});
    Descriptor::new(ID, VERSION, Artifact::Wasm)
        .depends_on(
            "sources",
            samepage_sources::VERSION,
            "Verified source bindings and facts",
        )
        .reads("node")
        .owns(
            KIND,
            KEYS,
            "Explicit extra bindings, baseline pins and host-stamped lane claims",
        )
        .action(claim)
        .action(read)
        .trigger(Trigger::kind(KIND))
        .trigger(Trigger::route("lanes"))
        .trigger(Trigger::tool("lane_claim"))
        .trigger(Trigger::tool("atlas_claim_lane"))
        .trigger(Trigger::tool("lanes_read"))
}
pub fn binding_matches(path: &str, lines: &str, lane: &Lane) -> bool {
    let path = path.trim().trim_start_matches("./");
    path == lane.evidence.path.trim().trim_start_matches("./")
        && lines
            .split('-')
            .next()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .is_some_and(|n| n.abs_diff(lane.evidence.line) <= 5)
}
pub fn extras(view: &View, node: &str, facts: &SourceFacts) -> Vec<Binding> {
    let mut out = facts.extras.get(node).cloned().unwrap_or_default();
    for r in view
        .records()
        .filter(|r| r.kind == KIND && string(r, "node") == node)
    {
        let b = Binding {
            path: string(r, "path").into(),
            lines: string(r, "lines").into(),
        };
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out
}
pub fn reconcile(view: &View, facts: &Facts) -> Value {
    let removal = |r: &Removal| {
        if r.obligation_id.is_empty() {
            json!({"tier":"unresolved","chip":"SOURCE VANISHED","obligation_id":null,"reason":"No longer found by static extraction; no removal obligation has been published."})
        } else if facts.clean.contains(&r.obligation_id) {
            json!({"tier":"gate-met","chip":"REMOVE met","obligation_id":r.obligation_id})
        } else {
            json!({"tier":"gate-unmet","chip":"REMOVE unmet","obligation_id":r.obligation_id})
        }
    };
    let mut seen = BTreeSet::new();
    let mut lanes:Vec<_>=facts.scan.lanes.iter().map(|lane| {
  seen.insert(lane.lane_id.clone());
  let matched=view.records().filter(|r|r.kind=="node").find(|n| effective_bindings(n,&extras(view,&n.id,&facts.sources)).iter().any(|b|binding_matches(&b.path,&b.lines,lane))).map(|n|n.id.clone()).or_else(|| facts.registry.lane_claims.get(&lane.lane_id).filter(|id|view.get(id).is_some_and(|r|r.kind=="node")).cloned());
  let r=matched.is_none().then(||facts.registry.removals.get(&lane.lane_id)).flatten().map(removal);
  json!({"lane_id":lane.lane_id,"kind":lane.kind,"label":lane.label,"package":lane.package,"detail":lane.detail,"evidence":lane.evidence,"matched_node_id":matched,"removal":r})
 }).collect();
    for (id, r) in &facts.registry.removals {
        if !seen.contains(id) {
            lanes.push(json!({"lane_id":id,"kind":r.kind,"label":r.label,"package":null,"detail":"no longer found in the code","evidence":{"path":r.path,"line":r.line,"snippet":r.snippet,"sha256":null},"matched_node_id":null,"removal":removal(r)}));
        }
    }
    let undeclared: Vec<_> = lanes
        .iter()
        .filter(|l| l["matched_node_id"].is_null())
        .cloned()
        .collect();
    // The legacy undeclared list retains every unmatched finding. A met
    // removal remains inspectable while no longer counting as unresolved.
    let unresolved_count = undeclared
        .iter()
        .filter(|l| l["removal"]["tier"] != "gate-met")
        .count();
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for l in &lanes {
        let key = format!(
            "{} · {}",
            l["kind"].as_str().unwrap_or("unknown"),
            l["package"].as_str().unwrap_or("project")
        );
        groups.entry(key).or_default().push(l.clone());
    }
    let groups:Vec<_>=groups.into_iter().map(|(name,all)|json!({"name":name,"count":all.len(),"shown":all.iter().take(20).collect::<Vec<_>>(),"more":all.len().saturating_sub(20)})).collect();
    let mut readback = format!(
        "LANES: {} unresolved; {} found or retained; {} source files scanned. Static extraction, not runtime proof.\n",
        unresolved_count,
        lanes.len(),
        facts.scan.scanned_files
    );
    for l in &lanes {
        readback.push_str(&format!(
            "[{}] {} {}:{} — {}{}\n",
            l["lane_id"].as_str().unwrap_or_default(),
            l["kind"].as_str().unwrap_or_default(),
            l["evidence"]["path"].as_str().unwrap_or_default(),
            l["evidence"]["line"],
            l["matched_node_id"]
                .as_str()
                .map(|id| format!("claimed by {id}"))
                .unwrap_or_else(|| "UNDECLARED".into()),
            l["removal"]["chip"]
                .as_str()
                .map(|s| format!("; {s}"))
                .unwrap_or_default()
        ));
    }
    json!({"revision":view.revision(),"root":facts.scan.root,"scanned_files":facts.scan.scanned_files,"languages":facts.scan.languages,"skipped":facts.scan.skipped,"lanes":lanes,"undeclared":undeclared,"unresolved_count":unresolved_count,"groups":groups,"readback":readback})
}
pub fn pin_check(lane: &Lane, facts: &Facts) -> Result<String, CapError> {
    if facts.file_hashes.get(&lane.lane_id) != Some(&lane.evidence.sha256) {
        return Err(invalid(
            "lane changed since extraction; rescan before claiming",
        ));
    }
    facts
        .excerpt_hashes
        .get(&lane.lane_id)
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .cloned()
        .ok_or_else(|| invalid("lane needs a readable native excerpt and baseline hash"))
}
pub fn claim_plan(call: &Call, view: &View, facts: &Facts) -> Result<Plan, CapError> {
    let id = call.args["lane_id"]
        .as_str()
        .ok_or_else(|| invalid("missing lane_id"))?;
    let node = call
        .args
        .get("node_id")
        .or_else(|| call.args.get("card_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing node_id or legacy card_id"))?;
    if call.args.get("node_id").is_some()
        && call
            .args
            .get("card_id")
            .is_some_and(|id| id.as_str() != Some(node))
    {
        return Err(invalid("node_id and card_id disagree"));
    }
    if !view.get(node).is_some_and(|r| r.kind == "node") {
        return Err(invalid("claim needs an existing Map card"));
    }
    let lane = facts
        .scan
        .lanes
        .iter()
        .find(|l| l.lane_id == id)
        .ok_or_else(|| invalid("lane is not present in the current scan"))?;
    if call
        .args
        .get("scan_sha256")
        .and_then(|v| v.as_str())
        .is_some_and(|h| h != lane.evidence.sha256)
    {
        return Err(invalid("inspected lane changed; rescan before claiming"));
    }
    let baseline = pin_check(lane, facts)?;
    let props = [
        ("node".into(), node.into()),
        ("lane_id".into(), id.into()),
        ("path".into(), lane.evidence.path.clone().into()),
        ("lines".into(), lane.evidence.line.to_string().into()),
        ("baseline_sha256".into(), baseline.into()),
        ("scan_sha256".into(), lane.evidence.sha256.clone().into()),
    ]
    .into();
    let op = if let Some(r) = view
        .records()
        .find(|r| r.kind == KIND && string(r, "lane_id") == id)
    {
        Op::Set {
            target: Target::Id(r.id.clone()),
            props,
        }
    } else {
        Op::Create {
            reference: "binding".into(),
            kind: KIND.into(),
            props,
        }
    };
    Ok(Plan {
        ops: vec![op],
        readback: format!(
            "Claimed lane {id} for card {node}; added {}:{} without replacing its primary binding.",
            lane.evidence.path, lane.evidence.line
        ),
    })
}
pub struct Lanes;
impl Capability for Lanes {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, view: &View) -> Result<Value, CapError> {
        Ok(reconcile(view, &Facts::default()))
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            (
                "state",
                "unresolved",
                "Unresolved source finding",
                "lane-finding",
                "? Unresolved",
            ),
            (
                "state",
                "claimed",
                "Finding claimed by a card",
                "lane-finding",
                "✓ Claimed",
            ),
            (
                "state",
                "removal",
                "Reviewed removal obligation",
                "lane-finding",
                "Removal",
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
    fn digest(&self, v: &View) -> Result<Value, CapError> {
        self.project(v)
    }
    fn plan(&self, c: &Call, _: &View) -> Result<Plan, CapError> {
        match c.action.as_str() {
            "lanes_read" => Ok(Plan {
                ops: vec![],
                readback: "Fresh lane evidence requires native extraction.".into(),
            }),
            "lane_claim" => Err(CapError::new(
                "facts_required",
                "lane_claim requires fresh native scan facts",
            )),
            _ => Err(CapError::new("unknown_action", &c.action)),
        }
    }
    fn validate(&self, d: &Delta, v: &View) -> Result<(), CapError> {
        for id in d.created.iter().chain(d.modified.keys()) {
            if let Some(r) = v.get(id).filter(|r| r.kind == KIND) {
                if !v.get(string(r, "node")).is_some_and(|n| n.kind == "node") {
                    return Err(invalid("binding target is missing"));
                }
                let bound = Record::new("binding", "node")
                    .with("path", string(r, "path"))
                    .with("lines", string(r, "lines"));
                samepage_sources::check_binding(&bound)?;
                for k in ["lane_id", "baseline_sha256", "scan_sha256"] {
                    if string(r, k).is_empty() {
                        return Err(invalid(format!("binding needs {k}")));
                    }
                }
            }
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        self.validate(c.delta, c.after)?;
        for id in c.attempted.keys() {
            if let Some(r) = c.after.get(id).filter(|r| r.kind == KIND) {
                let expected: Value =
                    serde_json::to_value(&r.props).map_err(|e| invalid(e.to_string()))?;
                if c.facts.and_then(|f| f.get(id)) != Some(&expected) {
                    return Err(CapError::new(
                        "lane_unverified",
                        "binding requires matching native lane evidence",
                    ));
                }
            }
        }
        Ok(())
    }
    fn cascade(&self, deleted: &[String], v: &View) -> Result<Plan, CapError> {
        Ok(Plan {
            ops: v
                .records()
                .filter(|r| r.kind == KIND && deleted.iter().any(|id| id == string(r, "node")))
                .map(|r| Op::Delete {
                    target: Target::Id(r.id.clone()),
                })
                .collect(),
            readback: String::new(),
        })
    }
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Lanes, Lanes);
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn project_with_facts(view_json: &str, facts_json: &str) -> String {
    envelope((|| {
        let v: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let f: Facts = serde_json::from_str(facts_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        Ok(reconcile(&v, &f))
    })())
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn claim_with_facts(call_json: &str, view_json: &str, facts_json: &str) -> String {
    envelope((|| {
        let c: Call = serde_json::from_str(call_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let v: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let f: Facts = serde_json::from_str(facts_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        serde_json::to_value(samepage_capability::Batch {
            base_revision: v.revision().into(),
            entries: vec![samepage_capability::BatchEntry {
                capability: ID.into(),
                plan: claim_plan(&c, &v, &f)?,
            }],
        })
        .map_err(|e| invalid(e.to_string()))
    })())
}
fn envelope(r: Result<Value, CapError>) -> String {
    match r {
        Ok(v) => json!({"abi":samepage_capability::ABI,"ok":v}).to_string(),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}).to_string(),
    }
}
