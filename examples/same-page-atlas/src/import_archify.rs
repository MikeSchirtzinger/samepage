//! Archify architecture and workflow import for the Atlas node register.
//!
//! Archify's cards become Atlas nodes, its boundaries, lanes, and groups
//! become container nodes, and its directed connections become Atlas links.
//! Every imported card also gets exactly one claim saying what the typed
//! source says it is. Complete repository evidence upgrades that claim from
//! `inferred` to `verified` and keeps the source pinned to its commit.

use std::collections::{HashMap, HashSet};

use ag_ui_canvas::scene::{Author, Scene};
use same_page_atlas_core as atlas;
use serde::Deserialize;

const COMPONENT_TYPES: &[&str] = &[
    "frontend",
    "backend",
    "database",
    "cloud",
    "security",
    "messagebus",
    "external",
];
const ARCHITECTURE_VERSION: u64 = 1;
const WORKFLOW_VERSIONS: &[u64] = &[1, 2];
const ARCHITECTURE_CARD_WIDTH: f64 = 232.0;
pub(crate) const ARCHITECTURE_CARD_HEIGHT: f64 = 280.0;

#[derive(Debug, Deserialize)]
struct Envelope {
    schema_version: u64,
    diagram_type: String,
}

#[derive(Debug, Deserialize)]
struct Meta {
    title: String,
    #[serde(default)]
    repository: Option<Repository>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    revision: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Source {
    path: String,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default, rename = "endLine", alias = "end_line")]
    end_line: Option<u64>,
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Architecture {
    meta: Meta,
    #[serde(default)]
    layout: Option<GridLayout>,
    components: Vec<ArchitectureNode>,
    #[serde(default)]
    boundaries: Vec<Boundary>,
    #[serde(default)]
    connections: Vec<Connection>,
}

