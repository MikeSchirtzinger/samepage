//! Shared relationship proposals. Each revision is a separate CRDT object so
//! concurrent alternatives survive. A proposal never changes the mapped graph.
use samepage_capability::{CapError, View};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::{invalid, proposals, string};

pub const KIND: &str = "architecture-proposal";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub id: String,
    pub label: String,
    pub path: String,
    pub parent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub components: Vec<Component>,
    pub relationships: Vec<Relationship>,
}

impl Graph {
    pub fn capture(view: &View) -> Self {
        let mut components: Vec<_> = view
            .records()
            .filter(|r| r.kind == "node")
            .map(|n| Component {
                id: n.id.clone(),
                label: string(n, "label").into(),
                path: string(n, "path").into(),
                parent: string(n, "parent").into(),
            })
            .collect();
        components.sort_by(|a, b| a.id.cmp(&b.id));
        let mut relationships: Vec<_> = view
            .records()
            .filter(|r| r.kind == "edge")
            .map(|e| Relationship {
                from: string(e, "from").into(),
                to: string(e, "to").into(),
                label: if string(e, "event").is_empty() {
                    string(e, "label").into()
                } else {
                    format!("{} [{}]", string(e, "event"), string(e, "guard"))
                },
            })
            .collect();
        relationships.sort();
        relationships.dedup();
        Self {
            components,
            relationships,
        }
    }

