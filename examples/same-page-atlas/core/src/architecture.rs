//! Shared relationship proposals. Each revision is a separate CRDT object so
//! concurrent alternatives survive. A proposal never changes the mapped graph.
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::{Atlas, AtlasError};

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
    pub fn capture(atlas: &Atlas) -> Self {
        let mut components: Vec<_> = atlas
            .nodes
            .iter()
            .map(|n| Component {
                id: n.id.clone(),
                label: n.label.clone(),
                path: n.path.clone(),
                parent: n.parent.clone(),
            })
            .collect();
        components.sort_by(|a, b| a.id.cmp(&b.id));
        let mut relationships: Vec<_> = atlas
            .edges
            .iter()
            .map(|e| Relationship {
                from: e.from.clone(),
                to: e.to.clone(),
                label: if e.event.is_empty() {
                    e.label.clone()
                } else {
                    format!("{} [{}]", e.event, e.guard)
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

    pub fn revision(&self) -> Result<String, AtlasError> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|e| e.to_string())?)
        ))
    }

    fn validate(&self) -> Result<(), AtlasError> {
        let ids: BTreeSet<_> = self.components.iter().map(|c| &c.id).collect();
        if ids.len() != self.components.len() || self.components.len() > 512 {
            return Err("comparison requires at most 512 distinct components".into());
        }
        if self.relationships.len() > 4096 {
            return Err("comparison exceeds 4096 relationships".into());
        }
        let mut seen = BTreeSet::new();
        for edge in &self.relationships {
            if !ids.contains(&edge.from) || !ids.contains(&edge.to) {
                return Err("a proposed relationship references an unknown component".into());
            }
            if edge.label.chars().count() > 500 || !seen.insert(edge) {
                return Err("relationship labels must fit 500 characters and relationships must be distinct".into());
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
}

pub fn project(id: &str, definition: &str, created_by: String) -> Result<Proposal, AtlasError> {
    let definition: Definition = serde_json::from_str(definition)
        .map_err(|e| format!("invalid architecture proposal: {e}"))?;
    validate(&definition)?;
    Ok(Proposal {
        id: id.into(),
        created_by,
        definition,
    })
}

fn validate(definition: &Definition) -> Result<(), AtlasError> {
    if definition.title.trim().is_empty() || definition.title.chars().count() > 160 {
        return Err("a proposal title must contain 1 to 160 characters".into());
    }
    definition.baseline.validate()?;
    definition.proposed.validate()?;
    if definition.baseline.components != definition.proposed.components {
        return Err("relationship proposals preserve the captured components".into());
    }
    Ok(())
}

pub fn propose(
    scene: &mut Scene,
    input: &ProposalInput,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = crate::read(scene)?;
    if atlas.proposals.len() >= 256 {
        return Err("this page has reached its 256 proposal revision limit".into());
    }
    let (baseline, mut proposed) = if input.previous.is_empty() {
        let graph = Graph::capture(&atlas);
        (graph.clone(), graph)
    } else {
        let previous = atlas
            .proposals
            .iter()
            .find(|p| p.id == input.previous)
            .ok_or("the previous proposal does not exist")?;
        (
            previous.definition.baseline.clone(),
            previous.definition.proposed.clone(),
        )
    };
    if baseline.revision()? != input.baseline_revision {
        return Err("the comparison baseline changed; read it again before proposing".into());
    }
    let mut relationships: BTreeSet<_> = proposed.relationships.into_iter().collect();
    for edge in &input.remove {
        if !relationships.remove(edge) {
            return Err("cannot remove a relationship absent from this proposal revision".into());
        }
    }
    for edge in &input.add {
        if !relationships.insert(edge.clone()) {
            return Err("that relationship already exists in this proposal revision".into());
        }
    }
    proposed.relationships = relationships.into_iter().collect();
    let definition = Definition {
        title: input.title.trim().into(),
        previous: input.previous.clone(),
        baseline,
        proposed,
    };
    validate(&definition)?;
    let definition = serde_json::to_string(&definition).map_err(|e| e.to_string())?;
    scene
        .create_object_with_props(
            KIND,
            author.clone(),
            &[("definition", PropValue::Str(definition))],
        )
        .map(|id| id.into_string())
        .map_err(|e| e.to_string())
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

pub fn compare(proposal: &Proposal, current: &Graph) -> Result<Comparison, AtlasError> {
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

pub fn inspect(atlas: &Atlas) -> Result<serde_json::Value, AtlasError> {
    let current = Graph::capture(atlas);
    let comparisons: Result<Vec<_>, _> = atlas
        .proposals
        .iter()
        .map(|p| compare(p, &current))
        .collect();
    Ok(
        serde_json::json!({"current":current,"current_revision":current.revision()?,"comparisons":comparisons?,
        "boundary":"Proposals change authored relationships only. Current means the captured map, not verified runtime behavior. Upstream impact follows incoming arrows in both versions; it represents potential dependency impact only when arrows mean depends on. Source freshness must be checked separately."}),
    )
}

pub fn describe(atlas: &Atlas) -> String {
    if atlas.proposals.is_empty() {
        return String::new();
    }
    let current = Graph::capture(atlas);
    let mut out = String::from("\nARCHITECTURE PROPOSALS (drafts, not applied)\n");
    for proposal in &atlas.proposals {
        if let Ok(diff) = compare(proposal, &current) {
            out.push_str(&format!("[{}] {} by {}: {} added, {} removed; directly affected [{}]; upstream [{}]; baseline {}\n",
                proposal.id, proposal.definition.title, proposal.created_by, diff.added.len(), diff.removed.len(), diff.direct.join(", "),
                diff.upstream.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(", "), if diff.baseline_changed { "changed" } else { "matches map" }));
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Scene, Vec<String>) {
        let mut scene = Scene::new();
        let ids: Vec<_> = ["UI", "Host", "Core", "Unrelated"]
            .iter()
            .map(|label| {
                crate::place_node(
                    &mut scene,
                    &crate::NodePatch {
                        label: Some((*label).into()),
                        ..Default::default()
                    },
                    &Author::Agent,
                )
                .unwrap()
            })
            .collect();
        crate::link(&mut scene, &ids[0], &ids[1], "depends on", &Author::Agent).unwrap();
        crate::link(&mut scene, &ids[1], &ids[2], "depends on", &Author::Agent).unwrap();
        (scene, ids)
    }

    fn input(scene: &Scene) -> ProposalInput {
        ProposalInput {
            title: "Change host dependency".into(),
            baseline_revision: Graph::capture(&crate::read(scene).unwrap())
                .revision()
                .unwrap(),
            previous: String::new(),
            remove: vec![],
            add: vec![],
        }
    }

    #[test]
    fn proposal_changes_relationships_without_changing_the_current_map_and_reports_upstream_path() {
        let (mut scene, ids) = setup();
        let baseline = Graph::capture(&crate::read(&scene).unwrap());
        let mut request = input(&scene);
        request.remove.push(Relationship {
            from: ids[1].clone(),
            to: ids[2].clone(),
            label: "depends on".into(),
        });
        propose(&mut scene, &request, &Author::Human).unwrap();
        let atlas = crate::read(&scene).unwrap();
        assert_eq!(Graph::capture(&atlas), baseline);
        let diff = compare(&atlas.proposals[0], &baseline).unwrap();
        assert_eq!(diff.removed, request.remove);
        assert!(diff.added.is_empty());
        assert_eq!(
            diff.direct.iter().collect::<BTreeSet<_>>(),
            [&ids[1], &ids[2]].into_iter().collect()
        );
        assert_eq!(
            diff.upstream,
            vec![Impact {
                id: ids[0].clone(),
                via: vec![ids[0].clone(), ids[1].clone()]
            }]
        );
        assert!(!diff.direct.contains(&ids[3]));
        assert!(!diff.baseline_changed);
        assert_eq!(atlas.proposals[0].created_by, "human");
        let restored = Scene::from_state(&scene.encode_full().unwrap()).unwrap();
        assert_eq!(
            inspect(&crate::read(&restored).unwrap()).unwrap(),
            inspect(&atlas).unwrap()
        );
    }

    #[test]
    fn no_change_and_layout_changes_have_no_impact_but_semantic_edits_stale_the_baseline() {
        let (mut scene, ids) = setup();
        let request = input(&scene);
        propose(&mut scene, &request, &Author::Agent).unwrap();
        crate::place_node(
            &mut scene,
            &crate::NodePatch {
                id: Some(ids[0].clone()),
                x: Some(500.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        let atlas = crate::read(&scene).unwrap();
        let diff = compare(&atlas.proposals[0], &Graph::capture(&atlas)).unwrap();
        assert!(!diff.baseline_changed);
        assert!(diff.direct.is_empty() && diff.upstream.is_empty());
        crate::place_node(
            &mut scene,
            &crate::NodePatch {
                id: Some(ids[0].clone()),
                label: Some("Changed UI".into()),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        let atlas = crate::read(&scene).unwrap();
        assert!(
            compare(&atlas.proposals[0], &Graph::capture(&atlas))
                .unwrap()
                .baseline_changed
        );
    }

    #[test]
    fn invalid_and_stale_writes_are_atomic_and_revisions_keep_alternatives() {
        let (mut scene, ids) = setup();
        let request = input(&scene);
        let first = propose(&mut scene, &request, &Author::Agent).unwrap();
        let before = scene.encode_full().unwrap();
        let mut bad = request.clone();
        bad.baseline_revision = "stale".into();
        assert!(propose(&mut scene, &bad, &Author::Human).is_err());
        bad = request.clone();
        bad.add.push(Relationship {
            from: ids[0].clone(),
            to: "missing".into(),
            label: "calls".into(),
        });
        assert!(propose(&mut scene, &bad, &Author::Human).is_err());
        assert_eq!(scene.encode_full().unwrap(), before);
        let mut change = request.clone();
        change.previous = first.clone();
        change.add.push(Relationship {
            from: ids[2].clone(),
            to: ids[1].clone(),
            label: "calls".into(),
        });
        let second = propose(&mut scene, &change, &Author::Human).unwrap();
        let third = propose(&mut scene, &change, &Author::Agent).unwrap();
        assert_ne!(second, third);
        let atlas = crate::read(&scene).unwrap();
        assert_eq!(atlas.proposals.len(), 3);
        for proposal in atlas.proposals.iter().filter(|p| p.id != first) {
            let diff = compare(proposal, &Graph::capture(&atlas)).unwrap();
            assert_eq!(diff.added.len(), 1);
            assert_eq!(diff.upstream.len(), 1); // Cycle terminates and unrelated component stays out.
        }
        change.previous = second;
        assert!(propose(&mut scene, &change, &Author::Human)
            .unwrap_err()
            .contains("already exists"));
    }

    #[test]
    fn concurrent_peer_proposals_merge_and_each_reader_sees_the_other_authors_revision() {
        let (mut left, _) = setup();
        let mut right = Scene::from_state(&left.encode_full().unwrap()).unwrap();
        let baseline = crate::read(&left).unwrap().digest();
        let request = input(&left);
        let a = propose(&mut left, &request, &Author::Human).unwrap();
        let b = propose(&mut right, &request, &Author::Agent).unwrap();
        let left_bytes = left.encode_full().unwrap();
        let right_bytes = right.encode_full().unwrap();
        left.apply_update(&right_bytes).unwrap();
        right.apply_update(&left_bytes).unwrap();
        let atlas = crate::read(&left).unwrap();
        assert_eq!(atlas.proposals.len(), 2);
        assert_eq!(
            inspect(&atlas).unwrap(),
            inspect(&crate::read(&right).unwrap()).unwrap()
        );
        let human = crate::changes_for(&baseline, &atlas.digest(), "human").join("\n");
        let agent = crate::changes_for(&baseline, &atlas.digest(), "agent").join("\n");
        assert!(human.contains(&b) && !human.contains(&a));
        assert!(agent.contains(&a) && !agent.contains(&b));
    }

    #[test]
    fn decision_draft_readback_has_canonical_order_across_restored_peers() {
        let (mut scene, ids) = setup();
        let definition = serde_json::json!({
            "id":"", "context_ids":[], "content":"unreviewed", "created_by":"agent",
            "input":{"ids":[ids[0]],"title":"Review dependency","decision":"Keep the dependency explicit",
                "rationale":"Keep both peers aligned","tradeoffs":"Changes need review","consequences":"Check the source",
                "alternatives":[{"option":"Remove it","reason_not_chosen":"Needs further review"}],
                "checks":[{"id":"STATE-01","requirement":"Keep known states",
                    "check":{"backend":"rust_enum_shape","args":{"file":"lib.rs","enum_name":"State","expected_variants":["Ready"],"expected_serde_rename_all":null}},
                    "evidence_files":["lib.rs"]}],"not_enforced":[]}
        }).to_string();
        for _ in 0..8 {
            scene
                .create_object_with_props(
                    crate::decision::KIND,
                    Author::Agent,
                    &[("definition", PropValue::Str(definition.clone()))],
                )
                .unwrap();
        }
        let atlas = crate::read(&scene).unwrap();
        assert!(atlas
            .decision_drafts
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id));
        let peer = Scene::from_state(&scene.encode_full().unwrap()).unwrap();
        assert_eq!(atlas.describe(), crate::read(&peer).unwrap().describe());
    }
}
