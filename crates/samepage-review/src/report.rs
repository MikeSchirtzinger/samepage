//! Portable report construction from native, explicitly supplied evidence.
use crate::{describe, flag, projection, string};
use samepage_capability::{CapError, View};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const EVIDENCE_BOUNDARY: &str = "Readable source does not verify a claim's prose or a diagram's arrows. Commit-pinned claims describe their cited revision; freshness against current code has not been established. Connections are authored relations, not detected runtime calls.\n";

#[derive(Default, Serialize, Deserialize)]
pub struct Facts {
    pub repository: String,
    /// Record id -> checked current or commit-pinned references.
    pub sources: BTreeMap<String, Vec<Value>>,
    pub project: Value,
    /// Native adapters supply these when their capabilities are selected.
    pub architecture: Option<Value>,
    pub tiers: BTreeMap<String, Value>,
}

pub fn build(view: &View, facts: &Facts) -> Result<Value, CapError> {
    let mut report = projection(view)?;
    report["schema_version"] = json!(1);
    report["document_revision"] = json!(view.revision());
    report["repository"] = json!(facts.repository);
    report["evidence_boundary"] = json!(EVIDENCE_BOUNDARY);
    report["project_facts"] = facts.project.clone();
    report["architecture"] = json!(facts.architecture);
    report["readback"] = json!(describe(view)?);
    let nodes = report["nodes"]
        .as_array_mut()
        .ok_or_else(|| crate::invalid("report nodes missing"))?;
    for node in nodes {
        let id = node["id"].as_str().unwrap_or_default().to_string();
        node["evidence"] = json!(facts.sources.get(&id).cloned().unwrap_or_default());
        node["tier"] = json!(facts.tiers.get(&id));
        node["claims"] = json!(
            crate::claims_of(view, &id)
                .iter()
                .filter(|r| !flag(r, "withdrawn"))
                .map(|r| r.id.clone())
                .collect::<Vec<_>>()
        );
        node["incoming"] = json!(
            view.records()
                .filter(|r| r.kind == "edge" && string(r, "to") == id)
                .map(|r| string(r, "from"))
                .collect::<Vec<_>>()
        );
        node["outgoing"] = json!(
            view.records()
                .filter(|r| r.kind == "edge" && string(r, "from") == id)
                .map(|r| string(r, "to"))
                .collect::<Vec<_>>()
        );
    }
    Ok(report)
}