    pub fn revision(&self) -> Result<String, CapError> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|e| invalid(e.to_string()))?)
        ))
    }

    fn validate(&self) -> Result<(), CapError> {
        if self.components.iter().any(|c| c.id.is_empty())
            || !self.components.windows(2).all(|p| p[0].id < p[1].id)
            || !self.relationships.windows(2).all(|p| p[0] < p[1])
        {
            return Err(invalid(
                "comparison graphs must use canonical distinct component and relationship order",
            ));
        }
        let ids: BTreeSet<_> = self.components.iter().map(|c| &c.id).collect();
        if ids.len() != self.components.len() || self.components.len() > 512 {
            return Err(invalid(
                "comparison requires at most 512 distinct components",
            ));
        }
        if self.relationships.len() > 4096 {
            return Err(invalid("comparison exceeds 4096 relationships"));
        }
        let mut seen = BTreeSet::new();
        for edge in &self.relationships {
            if !ids.contains(&edge.from) || !ids.contains(&edge.to) {
                return Err(invalid(
                    "a proposed relationship references an unknown component",
                ));
            }
            if edge.label.chars().count() > 500 || !seen.insert(edge) {
                return Err(invalid(
                    "relationship labels must fit 500 characters and relationships must be distinct",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProposalInput {
    pub title: String,
    /// Exact semantic graph revision from architecture read-back. Layout does not count.
    pub baseline_revision: String,
    #[serde(default)]
    pub previous: String,
    #[serde(default)]
    pub remove: Vec<Relationship>,
    #[serde(default)]
    pub add: Vec<Relationship>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub title: String,
    pub previous: String,
    pub baseline: Graph,
    pub proposed: Graph,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proposal {
    pub id: String,
    pub created_by: String,
    pub definition: Definition,
    pub verdict: String,
    pub decided_by: String,
}

pub fn project(id: &str, definition: &str, created_by: String) -> Result<Proposal, CapError> {
    let definition: Definition = serde_json::from_str(definition)
        .map_err(|e| invalid(format!("invalid architecture proposal: {e}")))?;
    validate(&definition)?;
    Ok(Proposal {
        id: id.into(),
        created_by,
        definition,
        verdict: "open".into(),
        decided_by: String::new(),
    })
}

pub(crate) fn validate(definition: &Definition) -> Result<(), CapError> {
    if definition.title.trim().is_empty() || definition.title.chars().count() > 160 {
        return Err(invalid("a proposal title must contain 1 to 160 characters"));
    }
    definition.baseline.validate()?;
    definition.proposed.validate()?;
    if definition.baseline.components != definition.proposed.components {
        return Err(invalid(
            "relationship proposals preserve the captured components",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Impact {
    pub id: String,
    /// One shortest incoming-arrow path to a directly affected component.
    pub via: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Comparison {
    pub proposal: Proposal,
    pub baseline_revision: String,
    pub baseline_changed: bool,
    pub added: Vec<Relationship>,
    pub removed: Vec<Relationship>,
    pub direct: Vec<String>,
    pub upstream: Vec<Impact>,
}

pub fn compare(proposal: &Proposal, current: &Graph) -> Result<Comparison, CapError> {
    let before: BTreeSet<_> = proposal
        .definition
        .baseline
        .relationships
        .iter()
        .cloned()
        .collect();
    let after: BTreeSet<_> = proposal
        .definition
        .proposed
        .relationships
        .iter()
        .cloned()
        .collect();
    let added: Vec<_> = after.difference(&before).cloned().collect();
    let removed: Vec<_> = before.difference(&after).cloned().collect();
    let direct: BTreeSet<_> = added
        .iter()
        .chain(&removed)
        .flat_map(|e| [e.from.clone(), e.to.clone()])
        .collect();
    let union: Vec<_> = before.union(&after).collect();
    let mut paths: BTreeMap<String, Vec<String>> = direct
        .iter()
        .map(|id| (id.clone(), vec![id.clone()]))
        .collect();
    let mut queue: std::collections::VecDeque<_> = direct.iter().cloned().collect();
    while let Some(target) = queue.pop_front() {
        for edge in union.iter().filter(|e| e.to == target) {
            if !paths.contains_key(&edge.from) {
                let mut route = vec![edge.from.clone()];
                route.extend(paths[&target].clone());
                paths.insert(edge.from.clone(), route);
                queue.push_back(edge.from.clone());
            }
        }
    }
    Ok(Comparison {
        proposal: proposal.clone(),
        baseline_revision: proposal.definition.baseline.revision()?,
        baseline_changed: &proposal.definition.baseline != current,
        added,
        removed,
        direct: direct.iter().cloned().collect(),
        upstream: paths
            .into_iter()
            .filter(|(id, _)| !direct.contains(id))
            .map(|(id, via)| Impact { id, via })
            .collect(),
    })
}

pub fn inspect(view: &View) -> Result<serde_json::Value, CapError> {
    let current = Graph::capture(view);
    let proposals = proposals(view)?;
    let comparisons: Result<Vec<_>, _> = proposals.iter().map(|p| compare(p, &current)).collect();
    Ok(
        serde_json::json!({"current":current,"current_revision":current.revision()?,"comparisons":comparisons?,
        "boundary":"Proposals change authored relationships only. Current means the captured map, not verified runtime behavior. Upstream impact follows incoming arrows in both versions; it represents potential dependency impact only when arrows mean depends on. Source freshness must be checked separately."}),
    )
}

pub fn describe(view: &View) -> Result<String, CapError> {
    let proposals = proposals(view)?;
    let current = Graph::capture(view);
    let mut out = format!(
        "ARCHITECTURE PROPOSALS (verdicts do not apply the map)\nCurrent semantic revision: {}\n",
        current.revision()?
    );
    for proposal in &proposals {
        if let Ok(diff) = compare(proposal, &current) {
            out.push_str(&format!("[{}] {} by {}: {} added, {} removed; directly affected [{}]; upstream [{}]; baseline {}\n",
                proposal.id, proposal.definition.title, proposal.created_by, diff.added.len(), diff.removed.len(), diff.direct.join(", "),
                diff.upstream.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(", "), if diff.baseline_changed { "changed" } else { "matches map" }));
            out.push_str(&format!(
                "  Verdict: {}{}\n",
                proposal.verdict,
                if proposal.decided_by.is_empty() {
                    String::new()
                } else {
                    format!(" by {}", proposal.decided_by)
                }
            ));
            for impact in &diff.upstream {
                out.push_str(&format!(
                    "  UPSTREAM {} via {}\n",
                    impact.id,
                    impact.via.join(" -> ")
                ));
            }
            if diff.baseline_changed {
                out.push_str("  The current map changed after this proposal began; its captured baseline is stale.\n");
            }
            for (verb, edges) in [("ADD", &diff.added), ("REMOVE", &diff.removed)] {
                for edge in edges {
                    out.push_str(&format!(
                        "  {verb} {} -> {}: {}\n",
                        edge.from, edge.to, edge.label
                    ));
                }
            }
        }
    }
    Ok(out)
}