#[derive(Debug, Deserialize)]
struct GridLayout {
    #[serde(default)]
    origin: Option<[f64; 2]>,
    #[serde(default)]
    cols: Option<usize>,
    #[serde(default, rename = "gapX")]
    gap_x: Option<f64>,
    #[serde(default, rename = "gapY")]
    gap_y: Option<f64>,
    #[serde(default, rename = "cellW")]
    cell_w: Option<f64>,
    #[serde(default, rename = "cellH")]
    cell_h: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ArchitectureNode {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    label: String,
    #[serde(default)]
    sublabel: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    row: Option<usize>,
    #[serde(default)]
    col: Option<usize>,
    #[serde(default)]
    pos: Option<[f64; 2]>,
    #[serde(default)]
    size: Option<[f64; 2]>,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
struct Boundary {
    kind: String,
    label: String,
    wraps: Vec<String>,
    #[serde(default)]
    pad: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Connection {
    from: String,
    to: String,
    #[serde(default)]
    label: String,
}

#[derive(Debug, Deserialize)]
struct Workflow {
    meta: Meta,
    lanes: Vec<Lane>,
    #[serde(default)]
    groups: Vec<Group>,
    nodes: Vec<WorkflowNode>,
    #[serde(default)]
    edges: Vec<Connection>,
    #[serde(default, rename = "mainPath")]
    main_path: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Lane {
    id: String,
    label: String,
}

#[derive(Debug, Deserialize)]
struct Group {
    id: String,
    label: String,
    lane: String,
    #[serde(rename = "fromCol")]
    from_col: usize,
    #[serde(rename = "toCol")]
    to_col: usize,
}

#[derive(Debug, Deserialize)]
struct WorkflowNode {
    id: String,
    lane: String,
    col: usize,
    #[serde(rename = "type")]
    kind: String,
    label: String,
    #[serde(default)]
    sublabel: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default, rename = "yOffset")]
    y_offset: Option<f64>,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    sources: Vec<Source>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedSource {
    pub(crate) path: String,
    pub(crate) lines: String,
    pub(crate) revision: String,
}

#[derive(Debug)]
struct PlannedFrame {
    reference: String,
    label: String,
    parent: Option<String>,
    x: f64,
    y: f64,
    w: f64,
}

#[derive(Debug)]
struct PlannedNode {
    reference: String,
    label: String,
    note: String,
    kind: String,
    parent: Option<String>,
    x: f64,
    y: f64,
    w: f64,
    source: Option<VerifiedSource>,
    claim: String,
}

#[derive(Debug)]
struct PlannedRelation {
    from: String,
    to: String,
    label: String,
}

#[derive(Debug)]
pub(crate) struct ArchifyPlan {
    diagram_type: &'static str,
    title: String,
    frames: Vec<PlannedFrame>,
    nodes: Vec<PlannedNode>,
    relations: Vec<PlannedRelation>,
    main_path: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ImportedArchify {
    pub(crate) diagram_type: &'static str,
    pub(crate) title: String,
    pub(crate) node_ids: Vec<String>,
    pub(crate) frame_ids: Vec<String>,
    pub(crate) relation_ids: Vec<String>,
    pub(crate) claim_ids: Vec<String>,
}

impl ArchifyPlan {
    pub(crate) fn verified_sources(&self) -> impl Iterator<Item = &VerifiedSource> {
        self.nodes.iter().filter_map(|node| node.source.as_ref())
    }

    pub(crate) fn land(
        &self,
        scene: &mut Scene,
        author: &Author,
    ) -> Result<ImportedArchify, String> {
        let mut drafts = self
            .frames
            .iter()
            .map(|frame| atlas::NodeDraft {
                reference: Some(frame.reference.clone()),
                patch: atlas::NodePatch {
                    label: Some(frame.label.clone()),
                    x: Some(frame.x),
                    y: Some(frame.y),
                    w: Some(frame.w),
                    parent: frame.parent.clone(),
                    ..atlas::NodePatch::default()
                },
            })
            .collect::<Vec<_>>();
        drafts.extend(self.nodes.iter().map(|node| {
            atlas::NodeDraft {
                reference: Some(node.reference.clone()),
                patch: atlas::NodePatch {
                    label: Some(node.label.clone()),
                    note: (!node.note.is_empty()).then(|| node.note.clone()),
                    color: Some(
                        match node.kind.as_str() {
                            "frontend" => "blue",
                            "backend" => "green",
                            "security" => "red",
                            "database" => "violet",
                            "messagebus" | "cloud" => "amber",
                            _ => "slate",
                        }
                        .to_string(),
                    ),
                    tone: Some(if node.kind == "security" {
                        "risk".to_string()
                    } else {
                        "concept".to_string()
                    }),
                    x: Some(node.x),
                    y: Some(node.y),
                    w: Some(node.w),
                    parent: node.parent.clone(),
                    kind: Some(if node.kind == "database" {
                        "ellipse".to_string()
                    } else if node.kind == "security" {
                        "diamond".to_string()
                    } else {
                        atlas::DEFAULT_KIND.to_string()
                    }),
                    ..atlas::NodePatch::default()
                },
            }
        }));
        let links = self
            .relations
            .iter()
            .map(|relation| atlas::LinkDraft {
                from: relation.from.clone(),
                to: relation.to.clone(),
                label: relation.label.clone(),
            })
            .collect::<Vec<_>>();
        let drawn = atlas::draw(scene, &drafts, &links, author)?;
        let ids = drawn
            .nodes
            .iter()
            .filter_map(|(reference, id)| reference.as_ref().map(|key| (key.as_str(), id.clone())))
            .collect::<HashMap<_, _>>();

        let frame_ids = self
            .frames
            .iter()
            .map(|frame| resolve(&ids, &frame.reference, "frame"))
            .collect::<Result<Vec<_>, _>>()?;
        let node_ids = self
            .nodes
            .iter()
            .map(|node| resolve(&ids, &node.reference, "node"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut claim_ids = Vec::with_capacity(self.nodes.len());
        for (node, about) in self.nodes.iter().zip(&node_ids) {
            let (basis, path, lines, revision) = match &node.source {
                Some(source) => (
                    "verified",
                    source.path.as_str(),
                    source.lines.as_str(),
                    source.revision.as_str(),
                ),
                None => ("inferred", "", "", ""),
            };
            claim_ids.push(atlas::claims::claim_at(
                scene,
                about,
                &node.claim,
                basis,
                path,
                lines,
                revision,
                author,
            )?);
        }

        if !self.main_path.is_empty() {
            let beats = self
                .main_path
                .iter()
                .enumerate()
                .map(|(index, reference)| {
                    let node = self
                        .nodes
                        .iter()
                        .find(|node| &node.reference == reference)
                        .ok_or("guided path node is missing")?;
                    let target = resolve(&ids, reference, "guided path")?;
                    let next = self.main_path.get(index + 1);
                    let beat = atlas::ExplanationBeat {
                        id: format!("step-{index}"),
                        title: node.label.clone(),
                        intent: format!("Understand the role of {} in the workflow", node.label),
                        cue: if node.note.is_empty() {
                            node.claim.clone()
                        } else {
                            node.note.clone()
                        },
                        evidence: vec![atlas::ExplanationEvidence {
                            target_id: target.clone(),
                            detail: node.claim.clone(),
                        }],
                        actions: vec![atlas::ExplanationVisualAction {
                            kind: "point".into(),
                            target_id: target,
                            cue_span: None,
                        }],
                        advance: atlas::ExplanationAdvance {
                            mode: if next.is_some() {
                                "continue"
                            } else {
                                "terminal"
                            }
                            .into(),
                            prompt: String::new(),
                            transitions: next
                                .map(|_| {
                                    vec![atlas::ExplanationTransition {
                                        id: "next".into(),
                                        label: "Continue".into(),
                                        control: Some("button".into()),
                                        next: Some(format!("step-{}", index + 1)),
                                        target_ids: vec![],
                                        phrases: vec![],
                                    }]
                                })
                                .unwrap_or_default(),
                        },
                    };
                    Ok(beat)
                })
                .collect::<Result<Vec<_>, String>>()?;
            atlas::define_explanation_flow(scene, &atlas::ExplanationFlowDefinition {
                schema: atlas::EXPLANATION_FLOW_SCHEMA.into(), id: format!("workflow-{}",node_ids[0]), title: format!("{}: main path",self.title), goal: "Follow the workflow one step at a time. Question any step that differs from your understanding.".into(),
                about: node_ids.clone(), scene_ids: vec![], start: "step-0".into(), beats,
            }, author)?;
        }
        Ok(ImportedArchify {
            diagram_type: self.diagram_type,
            title: self.title.clone(),
            node_ids,
            frame_ids,
            relation_ids: drawn.edges,
            claim_ids,
        })
    }
}

fn resolve(ids: &HashMap<&str, String>, reference: &str, kind: &str) -> Result<String, String> {
    ids.get(reference)
        .cloned()
        .ok_or_else(|| format!("Archify {kind} {reference:?} did not land"))
}

pub(crate) fn parse(document: &str, dx: f64, dy: f64) -> Result<ArchifyPlan, String> {
    if !dx.is_finite() || !dy.is_finite() {
        return Err("Archify import offsets must be finite numbers".to_string());
    }
    let envelope: Envelope = serde_json::from_str(document)
        .map_err(|error| format!("Archify import is not valid typed JSON: {error}"))?;
    match envelope.diagram_type.as_str() {
        "architecture" if envelope.schema_version == ARCHITECTURE_VERSION => {
            let source: Architecture = serde_json::from_str(document)
                .map_err(|error| format!("invalid Archify architecture JSON: {error}"))?;
            architecture(source, dx, dy)
        }
        "architecture" => Err(format!(
            "unsupported Archify architecture schema version {}; supported version is {ARCHITECTURE_VERSION}",
            envelope.schema_version
        )),
        "workflow" if WORKFLOW_VERSIONS.contains(&envelope.schema_version) => {
            let source: Workflow = serde_json::from_str(document)
                .map_err(|error| format!("invalid Archify workflow JSON: {error}"))?;
            workflow(source, dx, dy)
        }
        "workflow" => Err(format!(
            "unsupported Archify workflow schema version {}; supported versions are 1 and 2",
            envelope.schema_version
        )),
        other => Err(format!(
            "unsupported Archify diagram type {other:?}; supported types are architecture and workflow"
        )),
    }
}

fn architecture(source: Architecture, dx: f64, dy: f64) -> Result<ArchifyPlan, String> {
    require_title(&source.meta.title)?;
    let revision = source
        .meta
        .repository
        .as_ref()
        .map(|repository| repository.revision.as_str());
    let ids = validate_nodes(
        source
            .components
            .iter()
            .map(|node| (node.id.as_str(), node.label.as_str(), node.kind.as_str())),
    )?;
    validate_relations(&source.connections, &ids)?;

    let layout = source.layout.as_ref();
    let cols = layout.and_then(|grid| grid.cols).unwrap_or(4).max(1);
    let origin = layout.and_then(|grid| grid.origin).unwrap_or([80.0, 120.0]);
    let cell_w = layout.and_then(|grid| grid.cell_w).unwrap_or(232.0);
    let cell_h = layout.and_then(|grid| grid.cell_h).unwrap_or(120.0);
    let gap_x = layout.and_then(|grid| grid.gap_x).unwrap_or(56.0);
    let gap_y = layout.and_then(|grid| grid.gap_y).unwrap_or(48.0);
    let authored = source
        .components
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let row = node.row.unwrap_or(index / cols);
            let col = node.col.unwrap_or(index % cols);
            let pos = node.pos.unwrap_or([
                origin[0] + col as f64 * (cell_w + gap_x),
                origin[1] + row as f64 * (cell_h + gap_y),
            ]);
            let size = node.size.unwrap_or([cell_w, cell_h]);
            if !pos.into_iter().chain(size).all(f64::is_finite) || size[0] <= 0.0 || size[1] <= 0.0
            {
                return Err(format!(
                    "Archify architecture node {:?} needs finite coordinates and a positive size",
                    node.label
                ));
            }
            Ok((pos, size))
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Archify coordinates describe its own compact cards. Atlas cards carry a
    // claim row and can be much taller, so copying those coordinates makes the
    // rendered cards collide even when the source diagram was clear. Scale the
    // whole authored coordinate system by the ratio between the largest source
    // card and the Atlas footprint reserved here. That preserves the authored
    // topology while making every source-card-sized gap an Atlas-card-sized gap.
    let authored_width = authored.iter().map(|(_, size)| size[0]).fold(0.0, f64::max);
    let authored_height = authored.iter().map(|(_, size)| size[1]).fold(0.0, f64::max);
    let scale_x = (ARCHITECTURE_CARD_WIDTH / authored_width).max(1.0);
    let scale_y = (ARCHITECTURE_CARD_HEIGHT / authored_height).max(1.0);
    let positions = authored
        .into_iter()
        .map(|(pos, size)| {
            (
                pos[0] * scale_x + dx,
                pos[1] * scale_y + dy,
                (size[0] * scale_x).max(ARCHITECTURE_CARD_WIDTH),
            )
        })
        .collect::<Vec<_>>();
    let component_ids = source
        .components
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let (frames, parents) = architecture_frames(&source.boundaries, &component_ids, &positions)?;
    let nodes = source
        .components
        .into_iter()
        .enumerate()
        .map(|(index, node)| {
            let source = complete_source(node.source.as_ref(), &node.sources, revision)?;
            let claim = claim_text(
                &node.label,
                &format!("{} architecture component", node.kind),
                &node.sublabel,
                &node.tag,
            );
            Ok(PlannedNode {
                reference: node.id,
                label: node.label,
                note: note(&node.sublabel, &node.tag),
                kind: node.kind,
                parent: parents[index].clone(),
                x: positions[index].0,
                y: positions[index].1,
                w: positions[index].2,
                source,
                claim,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ArchifyPlan {
        diagram_type: "architecture",
        title: source.meta.title,
        frames,
        nodes,
        relations: relations(source.connections),
        main_path: vec![],
    })
}

fn architecture_frames(
    boundaries: &[Boundary],
    node_ids: &[String],
    positions: &[(f64, f64, f64)],
) -> Result<(Vec<PlannedFrame>, Vec<Option<String>>), String> {
    let known_nodes = node_ids.iter().cloned().collect::<HashSet<_>>();
    let mut sets = Vec::with_capacity(boundaries.len());
    for (index, boundary) in boundaries.iter().enumerate() {
        if !matches!(boundary.kind.as_str(), "region" | "security-group") {
            return Err(format!(
                "architecture boundary {index} has unknown kind {:?}",
                boundary.kind
            ));
        }
        if boundary.label.trim().is_empty() || boundary.wraps.is_empty() {
            return Err(format!(
                "architecture boundary {index} needs a label and at least one wrapped node"
            ));
        }
        let set = boundary.wraps.iter().cloned().collect::<HashSet<_>>();
        if set.len() != boundary.wraps.len() {
            return Err(format!(
                "architecture boundary {:?} wraps the same node more than once",
                boundary.label
            ));
        }
        for id in &set {
            if !known_nodes.contains(id) {
                return Err(format!(
                    "architecture boundary {:?} wraps unknown node {id:?}",
                    boundary.label
                ));
            }
        }
        sets.push(set);
    }
    for left in 0..sets.len() {
        for right in left + 1..sets.len() {
            let overlaps = !sets[left].is_disjoint(&sets[right]);
            let nested = sets[left].is_subset(&sets[right]) || sets[right].is_subset(&sets[left]);
            if overlaps && !nested {
                return Err(format!(
                    "architecture boundaries {:?} and {:?} overlap without nesting; Atlas frames have one parent",
                    boundaries[left].label, boundaries[right].label
                ));
            }
            if sets[left] == sets[right] {
                return Err(format!(
                    "architecture boundaries {:?} and {:?} wrap the same nodes; their nesting is ambiguous",
                    boundaries[left].label, boundaries[right].label
                ));
            }
        }
    }

    let references = (0..boundaries.len())
        .map(|index| format!("archify:boundary:{index}"))
        .collect::<Vec<_>>();
    let parents = sets
        .iter()
        .enumerate()
        .map(|(index, members)| {
            sets.iter()
                .enumerate()
                .filter(|(candidate, set)| *candidate != index && members.is_subset(set))
                .min_by_key(|(_, set)| set.len())
                .map(|(parent, _)| references[parent].clone())
        })
        .collect::<Vec<_>>();
    let depths = (0..boundaries.len())
        .map(|mut index| {
            let mut depth = 0usize;
            while let Some(parent) = parents[index].as_deref() {
                index = references
                    .iter()
                    .position(|reference| reference == parent)
                    .expect("boundary parent is local");
                depth += 1;
            }
            depth
        })
        .collect::<Vec<_>>();
    let mut order = (0..boundaries.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| depths[*index]);
    let frames = order
        .into_iter()
        .map(|index| {
            let pad = boundaries[index].pad.unwrap_or(22.0);
            let wrapped = boundaries[index]
                .wraps
                .iter()
                .filter_map(|id| node_ids.iter().position(|candidate| candidate == id))
                .collect::<Vec<_>>();
            let left = wrapped
                .iter()
                .map(|node| positions[*node].0)
                .fold(f64::INFINITY, f64::min);
            let top = wrapped
                .iter()
                .map(|node| positions[*node].1)
                .fold(f64::INFINITY, f64::min);
            let right = wrapped
                .iter()
                .map(|node| positions[*node].0 + positions[*node].2)
                .fold(f64::NEG_INFINITY, f64::max);
            PlannedFrame {
                reference: references[index].clone(),
                label: boundaries[index].label.clone(),
                parent: parents[index].clone(),
                x: left - pad,
                y: top - pad - atlas::CONTAINER_TITLE_HEIGHT,
                w: right - left + pad * 2.0,
            }
        })
        .collect::<Vec<_>>();
    let node_parents = node_ids
        .iter()
        .map(|id| {
            sets.iter()
                .enumerate()
                .filter(|(_, set)| set.contains(id))
                .min_by_key(|(_, set)| set.len())
                .map(|(index, _)| references[index].clone())
        })
        .collect();
    Ok((frames, node_parents))
}

fn workflow(source: Workflow, dx: f64, dy: f64) -> Result<ArchifyPlan, String> {
    for id in &source.main_path {
        if !source.nodes.iter().any(|node| &node.id == id) {
            return Err(format!("guided path refers to unknown node {id:?}"));
        }
    }
    if source.main_path.len() > 64
        || source.main_path.iter().collect::<HashSet<_>>().len() != source.main_path.len()
    {
        return Err("guided path needs at most 64 distinct nodes".into());
    }
    require_title(&source.meta.title)?;
    let revision = source
        .meta
        .repository
        .as_ref()
        .map(|repository| repository.revision.as_str());
    let ids = validate_nodes(
        source
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.label.as_str(), node.kind.as_str())),
    )?;
    validate_relations(&source.edges, &ids)?;
    let lane_ids = unique_ids(
        source
            .lanes
            .iter()
            .map(|lane| (lane.id.as_str(), lane.label.as_str())),
        "workflow lane",
        true,
    )?;
    let group_ids = unique_ids(
        source
            .groups
            .iter()
            .map(|group| (group.id.as_str(), group.label.as_str())),
        "workflow group",
        false,
    )?;
    if !lane_ids.is_disjoint(&group_ids) {
        return Err("a workflow lane and group use the same id".to_string());
    }
    for group in &source.groups {
        if !lane_ids.contains(&group.lane) {
            return Err(format!(
                "workflow group {:?} names unknown lane {:?}",
                group.label, group.lane
            ));
        }
        if group.to_col < group.from_col {
            return Err(format!(
                "workflow group {:?} has toCol before fromCol",
                group.label
            ));
        }
    }

    let lane_references = source
        .lanes
        .iter()
        .map(|lane| (lane.id.as_str(), format!("archify:lane:{}", lane.id)))
        .collect::<HashMap<_, _>>();
    let group_references = source
        .groups
        .iter()
        .map(|group| (group.id.as_str(), format!("archify:group:{}", group.id)))
        .collect::<HashMap<_, _>>();
    let mut frames = source
        .lanes
        .iter()
        .enumerate()
        .map(|(index, lane)| PlannedFrame {
            reference: lane_references[&lane.id.as_str()].clone(),
            label: lane.label.clone(),
            parent: None,
            x: 48.0 + dx,
            y: 72.0 + dy + index as f64 * 290.0,
            w: 6.0 * 232.0,
        })
        .collect::<Vec<_>>();
    frames.extend(source.groups.iter().map(|group| {
        PlannedFrame {
            reference: group_references[&group.id.as_str()].clone(),
            label: group.label.clone(),
            parent: Some(lane_references[&group.lane.as_str()].clone()),
            x: 80.0 + dx + group.from_col as f64 * 240.0,
            y: 96.0
                + dy
                + source
                    .lanes
                    .iter()
                    .position(|lane| lane.id == group.lane)
                    .expect("group lane was validated") as f64
                    * 290.0,
            w: (group.to_col - group.from_col + 1) as f64 * 232.0,
        }
    }));

    let mut group_members = vec![0usize; source.groups.len()];
    let nodes = source
        .nodes
        .into_iter()
        .map(|node| {
            let lane_index = source
                .lanes
                .iter()
                .position(|lane| lane.id == node.lane)
                .ok_or_else(|| {
                    format!(
                        "workflow node {:?} names unknown lane {:?}",
                        node.label, node.lane
                    )
                })?;
            let groups = source
                .groups
                .iter()
                .enumerate()
                .filter(|(_, group)| {
                    group.lane == node.lane
                        && node.col >= group.from_col
                        && node.col <= group.to_col
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if groups.len() > 1 {
                return Err(format!(
                    "workflow node {:?} falls inside more than one group; Atlas frames have one parent",
                    node.label
                ));
            }
            let parent = match groups.first().copied() {
                Some(group) => {
                    group_members[group] += 1;
                    Some(group_references[&source.groups[group].id.as_str()].clone())
                }
                None => Some(lane_references[&node.lane.as_str()].clone()),
            };
            let lane_label = &source.lanes[lane_index].label;
            let evidence = complete_source(node.source.as_ref(), &node.sources, revision)?;
            let claim = claim_text(
                &node.label,
                &format!("{} workflow node in the {lane_label} lane", node.kind),
                &node.sublabel,
                &node.tag,
            );
            Ok(PlannedNode {
                reference: node.id,
                label: node.label,
                note: note(&node.sublabel, &node.tag),
                kind: node.kind,
                parent,
                x: 80.0 + dx + node.col as f64 * 240.0,
                y: 120.0 + dy + lane_index as f64 * 290.0 + node.y_offset.unwrap_or(0.0),
                // Source pixels describe a static card. Atlas also exposes editable claims.
                w: node.width.unwrap_or(200.0).max(200.0),
                source: evidence,
                claim,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (index, count) in group_members.into_iter().enumerate() {
        if count == 0 {
            return Err(format!(
                "workflow group {:?} contains no node",
                source.groups[index].label
            ));
        }
    }
    Ok(ArchifyPlan {
        diagram_type: "workflow",
        title: source.meta.title,
        frames,
        nodes,
        relations: relations(source.edges),
        main_path: source.main_path,
    })
}

fn require_title(title: &str) -> Result<(), String> {
    if title.trim().is_empty() {
        Err("an Archify diagram needs a non-empty title".to_string())
    } else {
        Ok(())
    }
}

fn validate_nodes<'a>(
    nodes: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    for (id, label, kind) in nodes {
        if id.trim().is_empty() || label.trim().is_empty() {
            return Err("every Archify node needs a non-empty id and label".to_string());
        }
        if !COMPONENT_TYPES.contains(&kind) {
            return Err(format!(
                "Archify node {id:?} has unknown component type {kind:?}"
            ));
        }
        if !ids.insert(id.to_string()) {
            return Err(format!("duplicate Archify node id {id:?}"));
        }
    }
    if ids.is_empty() {
        return Err("an Archify diagram needs at least one node".to_string());
    }
    Ok(ids)
}

fn unique_ids<'a>(
    values: impl Iterator<Item = (&'a str, &'a str)>,
    kind: &str,
    required: bool,
) -> Result<HashSet<String>, String> {
    let mut ids = HashSet::new();
    for (id, label) in values {
        if id.trim().is_empty() || label.trim().is_empty() {
            return Err(format!("every {kind} needs a non-empty id and label"));
        }
        if !ids.insert(id.to_string()) {
            return Err(format!("duplicate {kind} id {id:?}"));
        }
    }
    if required && ids.is_empty() {
        return Err(format!("an Archify workflow needs at least one {kind}"));
    }
    Ok(ids)
}

fn validate_relations(relations: &[Connection], ids: &HashSet<String>) -> Result<(), String> {
    for (index, relation) in relations.iter().enumerate() {
        for endpoint in [&relation.from, &relation.to] {
            if !ids.contains(endpoint) {
                return Err(format!(
                    "Archify relation {index} names unknown node {endpoint:?}"
                ));
            }
        }
    }
    Ok(())
}

fn relations(source: Vec<Connection>) -> Vec<PlannedRelation> {
    source
        .into_iter()
        .map(|relation| PlannedRelation {
            from: relation.from,
            to: relation.to,
            label: relation.label,
        })
        .collect()
}

fn complete_source(
    source: Option<&Source>,
    sources: &[Source],
    repository_revision: Option<&str>,
) -> Result<Option<VerifiedSource>, String> {
    let all = source.into_iter().chain(sources);
    let mut complete = None;
    for source in all {
        let revision = source
            .revision
            .as_deref()
            .or(repository_revision)
            .unwrap_or_default()
            .trim();
        if !revision.is_empty()
            && (revision.len() != atlas::REVISION_LENGTH
                || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(format!(
                "Archify source revision {revision:?} must be a full forty-hex commit"
            ));
        }
        let Some(line) = source.line else { continue };
        let end = source.end_line.unwrap_or(line);
        if line == 0 || end == 0 {
            return Err(format!(
                "Archify source {:?} uses a zero line; source lines are 1-based",
                source.path
            ));
        }
        if end < line {
            return Err(format!(
                "Archify source {:?} has endLine {end} before line {line}",
                source.path
            ));
        }
        if complete.is_none() && !source.path.trim().is_empty() && !revision.is_empty() {
            complete = Some(VerifiedSource {
                path: source.path.clone(),
                lines: if line == end {
                    line.to_string()
                } else {
                    format!("{line}-{end}")
                },
                revision: revision.to_ascii_lowercase(),
            });
        }
    }
    Ok(complete)
}

fn note(sublabel: &str, tag: &str) -> String {
    match (sublabel.trim(), tag.trim()) {
        ("", "") => String::new(),
        (sublabel, "") => sublabel.to_string(),
        ("", tag) => format!("tag: {tag}"),
        (sublabel, tag) => format!("{sublabel}; tag: {tag}"),
    }
}

fn claim_text(label: &str, kind: &str, sublabel: &str, tag: &str) -> String {
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    let mut claim = format!("{label} is {article} {kind}");
    if !sublabel.trim().is_empty() {
        claim.push_str(&format!(": {}", sublabel.trim()));
    }
    if !tag.trim().is_empty() {
        claim.push_str(&format!(" [tag: {}]", tag.trim()));
    }
    claim
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_fixture(document: &str) -> (atlas::Atlas, ImportedArchify) {
        let plan = parse(document, 0.0, 0.0).expect("parse Archify fixture");
        let mut scene = Scene::new();
        let imported = plan
            .land(&mut scene, &Author::Agent)
            .expect("land Archify fixture");
        (atlas::read(&scene).expect("read imported Atlas"), imported)
    }

    fn assert_exact_claims(atlas: &atlas::Atlas, expected: &[(&str, &str)]) {
        let mut section = String::from(
            "\nCLAIMS (what the agent says it understands, and what the human said back)\n",
        );
        let expected = expected.iter().copied().collect::<HashMap<_, _>>();
        assert_eq!(expected.len(), atlas.claims.len());
        for node in &atlas.nodes {
            let claims = atlas.claims_of(&node.id);
            if claims.is_empty() {
                continue;
            }
            let text = expected
                .get(node.label.as_str())
                .unwrap_or_else(|| panic!("unexpected imported claim on {:?}", node.label));
            let label = &node.label;
            assert_eq!(claims.len(), 1, "{label} must have exactly one claim");
            section.push_str(&format!(
                "- \"{label}\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n    [{}] inferred \"{text}\" open\n",
                claims[0].id
            ));
        }
        assert_eq!(atlas::claims::describe(atlas), section);
    }

    #[test]
    fn the_architecture_fixture_imports_nodes_relations_frames_and_exact_claims() {
        let (atlas, imported) = import_fixture(include_str!(
            "../fixtures/archify/web-app.architecture.json"
        ));
        assert_eq!(imported.node_ids.len(), 10);
        assert_eq!(imported.relation_ids.len(), 9);
        assert_eq!(imported.frame_ids.len(), 2);
        assert_eq!(atlas.nodes.len(), 12);
        assert_eq!(atlas.edges.len(), 9);
        assert_eq!(atlas.claims.len(), 10);
        assert!(
            imported.frame_ids.iter().all(|id| atlas.is_container(id)),
            "every Archify boundary must be an Atlas frame"
        );
        assert_exact_claims(
            &atlas,
            &[
                (
                    "Users",
                    "Users is an external architecture component: Browser / Mobile",
                ),
                (
                    "Auth Provider",
                    "Auth Provider is a security architecture component: OAuth 2.0 [tag: JWT + PKCE]",
                ),
                (
                    "CloudFront",
                    "CloudFront is a cloud architecture component: CDN",
                ),
                (
                    "Load Balancer",
                    "Load Balancer is a cloud architecture component: HTTPS :443",
                ),
                (
                    "API Server",
                    "API Server is a backend architecture component: FastAPI :8000",
                ),
                (
                    "Redis",
                    "Redis is a database architecture component: cache :6379",
                ),
                (
                    "PostgreSQL",
                    "PostgreSQL is a database architecture component: primary :5432",
                ),
                (
                    "S3",
                    "S3 is a cloud architecture component: static assets [tag: OAI protected]",
                ),
                (
                    "SQS",
                    "SQS is a messagebus architecture component: job queue",
                ),
                (
                    "Worker",
                    "Worker is a backend architecture component: async jobs",
                ),
            ],
        );
    }

    #[test]
    fn the_workflow_fixture_imports_nodes_relations_frames_and_exact_claims() {
        let (atlas, imported) = import_fixture(include_str!(
            "../fixtures/archify/agent-tool-call.workflow.json"
        ));
        assert_eq!(imported.node_ids.len(), 12);
        assert_eq!(imported.relation_ids.len(), 11);
        assert_eq!(imported.frame_ids.len(), 8);
        assert_eq!(atlas.nodes.len(), 20);
        assert_eq!(atlas.edges.len(), 11);
        assert_eq!(atlas.claims.len(), 12);
        let flow = atlas.explanation.as_ref().expect("shared guided main path");
        assert_eq!(flow.definition.beats.len(), 8);
        assert_eq!(flow.definition.beats[0].title, "User");
        assert_eq!(flow.definition.beats[7].title, "Final Reply");
        assert!(
            !atlas.describe().contains(" OVERLAP "),
            "nested lane boundaries must not overlap"
        );
        assert!(
            imported.frame_ids.iter().all(|id| atlas.is_container(id)),
            "every Archify lane and group must be an Atlas frame"
        );
        assert_exact_claims(
            &atlas,
            &[
                (
                    "User",
                    "User is an external workflow node in the User Interface lane: asks for work",
                ),
                (
                    "Chat Surface",
                    "Chat Surface is a frontend workflow node in the User Interface lane: thread + files",
                ),
                (
                    "Final Reply",
                    "Final Reply is a backend workflow node in the User Interface lane: answer + changes",
                ),
                (
                    "Agent Planner",
                    "Agent Planner is a backend workflow node in the Agent Runtime lane: plan next step [tag: context aware]",
                ),
                (
                    "Tool Router",
                    "Tool Router is a backend workflow node in the Agent Runtime lane: choose capability",
                ),
                (
                    "Approval Gate",
                    "Approval Gate is a security workflow node in the Policy & Recovery lane: scope + consent [tag: block risky ops]",
                ),
                (
                    "Blocked",
                    "Blocked is a security workflow node in the Policy & Recovery lane: wait or reject",
                ),
                (
                    "Retry Path",
                    "Retry Path is a messagebus workflow node in the Policy & Recovery lane: revise request",
                ),
                (
                    "Tool Call",
                    "Tool Call is a messagebus workflow node in the Tool Execution & Evidence lane: shell / browser / MCP [tag: structured result]",
                ),
                (
                    "External API",
                    "External API is a cloud workflow node in the Tool Execution & Evidence lane: network service",
                ),
                (
                    "Context Store",
                    "Context Store is a database workflow node in the Tool Execution & Evidence lane: repo + memory",
                ),
                (
                    "Trace Log",
                    "Trace Log is a database workflow node in the Tool Execution & Evidence lane: events + output",
                ),
            ],
        );
    }

    #[test]
    fn complete_node_evidence_becomes_one_revision_pinned_verified_claim() {
        let document = r#"{
            "schema_version": 1,
            "diagram_type": "architecture",
            "meta": { "title": "Evidence" },
            "components": [{
                "id": "reader", "type": "backend", "label": "Reader",
                "source": {
                    "path": "src/reader.rs", "line": 12, "endLine": 18,
                    "revision": "0123456789abcdef0123456789abcdef01234567"
                }
            }]
        }"#;
        let (atlas, imported) = import_fixture(document);
        let claim = &atlas.claims[0];
        assert_eq!(imported.claim_ids.len(), 1);
        assert_eq!(claim.basis, "verified");
        assert_eq!(claim.path, "src/reader.rs");
        assert_eq!(claim.lines, "12-18");
        assert_eq!(claim.revision, "0123456789abcdef0123456789abcdef01234567");
    }

    #[test]
    fn unknown_archify_versions_and_diagram_types_are_refused_clearly() {
        let version = parse(
            r#"{"schema_version":9,"diagram_type":"architecture"}"#,
            0.0,
            0.0,
        )
        .expect_err("unknown version");
        assert!(version.contains("unsupported Archify architecture schema version 9"));

        let kind = parse(
            r#"{"schema_version":1,"diagram_type":"sequence"}"#,
            0.0,
            0.0,
        )
        .expect_err("unknown type");
        assert!(kind.contains("unsupported Archify diagram type \"sequence\""));
    }
}
