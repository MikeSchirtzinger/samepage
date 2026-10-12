//! Reviewed decisions and deterministic obligations. Native I/O stays in the host.
pub mod decision;
use decision::{Draft, DraftInput};
use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CallerKind, CapError, Capability,
    Change, Delta, Descriptor, Effect, LegendEntry, Op, Plan, Record, Target, Trigger, View,
};
use samepage_sources::string;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub const ID: &str = "cement";
pub const VERSION: &str = "0.1.0";
pub const MODES: &[&str] = &["learning", "alignment", "explain", "map", "cement"];
fn err(s: impl Into<String>) -> CapError {
    CapError::new("cement_invalid", s)
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn descriptor() -> Descriptor {
    let mut d = Descriptor::new(ID, VERSION, Artifact::Wasm)
        .depends_on(
            "sources",
            samepage_sources::VERSION,
            "Verified bindings and source facts",
        )
        .depends_on(
            "lanes",
            samepage_lanes::VERSION,
            "Extracted lanes and removal obligations",
        )
        .depends_on(
            "review",
            samepage_review::VERSION,
            "Human verdicts and shared context",
        )
        .extends("node", &["cemented"], "Reviewed publication coverage")
        .owns(
            decision::KIND,
            &["definition"],
            "Immutable agent decision drafts",
        )
        .owns("cement-mode", &["value"], "Shared commitment posture");
    for kind in [
        "node",
        "edge",
        "claim",
        "mark",
        "binding",
        "understanding-context",
    ] {
        d = d.reads(kind);
    }
    let ids = json!({"type":"array","minItems":1,"items":{"type":"string"},"description":"Card ids, as page_read prints them."});
    let draft_id = json!({"type":"string","description":"The draft id from decision_draft (its reply's ids.draft)."});
    for (name, alias, audience, effect, required, properties, description) in [
        (
            "mode_read",
            "atlas_map_mode_get",
            Audience::Both,
            Effect::Query,
            vec![],
            json!({}),
            "Read the shared commitment mode (learning, alignment, explain, map or cement), the decision drafts and which cards are cemented. The mode is whatever a person or agent last set.",
        ),
        (
            "mode_set",
            "atlas_map_mode_set",
            Audience::Both,
            Effect::Mutate,
            vec!["mode"],
            json!({"mode":{"enum":MODES}}),
            "Set the shared commitment mode for everyone on the page: learning, alignment, explain, map or cement. Switching never deletes or changes a card.",
        ),
        (
            "map_cement",
            "atlas_map_cement",
            Audience::Agent,
            Effect::Query,
            vec![],
            json!({"ids":ids.clone()}),
            "Export agreed cards to G8 obligations in specs/obligations-v0.1.json at the project root, one obligation per card. Pass ids to cement the agreed subset. Each card must be agreed by a person, its claims accepted and its question marks answered; the refusal lists every reason. New obligations start unratified and unattested, so each card reads GATE UNMET until a person runs g8 ratify and g8 attest.",
        ),
        (
            "lane_remove",
            "atlas_remove_lane",
            Audience::Both,
            Effect::Query,
            vec!["lane_id"],
            json!({"lane_id":{"type":"string","description":"A lane id from lanes_read."},"reason":{"type":"string","description":"Why this code should not exist; stored as the obligation's note."}}),
            "Write a G8 removal obligation for a lane the source scan found, instead of claiming it: the code at that lane must stop existing. The lane reads REMOVE unmet until the code is deleted and g8 confirms it.",
        ),
        (
            "decision_draft",
            "atlas_decision_draft",
            Audience::Agent,
            Effect::Mutate,
            vec!["draft"],
            json!({"draft":draft_schema()}),
            "Record an immutable decision draft for a person to review. Set the shared question with context_set first. draft.ids are the cards it covers. checks are explicit G8 structural checks (rust_enum_shape or cargo_metadata_no_dep) with the files a person reviews as evidence; they constrain structure, not behaviour, so list what they leave unchecked in not_enforced. Next, decision_cement_propose with the reply's ids.draft. Only a person publishes a decision.",
        ),
        (
            "decision_preview",
            "atlas_decision_preview",
            Audience::Human,
            Effect::Query,
            vec!["ids", "draft_id"],
            json!({"ids":ids.clone(),"draft_id":draft_id.clone()}),
            "Preview the decision record and checks a draft would publish, with the review token publication needs. Writes nothing.",
        ),
        (
            "decision_list",
            "atlas_decisions_read",
            Audience::Human,
            Effect::Query,
            vec![],
            json!({}),
            "Read the published decision records under docs/decisions.",
        ),
        (
            "decision_cement_propose",
            "atlas_cement_decision_propose",
            Audience::Agent,
            Effect::Query,
            vec!["ids", "draft_id"],
            json!({"ids":ids.clone(),"draft_id":draft_id.clone()}),
            "Check whether a decision draft could be published, without writing. Pass the draft id and the same card ids. The reply lists every blocking reason at once: cards not agreed, claims not accepted, unanswered marks, cards or context changed since the draft, unreadable evidence files. Only a person can publish.",
        ),
        (
            "decision_cement",
            "atlas_cement_decision",
            Audience::Human,
            Effect::Mutate,
            vec!["ids", "draft_id", "review_token"],
            json!({"ids":ids.clone(),"draft_id":draft_id.clone(),"review_token":{"type":"string"}}),
            "Publish a reviewed decision: write its record and obligations and stamp the covered cards. Only a person can, with the review token from the preview.",
        ),
    ] {
        let mut a = ActionDecl::new(name, audience, effect, description).alias(alias);
        a.input_schema = json!({"type":"object","required":required,"properties":properties,"additionalProperties":false});
        d = d
            .action(a)
            .trigger(Trigger::tool(name))
            .trigger(Trigger::tool(alias));
    }
    d.trigger(Trigger::kind(decision::KIND))
        .trigger(Trigger::property("node", "cemented"))
        .trigger(Trigger::route("cement"))
}
/// The typed argument contract of `decision_draft`, matching [`DraftInput`].
fn draft_schema() -> Value {
    let text = |what: &str| json!({"type":"string","minLength":1,"maxLength":8000,"description":what});
    let strings = |min: u64| json!({"type":"array","minItems":min,"items":{"type":"string","minLength":1}});
    let enum_args = json!({
        "type":"object","additionalProperties":false,"required":["file","enum_name","expected_variants"],
        "properties":{
            "file":{"type":"string","description":"Repository-relative Rust file, also listed in evidence_files."},
            "enum_name":{"type":"string","minLength":1},
            "expected_variants":strings(1),
            "expected_serde_rename_all":{"type":["string","null"]}
        }
    });
    let denied = json!({
        "type":"object","additionalProperties":false,"required":["kind","value"],
        "properties":{"kind":{"const":"exact"},"value":{"type":"string","minLength":1}}
    });
    let build_config = json!({"anyOf":[
        {"const":"default"},
        {"type":"object","additionalProperties":false,"required":["features"],"properties":{"features":strings(0)}}
    ]});
    let dependency_args = json!({
        "type":"object","additionalProperties":false,"required":["denied","scope_crates","build_config"],
        "properties":{
            "denied":{"type":"array","minItems":1,"items":denied},
            "scope_crates":{"type":"array","minItems":1,"items":{"type":"string"},"description":"Package names the rule applies to."},
            "build_config":build_config
        }
    });
    let check = json!({
        "type":"object","additionalProperties":false,"required":["backend","args"],
        "properties":{"backend":{"enum":["rust_enum_shape","cargo_metadata_no_dep"]},"args":{"type":"object"}},
        "allOf":[
            {"if":{"properties":{"backend":{"const":"rust_enum_shape"}}},"then":{"properties":{"args":enum_args}}},
            {"if":{"properties":{"backend":{"const":"cargo_metadata_no_dep"}}},"then":{"properties":{"args":dependency_args}}}
        ]
    });
    let item = json!({
        "type":"object","additionalProperties":false,"required":["id","requirement","check","evidence_files"],
        "properties":{
            "id":{"type":"string","pattern":"^[A-Z0-9-]+$","description":"A distinct uppercase id such as NO-REQWEST-1."},
            "requirement":{"type":"string","minLength":1,"description":"The requirement in words."},
            "check":check,
            "evidence_files":{"type":"array","minItems":1,"maxItems":32,"uniqueItems":true,"items":{"type":"string"},"description":"Repository-relative files a person reviews; g8 invalidates the attestation when they change."}
        }
    });
    let alternative = json!({
        "type":"object","additionalProperties":false,"required":["option","reason_not_chosen"],
        "properties":{"option":{"type":"string","minLength":1},"reason_not_chosen":{"type":"string","minLength":1}}
    });
    json!({
        "type":"object",
        "additionalProperties":false,
        "required":["ids","title","decision","rationale","alternatives","tradeoffs","consequences","checks"],
        "properties":{
            "ids":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"string"},"description":"The card ids this decision covers."},
            "title":text("A short title for the decision record."),
            "decision":text("What was decided, in one or two sentences."),
            "rationale":text("Why this approach."),
            "alternatives":{"type":"array","minItems":1,"maxItems":20,"items":alternative},
            "tradeoffs":text("What this costs."),
            "consequences":text("What follows from it."),
            "checks":{"type":"array","minItems":1,"maxItems":32,"items":item},
            "not_enforced":{"type":"array","maxItems":64,"items":{"type":"string"},"description":"Requirements these checks do not enforce."}
        }
    })
}
pub fn content(v: &View, ids: &[String]) -> Result<String, CapError> {
    let selected: BTreeSet<_> = ids.iter().collect();
    if selected.is_empty() || selected.len() != ids.len() {
        return Err(err("cement needs a nonempty selection of distinct nodes"));
    }
    let mut nodes = Vec::new();
    for id in &selected {
        let n = v
            .get(id)
            .filter(|n| n.kind == "node")
            .ok_or_else(|| err(format!("no node {id}")))?;
        let claims: Vec<_> = samepage_review::claims_of(v, id)
            .into_iter()
            .filter(|c| !samepage_review::flag(c, "withdrawn"))
            .map(|c| {
                json!([
                    c.id,
                    string(c, "text"),
                    string(c, "basis"),
                    string(c, "path"),
                    string(c, "lines"),
                    string(c, "revision"),
                    string(c, "verdict")
                ])
            })
            .collect();
        let bindings: Vec<_> = v
            .records()
            .filter(|r| r.kind == "binding" && string(r, "node") == id.as_str())
            .map(|r| {
                json!([
                    r.id,
                    string(r, "path"),
                    string(r, "lines"),
                    string(r, "baseline_sha256")
                ])
            })
            .collect();
        nodes.push(json!({"id":n.id,"label":string(n,"label"),"path":string(n,"path"),"lines":string(n,"lines"),"note":string(n,"note"),"tone":string(n,"tone"),"status":string(n,"status"),"claims":claims,"bindings":bindings}));
    }
    let edges: Vec<_> = v
        .records()
        .filter(|r| {
            r.kind == "edge"
                && selected.iter().any(|i| i.as_str() == string(r, "from"))
                && selected.iter().any(|i| i.as_str() == string(r, "to"))
        })
        .map(|r| {
            json!([
                r.id,
                string(r, "from"),
                string(r, "to"),
                string(r, "label"),
                string(r, "event"),
                string(r, "guard")
            ])
        })
        .collect();
    Ok(hash(
        &serde_json::to_vec(&(nodes, edges)).map_err(|e| err(e.to_string()))?,
    ))
}
pub fn settled(v: &View, ids: &[String]) -> Result<(), CapError> {
    content(v, ids)?;
    let mut refusals = Vec::new();
    for id in ids {
        let n = v.get(id).ok_or_else(|| err("missing node"))?;
        if string(n, "status") != "agreed" {
            refusals.push(format!("{} ({id}): not agreed", string(n, "label")));
        }
        let cs: Vec<_> = samepage_review::claims_of(v, id)
            .into_iter()
            .filter(|c| !samepage_review::flag(c, "withdrawn"))
            .collect();
        if cs.is_empty() {
            refusals.push(format!("{id}: no settled claims"));
        }
        for c in cs {
            if string(c, "verdict") != "accepted" || string(c, "basis") == "unknown" {
                refusals.push(format!(
                    "claim {}: {} ({})",
                    c.id,
                    string(c, "verdict"),
                    string(c, "basis")
                ));
            }
        }
    }
    for m in v.records().filter(|r| {
        r.kind == "mark"
            && matches!(string(r, "glyph"), "?" | "!")
            && string(r, "answer").trim().is_empty()
    }) {
        let target = string(m, "target");
        if ids.iter().any(|id| id == target)
            || v.get(target).is_some_and(|r| {
                r.kind == "claim" && ids.iter().any(|id| id == string(r, "about"))
                    || r.kind == "edge"
                        && ids.iter().any(|id| id == string(r, "from"))
                        && ids.iter().any(|id| id == string(r, "to"))
            })
        {
            refusals.push(format!("mark {}: unresolved {}", m.id, string(m, "text")));
        }
    }
    if refusals.is_empty() {
        Ok(())
    } else {
        Err(err(format!("cement refused:\n{}", refusals.join("\n"))))
    }
}
pub fn draft(v: &View, id: &str) -> Result<Draft, CapError> {
    let r = v
        .get(id)
        .filter(|r| r.kind == decision::KIND)
        .ok_or_else(|| err("decision draft not found"))?;
    let mut d: Draft =
        serde_json::from_str(string(r, "definition")).map_err(|e| err(e.to_string()))?;
    d.input.validate().map_err(err)?;
    d.id = id.into();
    d.created_by = string(r, "created_by").into();
    Ok(d)
}
pub fn reviewed(v: &View, id: &str, ids: &[String]) -> Result<Draft, CapError> {
    let d = draft(v, id)?;
    let mut expected = d.input.ids.clone();
    expected.sort();
    let mut actual = ids.to_vec();
    actual.sort();
    if actual != expected
        || d.content != content(v, ids)?
        || d.context_ids != samepage_review::context::project(v)?.heads
    {
        return Err(err(
            "page or shared scope changed after this decision draft; review a new draft",
        ));
    }
    settled(v, ids)?;
    Ok(d)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Coverage {
    pub ids: Vec<String>,
    pub content: String,
    pub g8_status: String,
    pub obligation_ids: Vec<String>,
}
pub fn covers(v: &View, node: &Record) -> bool {
    string(node, "cemented")
        .splitn(3, '\n')
        .nth(2)
        .and_then(|s| serde_json::from_str::<Coverage>(s).ok())
        .is_some_and(|c| {
            c.ids.contains(&node.id) && content(v, &c.ids).is_ok_and(|h| h == c.content)
        })
}
pub fn mode(v: &View) -> &str {
    v.records()
        .find(|r| r.kind == "cement-mode")
        .map(|r| string(r, "value"))
        .unwrap_or("learning")
}
pub struct Cement;
impl Capability for Cement {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, v: &View) -> Result<Value, CapError> {
        Ok(
            json!({"mode":mode(v),"drafts":v.records().filter(|r|r.kind==decision::KIND).map(|r|draft(v,&r.id)).collect::<Result<Vec<_>,_>>()?,"coverage":v.records().filter(|r|r.kind=="node"&&!string(r,"cemented").is_empty()).map(|n|json!({"id":n.id,"current":covers(v,n),"record":string(n,"cemented")})).collect::<Vec<_>>()}),
        )
    }
    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok([
            (
                "tier",
                "gate-met",
                "Current gate passes",
                "source-tier source-tier-gate-met",
                "✓ GATE MET",
            ),
            (
                "tier",
                "gate-unmet",
                "Current gate fails",
                "source-tier source-tier-gate-unmet",
                "! GATE UNMET",
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
    fn plan(&self, c: &Call, v: &View) -> Result<Plan, CapError> {
        let declaration = descriptor()
            .actions
            .into_iter()
            .find(|a| a.name == c.action)
            .ok_or_else(|| CapError::new("unknown_action", &c.action))?;
        let args = c
            .args
            .as_object()
            .ok_or_else(|| err("arguments must be an object"))?;
        for key in args.keys() {
            if declaration.input_schema["properties"].get(key).is_none() {
                return Err(err(format!("unknown argument {key}")));
            }
        }
        for key in declaration.input_schema["required"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(Value::as_str)
        {
            if !args.contains_key(key) {
                return Err(err(format!("missing argument {key}")));
            }
        }

        let mut ops = Vec::new();
        let readback = match c.action.as_str() {
            "mode_set" => {
                let m = c.args["mode"]
                    .as_str()
                    .ok_or_else(|| err("mode must be text"))?;
                if !MODES.contains(&m) {
                    return Err(err("unsupported mode"));
                }
                let props = [("value".into(), m.into())].into();
                ops.push(
                    if let Some(r) = v.records().find(|r| r.kind == "cement-mode") {
                        Op::Set {
                            target: Target::Id(r.id.clone()),
                            props,
                        }
                    } else {
                        Op::Create {
                            reference: "mode".into(),
                            kind: "cement-mode".into(),
                            props,
                        }
                    },
                );
                format!("Mode: {m}")
            }
            "decision_draft" => {
                let input: DraftInput = serde_json::from_value(c.args["draft"].clone())
                    .map_err(|e| err(e.to_string()))?;
                input.validate().map_err(err)?;
                let ctx = samepage_review::context::project(v)?;
                if ctx.current.is_none() {
                    return Err(err(
                        "record or reconcile the shared question before drafting",
                    ));
                }
                if v.records().filter(|r| r.kind == decision::KIND).count() >= 128 {
                    return Err(err("decision draft history is full"));
                }
                let d = Draft {
                    id: String::new(),
                    content: content(v, &input.ids)?,
                    input,
                    context_ids: ctx.heads,
                    created_by: String::new(),
                };
                ops.push(Op::Create {
                    reference: "draft".into(),
                    kind: decision::KIND.into(),
                    props: [(
                        "definition".into(),
                        serde_json::to_string(&d)
                            .map_err(|e| err(e.to_string()))?
                            .into(),
                    )]
                    .into(),
                });
                "Decision draft recorded; publication requires human review.".into()
            }
            "decision_cement" => {
                return Err(CapError::new(
                    "publication_required",
                    "native reviewed publication must precede the CRDT stamp",
                ))
            }
            "mode_read"
            | "map_cement"
            | "lane_remove"
            | "decision_preview"
            | "decision_list"
            | "decision_cement_propose" => format!(
                "CEMENT: mode {}; native repository adapter supplies publication and gate facts.",
                mode(v)
            ),
            _ => return Err(CapError::new("unknown_action", &c.action)),
        };
        Ok(Plan { ops, readback })
    }
    fn validate(&self, _: &Delta, v: &View) -> Result<(), CapError> {
        let mut modes = 0;
        for r in v.records() {
            if r.kind == decision::KIND {
                draft(v, &r.id)?;
            }
            if r.kind == "cement-mode" {
                modes += 1;
                if !MODES.contains(&string(r, "value")) {
                    return Err(err("unsupported mode"));
                }
            }
        }
        if modes > 1 {
            return Err(err("only one commitment mode is allowed"));
        }
        Ok(())
    }
    fn validate_change(&self, c: &Change<'_>) -> Result<(), CapError> {
        self.validate(c.delta, c.after)?;
        for (id, keys) in c.attempted {
            if c.before.get(id).is_some_and(|r| r.kind == decision::KIND) {
                return Err(err("decision draft history is append-only"));
            }

            let Some(r) = c.after.get(id) else { continue };
            if r.kind == decision::KIND
                && (c.principal.caller != CallerKind::Agent || c.before.contains(id))
            {
                return Err(err("decision drafts are immutable agent history"));
            }
            if r.kind == decision::KIND {
                let d = draft(c.after, id)?;
                if d.content != content(c.before, &d.input.ids)?
                    || d.context_ids != samepage_review::context::project(c.before)?.heads
                {
                    return Err(err(
                        "draft must bind the current selection and shared context",
                    ));
                }
            }
            if r.kind == "node"
                && keys.contains("cemented")
                && (c.principal.caller != CallerKind::Human
                    || c.facts.and_then(|f| f.get(id)).and_then(Value::as_str)
                        != Some(string(r, "cemented")))
            {
                return Err(CapError::new(
                    "publication_required",
                    "only a human with matching native publication can stamp cement",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleFacts {
    pub repository: String,
    pub at: String,
    pub number: u32,
    pub spec_hash: Option<String>,
    pub index_hash: Option<String>,
    pub evidence: BTreeMap<String, Option<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub draft: Draft,
    pub revision: String,
    pub adr_path: String,
    pub fragment_path: String,
    pub adr: String,
    pub fragment: Value,
    pub receipt: Value,
    pub review_token: String,
}
pub fn bundle(
    v: &View,
    ids: &[String],
    draft_id: &str,
    f: &BundleFacts,
) -> Result<Bundle, CapError> {
    let d = reviewed(v, draft_id, ids)?;
    if f.at.trim().is_empty() || f.repository.is_empty() {
        return Err(err("bundle needs native time and repository"));
    }
    let slug: String = d
        .input
        .title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "decision" } else { slug };
    let adr_path = format!("docs/decisions/{:04}-{slug}.md", f.number);
    let fragment_path = format!("docs/decisions/{:04}-{slug}.json", f.number);
    let ctx = samepage_review::context::project(v)?
        .current
        .ok_or_else(|| err("one shared question is required"))?;
    let input = &d.input;
    let mut adr=format!("# {}\n\nStatus: accepted by the human when cemented.\nDate: {}\n\n## Context\n\nQuestion: {}\nScope: {}\nSubjects: {}\nAssumptions: {}\n\n## Decision\n\n{}\n\n## Why this approach\n\n{}\n\n## Alternatives considered\n\n",input.title,f.at,ctx.question,ctx.altitude.label(),ctx.subjects.join(", "),ctx.assumptions,input.decision,input.rationale);
    for a in &input.alternatives {
        adr.push_str(&format!("- {}: {}\n", a.option, a.reason_not_chosen));
    }
    adr.push_str(&format!(
        "\n## Tradeoffs\n\n{}\n\n## Consequences\n\n{}\n\n## Structural checks\n\n",
        input.tradeoffs, input.consequences
    ));
    for c in &input.checks {
        adr.push_str(&format!("- {}: {}\n", c.id, c.requirement));
    }
    adr.push_str("\nStructural checks do not prove behavioral correctness or comprehension. Changed evidence requires renewed review.\n\n## Not enforced\n\n");
    for n in &input.not_enforced {
        adr.push_str(&format!("- {n}\n"));
    }
    let obligations:Vec<_>=input.checks.iter().map(|c|json!({"id":c.id,"descends_from":{"decision":adr_path,"draft_id":d.id,"context_ids":d.context_ids},"rule":{"kind":"reviewed_structure","description":c.requirement},"checker":{"mode":"typed","checks":[c.check]},"signal":{"gate":"cemented_intent","wiring":"repository_artifact","advisory":false}})).collect();
    let fragment = json!({"meta":{"artifact":"samepage-reviewed-obligations","authority":adr_path},"obligations":obligations,"conflicts":[],"open_questions":input.not_enforced});
    let review = json!({"draft":d,"spec_sha256":f.spec_hash,"evidence":f.evidence,"adr_path":adr_path,"index_sha256":f.index_hash,"document_revision":v.revision(),"cemented_at":f.at,"adr_sha256":hash(adr.as_bytes()),"fragment_sha256":hash(&serde_json::to_vec_pretty(&fragment).map_err(|e|err(e.to_string()))?)});
    let review_token = hash(&serde_json::to_vec_pretty(&review).map_err(|e| err(e.to_string()))?);
    let receipt = json!({"schema_version":1,"kind":"samepage-cement-receipt","review":review,"review_token":review_token,"repository":f.repository,"approval":{"audience":"human","mechanism":"human-only preview and publication; token proves freshness, not authority"},"g8_status":"pending adoption"});
    Ok(Bundle {
        draft: d,
        revision: v.revision().into(),
        adr_path,
        fragment_path,
        adr,
        fragment,
        receipt,
        review_token,
    })
}
pub fn stamp(v: &View, b: &Bundle, status: &str) -> Result<Batch, CapError> {
    if !["active_passing", "active_blocked"].contains(&status) {
        return Err(err("invalid G8 adoption status"));
    }
    let d = reviewed(v, &b.draft.id, &b.draft.input.ids)?;
    if v.revision() != b.revision || d.content != b.draft.content {
        return Err(err(
            "selected decision changed while cement was publishing; review it again",
        ));
    }
    let coverage = Coverage {
        ids: d.input.ids.clone(),
        content: d.content,
        g8_status: status.into(),
        obligation_ids: d.input.checks.iter().map(|c| c.id.clone()).collect(),
    };
    let encoded = format!(
        "{}\n{}\n{}",
        b.adr_path,
        b.revision,
        serde_json::to_string(&coverage).map_err(|e| err(e.to_string()))?
    );
    Ok(Batch {
        base_revision: v.revision().into(),
        entries: vec![BatchEntry {
            capability: ID.into(),
            plan: Plan {
                ops: coverage
                    .ids
                    .iter()
                    .map(|id| Op::Set {
                        target: Target::Id(id.clone()),
                        props: [("cemented".into(), encoded.clone().into())].into(),
                    })
                    .collect(),
                readback: format!("Reviewed decision published: {}; G8 {status}.", b.adr_path),
            },
        }],
    })
}
pub fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
pub fn removal(l: &samepage_lanes::Lane, reason: &str) -> Result<Value, CapError> {
    let pattern = regex_escape(l.evidence.snippet.trim());
    if pattern.is_empty() {
        return Err(err("lane has no checkable snippet"));
    }
    Ok(
        json!({"id":format!("ATLAS-REMOVE-{}",&hash(l.lane_id.as_bytes())[..8]),"lane_id":l.lane_id,"note":reason,"checker":{"mode":"typed","checks":[{"backend":"rg_match_count","args":{"pattern":pattern,"glob":[l.evidence.path],"expected":{"kind":"exactly","value":0}}}]},"signal":{"gate":"atlas_map_conformance","wiring":"repository_artifact","advisory":false}}),
    )
}
pub fn merge(existing: Value, entries: Vec<Value>) -> Result<Value, CapError> {
    let mut out = existing;
    if out.is_null() {
        out = json!({"meta":{"note":"Unratified and unattested obligations start red."},"obligations":[]});
    }
    let mut by_id = BTreeMap::new();
    for entry in out["obligations"]
        .as_array()
        .ok_or_else(|| err("obligations artifact has no array"))?
        .iter()
        .chain(entries.iter())
    {
        let id = entry["id"]
            .as_str()
            .ok_or_else(|| err("obligation needs id"))?;
        if id.is_empty() {
            return Err(err("empty obligation id"));
        }
        by_id.insert(id.to_string(), entry.clone());
    }
    out["obligations"] = json!(by_id.into_values().collect::<Vec<_>>());
    Ok(out)
}
pub fn advisory(assertions: &[Value], revision: &str, at: &str) -> Result<Value, CapError> {
    let refusals: Vec<_> = assertions
        .iter()
        .filter(|a| {
            a["status"] != "settled"
                || a["signed_by"] != "human"
                || a["authored_by"].as_str().is_none_or(str::is_empty)
                || a["signed_revision"].as_u64().is_none_or(|n| {
                    n == 0
                        || a["authored_revision"]
                            .as_u64()
                            .is_none_or(|old| old == 0 || old > n)
                })
                || a["human_mark"] != "agree"
                || a["signed_revision"] != a["changed_revision"]
                || a["authored_at"].as_str().is_none_or(str::is_empty)
                || a["signed_at"].as_str().is_none_or(str::is_empty)
        })
        .map(|a| format!("assertion {} is unsettled or disputed", a["id"]))
        .collect();
    if assertions.is_empty() || revision.is_empty() || at.is_empty() || !refusals.is_empty() {
        return Err(err(format!(
            "advisory cement refused: {}",
            refusals.join("; ")
        )));
    }
    Ok(
        json!({"receipt":{"revision":revision,"at":at,"assertions":assertions},"obligations":{"meta":{"advisory_only":true},"obligations":assertions.iter().map(|a|json!({"id":a["id"],"rule":a["text"],"checker":{"mode":"prose_only"},"signal":{"gate":"none","wiring":"unwired_draft","advisory":true}})).collect::<Vec<_>>()}}),
    )
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Cement, Cement);
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn bundle_with_facts(view_json: &str, args_json: &str, facts_json: &str) -> String {
    let result = (|| {
        let v: View = serde_json::from_str(view_json).map_err(|e| err(e.to_string()))?;
        let a: Value = serde_json::from_str(args_json).map_err(|e| err(e.to_string()))?;
        let f: BundleFacts = serde_json::from_str(facts_json).map_err(|e| err(e.to_string()))?;
        let ids: Vec<String> =
            serde_json::from_value(a["ids"].clone()).map_err(|e| err(e.to_string()))?;
        let b = bundle(&v, &ids, a["draft_id"].as_str().unwrap_or_default(), &f)?;
        serde_json::to_value(b).map_err(|e| err(e.to_string()))
    })();
    match result {
        Ok(v) => json!({"abi":samepage_capability::ABI,"ok":v}).to_string(),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}).to_string(),
    }
}
/// One deterministic anchor check per effective binding; source excerpts are raw host input.
pub fn map_obligations(
    v: &View,
    ids: &[String],
    facts: &samepage_sources::Facts,
    excerpts: &BTreeMap<String, String>,
) -> Result<Vec<Value>, CapError> {
    settled(v, ids)?;
    let projected = samepage_sources::project(v, facts)?;
    let mut out = Vec::new();
    for id in ids {
        let n = v.get(id).ok_or_else(|| err("missing node"))?;
        let card = &projected["cards"][id];
        let struck = card["struck"].as_bool().unwrap_or(false);
        let bindings = samepage_sources::effective_bindings(
            n,
            &facts.extras.get(id).cloned().unwrap_or_default(),
        );
        if bindings.is_empty() {
            return Err(err(format!("card {id}: no checkable bindings")));
        }
        let mut checks = Vec::new();
        for b in bindings {
            let text = excerpts
                .get(&samepage_sources::binding_key(id, &b))
                .ok_or_else(|| err(format!("card {id}: unreadable binding {}", b.path)))?;
            let anchor = text
                .lines()
                .map(str::trim)
                .find(|s| !s.is_empty())
                .ok_or_else(|| err(format!("card {id}: empty source")))?;
            checks.push(json!({"backend":"rg_match_count","args":{"pattern":regex_escape(anchor),"glob":[b.path],"expected":{"kind":if struck{"exactly"}else{"at_least"},"value":if struck{0}else{1}}}}));
        }
        out.push(json!({"id":format!("ATLAS-CARD-{}",&hash(id.as_bytes())[..8]),"node_id":id,"label":string(n,"label"),"checker":{"mode":"typed","checks":checks},"signal":{"gate":"atlas_map_conformance","wiring":"repository_artifact","advisory":false}}));
    }
    Ok(out)
}
#[cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-export"),
    wasm_bindgen::prelude::wasm_bindgen
)]
pub fn obligations_with_facts(view_json: &str, args_json: &str, facts_json: &str) -> String {
    let result = (|| {
        let v: View = serde_json::from_str(view_json).map_err(|e| err(e.to_string()))?;
        let a: Value = serde_json::from_str(args_json).map_err(|e| err(e.to_string()))?;
        let f: Value = serde_json::from_str(facts_json).map_err(|e| err(e.to_string()))?;
        match a["kind"].as_str() {
            Some("map") => {
                let ids: Vec<String> =
                    serde_json::from_value(a["ids"].clone()).map_err(|e| err(e.to_string()))?;
                let sources =
                    serde_json::from_value(f["sources"].clone()).map_err(|e| err(e.to_string()))?;
                let excerpts = serde_json::from_value(f["excerpts"].clone())
                    .map_err(|e| err(e.to_string()))?;
                Ok(json!(map_obligations(&v, &ids, &sources, &excerpts)?))
            }
            Some("removal") => {
                let lane =
                    serde_json::from_value(f["lane"].clone()).map_err(|e| err(e.to_string()))?;
                removal(&lane, a["reason"].as_str().unwrap_or_default())
            }
            Some("merge") => {
                let entries =
                    serde_json::from_value(a["entries"].clone()).map_err(|e| err(e.to_string()))?;
                merge(f["existing"].clone(), entries)
            }
            Some("advisory") => {
                let assertions: Vec<Value> = serde_json::from_value(f["assertions"].clone())
                    .map_err(|e| err(e.to_string()))?;
                advisory(
                    &assertions,
                    a["revision"].as_str().unwrap_or_default(),
                    a["at"].as_str().unwrap_or_default(),
                )
            }
            _ => Err(err("unknown obligation kind")),
        }
    })();
    match result {
        Ok(v) => json!({"abi":samepage_capability::ABI,"ok":v}).to_string(),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}).to_string(),
    }
}
