//! Source bindings and trust computed from native evidence, shared with WASM.
//! Repository, Git, baseline persistence and caller identity belong to the host.
use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CapError, Capability, Change, Delta,
    Descriptor, Effect, LegendEntry, Op, Plan, Record, Target, Trigger, View,
};
use samepage_map::{prop_str, Map};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const ID: &str = "sources";
pub const VERSION: &str = "0.1.0";
pub const KEYS: &[&str] = &["path", "lines"];
fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("source_invalid", message)
}
pub fn string<'a>(r: &'a Record, key: &str) -> &'a str {
    prop_str(r, key).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub path: String,
    pub lines: String,
}

/// Native data, never a precomputed tier. Gate evidence is contributed by Cement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    #[serde(default)]
    pub extras: BTreeMap<String, Vec<Binding>>,
    #[serde(default)]
    pub bindings: BTreeMap<String, BindingFact>,
    #[serde(default)]
    pub gates: BTreeMap<String, GateFact>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingFact {
    pub readable: bool,
    pub current_sha256: Option<String>,
    pub baseline_sha256: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateFact {
    pub obligation_id: String,
    pub met: bool,
    pub reason: String,
    /// Why an unmet gate is unmet: `unratified`, `unattested`, `failed`,
    /// `changed`, `missing` or `blocked`. Empty when the gate is met.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blocker: String,
}
pub fn binding_key(node: &str, b: &Binding) -> String {
    format!("{node}\u{1}{}\u{1}{}", b.path, b.lines)
}
pub fn effective_bindings(node: &Record, extras: &[Binding]) -> Vec<Binding> {
    let mut out = Vec::new();
    if !string(node, "path").trim().is_empty() {
        out.push(Binding {
            path: string(node, "path").into(),
            lines: string(node, "lines").into(),
        });
    }
    for b in extras {
        if !out.contains(b) {
            out.push(b.clone());
        }
    }
    out
}
pub fn range(lines: &str) -> Result<(usize, usize), CapError> {
    let (a, b) = lines.split_once('-').unwrap_or((lines, lines));
    let a = a
        .parse::<usize>()
        .map_err(|_| invalid("lines must be N or N-M"))?;
    let b = b
        .parse::<usize>()
        .map_err(|_| invalid("lines must be N or N-M"))?;
    if a == 0 || b < a || b - a >= 400 {
        return Err(invalid(
            "lines must be a forward range of at most 400 lines",
        ));
    }
    Ok((a, b))
}
pub fn check_binding(r: &Record) -> Result<(), CapError> {
    for key in KEYS {
        if r.props.get(*key).is_some_and(|v| v.as_str().is_none()) {
            return Err(invalid(format!("{key} must be text")));
        }
    }
    let p = string(r, "path");
    let l = string(r, "lines");
    if p.is_empty() && !l.is_empty() {
        return Err(invalid("lines needs a path"));
    }
    if !p.is_empty() && l.is_empty() {
        return Err(invalid(format!("{p}: pass lines, such as 1-40")));
    }
    if !p.is_empty() {
        if p.starts_with('/')
            || p.starts_with('~')
            || p.contains('\\')
            || p.chars().any(char::is_control)
            || p.split('/').any(|s| s.is_empty() || s == "." || s == "..")
        {
            return Err(invalid("path must be project-relative"));
        }
        range(l)?;
    }
    Ok(())
}

pub fn descriptor() -> Descriptor {
    let mut d = Descriptor::new(ID, VERSION, Artifact::Wasm)
        .depends_on(
            "map",
            samepage_map::VERSION,
            "Sources binds and projects Map cards",
        )
        .extends("node", KEYS, "Source bindings checked before publication")
        .reads("node")
        .reads("edge")
        .trigger(Trigger::property("node", "path"))
        .trigger(Trigger::route("sources"));
    for (name, alias, required, properties, description) in [
        (
            "source_list",
            Some("repo_list"),
            vec![],
            json!({"path":{"type":"string"}}),
            "List readable project files.",
        ),
        (
            "source_read",
            Some("repo_read"),
            vec!["path"],
            json!({"path":{"type":"string"},"lines":{"type":"string"},"revision":{"type":"string"}}),
            "Read a host-resolved source excerpt, optionally at a full Git commit.",
        ),
        (
            "sources_read",
            None,
            vec![],
            json!({}),
            "Read fresh source evidence and computed tiers for this page.",
        ),
    ] {
        let mut a = ActionDecl::new(name, Audience::Both, Effect::Query, description);
        if let Some(alias) = alias {
            a = a.alias(alias);
        }
        a.input_schema = json!({"type":"object","required":required,"properties":properties,"additionalProperties":false});
        d = d.trigger(Trigger::tool(name));
        for alias in &a.aliases {
            d = d.trigger(Trigger::tool(alias));
        }
        d = d.action(a);
    }
    d
}
pub struct Sources;
impl Capability for Sources {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, view: &View) -> Result<Value, CapError> {
        project(view, &Facts::default())
    }
    fn digest(&self, view: &View) -> Result<Value, CapError> {
        self.project(view)
    }
    fn plan(&self, call: &Call, _view: &View) -> Result<Plan, CapError> {
        let d = descriptor();
        let a = d
            .actions
            .iter()
            .find(|a| a.name == call.action)
            .ok_or_else(|| CapError::new("unknown_action", &call.action))?;
        let args = call
            .args
            .as_object()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        for (key, value) in args {
            if a.input_schema["properties"].get(key).is_none() || !value.is_string() {
                return Err(invalid(format!("invalid argument {key}")));
            }
        }
        for required in a.input_schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !args.contains_key(required) {
                return Err(invalid(format!("missing {required}")));
            }
        }
        Ok(Plan {
            ops: vec![],
            readback: "Source evidence requires fresh native repository facts.".into(),
        })
    }
    fn plan_batch(&self, call: &Call, view: &View) -> Result<Batch, CapError> {
        if call.action == "card_place" {
            place_batch(call, view)
        } else {
            Ok(Batch {
                base_revision: view.revision().into(),
                entries: vec![BatchEntry {
                    capability: ID.into(),
                    plan: self.plan(call, view)?,
                }],
            })
        }
    }
    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        for id in delta.created.iter().chain(delta.modified.keys()) {
            if let Some(r) = view.get(id).filter(|r| r.kind == "node") {
                check_binding(r)?;
            }
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        self.validate(c.delta, c.after)?;
        for (id, keys) in c.attempted {
            if let Some(r) = c.after.get(id).filter(|r| r.kind == "node") {
                if keys.iter().any(|k| KEYS.contains(&k.as_str())) && !string(r, "path").is_empty()
                {
                    let expected = json!({"path":string(r,"path"),"lines":string(r,"lines")});
                    if c.facts.and_then(|f| f.get(id)) != Some(&expected) {
                        return Err(CapError::new(
                            "source_unverified",
                            format!("{id}: source binding needs matching native facts"),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            (
                "tier",
                "verified",
                "VERIFIED",
                "source-tier source-tier-verified",
                "✓",
            ),
            (
                "tier",
                "drifted",
                "DRIFTED",
                "source-tier source-tier-drifted",
                "△",
            ),
            (
                "tier",
                "claimed",
                "CLAIMED",
                "source-tier source-tier-claimed",
                "?",
            ),
            (
                "tier",
                "proposed",
                "PROPOSED",
                "source-tier source-tier-proposed",
                "↪",
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
}

/// The native coordinator uses this pure plan for the existing card_place action.
/// Map creates/edits the card, then Sources binds it in that same transaction.
pub fn place_batch(call: &Call, view: &View) -> Result<Batch, CapError> {
    if call.action != "card_place" {
        return Err(CapError::new("unknown_action", &call.action));
    }
    let mut args = call.args.clone();
    let obj = args
        .as_object_mut()
        .ok_or_else(|| invalid("arguments must be an object"))?;
    let mut props = BTreeMap::new();
    for k in KEYS {
        if let Some(v) = obj.remove(*k) {
            props.insert(
                (*k).into(),
                v.as_str()
                    .ok_or_else(|| invalid(format!("{k} must be text")))?
                    .into(),
            );
        }
    }
    let map = Map.plan(
        &Call {
            action: call.action.clone(),
            args,
        },
        view,
    )?;
    let target = call.args["id"]
        .as_str()
        .map(|id| Target::Id(id.into()))
        .unwrap_or_else(|| Target::Ref("card".into()));
    let mut entries = vec![BatchEntry {
        capability: "map".into(),
        plan: map,
    }];
    if !props.is_empty() {
        entries.push(BatchEntry {
            capability: ID.into(),
            plan: Plan {
                ops: vec![Op::Set { target, props }],
                readback: "Source binding checked by the host before publication.".into(),
            },
        });
    }
    Ok(Batch {
        base_revision: view.revision().into(),
        entries,
    })
}

pub fn project(view: &View, facts: &Facts) -> Result<Value, CapError> {
    let mut cards = BTreeMap::new();
    let mut readback = String::from("SOURCES\n");
    for n in view.records().filter(|r| r.kind == "node") {
        check_binding(n)?;
        let extras = facts.extras.get(&n.id).map(Vec::as_slice).unwrap_or(&[]);
        let bindings = effective_bindings(n, extras);
        let proposed = view.records().any(|r| {
            r.kind == "edge" && string(r, "label").trim() == "replaces" && string(r, "from") == n.id
        });
        let struck = view.records().any(|r| {
            r.kind == "edge" && string(r, "label").trim() == "replaces" && string(r, "to") == n.id
        });
        let gate = facts.gates.get(&n.id);
        let reads: Vec<_> = bindings
            .iter()
            .map(|b| facts.bindings.get(&binding_key(&n.id, b)))
            .collect();
        let readable = reads.iter().all(|f| {
            f.is_some_and(|f| {
                f.readable && f.current_sha256.is_some() && f.baseline_sha256.is_some()
            })
        });
        let drifted = reads
            .iter()
            .any(|f| f.is_some_and(|f| f.current_sha256 != f.baseline_sha256));
        let tier = if proposed {
            "proposed"
        } else if let Some(g) = gate {
            if g.met {
                "gate-met"
            } else {
                "gate-unmet"
            }
        } else if bindings.is_empty() || !readable {
            "claimed"
        } else if drifted {
            "drifted"
        } else {
            "verified"
        };
        let prefix = if tier == "verified" && bindings.len() == 1 {
            reads[0]
                .and_then(|f| f.baseline_sha256.as_ref())
                .map(|h| h.chars().take(6).collect::<String>())
        } else {
            None
        };
        let chip = match tier {
            "verified" => format!(
                "VERIFIED {}",
                prefix
                    .clone()
                    .unwrap_or_else(|| format!("{} lanes", bindings.len()))
            ),
            "drifted" => "DRIFTED".into(),
            "gate-met" => "GATE MET".into(),
            "gate-unmet" => match gate.map(|g| g.blocker.as_str()) {
                Some("unratified") => "GATE UNMET: UNRATIFIED".into(),
                Some("unattested") => "GATE UNMET: UNATTESTED".into(),
                _ => "GATE UNMET".into(),
            },
            "proposed" => "PROPOSED".into(),
            _ => "CLAIMED".into(),
        };
        let symbol = match tier {
            "verified" | "gate-met" => "✓",
            "drifted" => "△",
            "proposed" => "↪",
            "gate-unmet" => "!",
            _ => "?",
        };
        readback.push_str(&format!(
            "[{}] {:?}: {chip}; {} source binding(s){}\n",
            n.id,
            string(n, "label"),
            bindings.len(),
            match gate {
                Some(g) if tier == "gate-unmet" && !g.reason.is_empty() => format!(" ({})", g.reason),
                _ => String::new(),
            }
        ));
        cards.insert(n.id.clone(),json!({"tier":tier,"chip":chip,"symbol":symbol,"sha256_prefix":prefix,"struck":struck,"bindings":bindings,"gate":gate}));
    }
    Ok(
        json!({"revision":view.revision(),"cards":cards,"readback":readback,"evidence_boundary":"Readable source does not verify a claim's prose or a diagram's arrows."}),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Excerpt {
    #[serde(default)]
    pub entry_points: Vec<String>,
    #[serde(default)]
    pub entry_points_error: Option<String>,
    pub path: String,
    pub lines: String,
    pub first_line: usize,
    pub total_lines: usize,
    pub text: String,
    pub truncated: bool,
    pub revision: String,
}
pub fn viewer(excerpt: &Excerpt) -> Result<Value, CapError> {
    let (_, end) = range(&excerpt.lines)?;
    if excerpt.first_line == 0 || end > excerpt.total_lines || excerpt.text.contains('\0') {
        return Err(invalid("invalid excerpt facts"));
    }
    Ok(
        json!({"location":format!("{}:{}",excerpt.path,excerpt.lines),"revision":excerpt.revision,"lines":excerpt.text.lines().enumerate().map(|(i,text)|json!({"number":excerpt.first_line+i,"text":text,"highlighted":true})).collect::<Vec<_>>(),"readback":format!("SOURCE {}:{}{}\n{}",excerpt.path,excerpt.lines,if excerpt.revision.is_empty(){String::new()}else{format!("@{}",excerpt.revision)},excerpt.text)}),
    )
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Sources, Sources);
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn project_with_facts(view_json: &str, facts_json: &str) -> String {
    let result = (|| {
        let v: View = serde_json::from_str(view_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        let f: Facts = serde_json::from_str(facts_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))?;
        project(&v, &f)
    })();
    envelope(result)
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn viewer_readback(excerpt_json: &str) -> String {
    envelope(
        serde_json::from_str::<Excerpt>(excerpt_json)
            .map_err(|e| CapError::new("bad_input", e.to_string()))
            .and_then(|e| viewer(&e)),
    )
}
fn envelope(result: Result<Value, CapError>) -> String {
    match result {
        Ok(v) => json!({"abi":samepage_capability::ABI,"ok":v}).to_string(),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}).to_string(),
    }
}
