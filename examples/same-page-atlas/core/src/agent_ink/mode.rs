//! Document-level mode for the agent-ink layer.
//!
//! A "step" means two different things depending on why a document is under
//! construction. In LEARNING mode a step is a frame in an explanation: it
//! re-derives from the index deterministically, so advancing replays the
//! previous snapshot in full and then solves over it — cheap, and safe to
//! re-record with different values later. In ALIGNMENT mode a step is a
//! commitment: a pinned edit a caller makes between steps IS the decision
//! being recorded, so it must reach the new step instead of being discarded
//! by a full restore, and a step once recorded is immutable.
//!
//! The mode is a document-wide setting, not a per-step one, and it is fixed
//! the moment a timeline exists: [`set_agent_ink_mode`] refuses to change it
//! once [`super::step::record_current_step`] has stored a first step.
//! Promoting an existing timeline from learning into alignment semantics
//! means re-solving and re-recording every stored step under the new
//! rules — a distinct operation from a flag flip, built in
//! [`super::promote::promote_to_alignment`], which is the one caller allowed
//! to write a new mode over a document that already has steps.

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use yrs::{Any, Map, Out, Transact};

use crate::AtlasError;

pub(super) const KIND_MODE: &str = "agent_ink_mode";
const K_MODE: &str = "mode";

pub(super) const KIND_PROMOTION: &str = "agent_ink_promotion";
const K_PROMOTED_FROM: &str = "promoted_from";

/// What a recorded step means in this document. Defaults to `Learning`, so a
/// document that has never set a mode reads exactly as it always has.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentInkMode {
    #[default]
    Learning,
    Alignment,
}

impl AgentInkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Learning => "learning",
            Self::Alignment => "alignment",
        }
    }

    fn parse(value: &str) -> Result<Self, AtlasError> {
        match value {
            "learning" => Ok(Self::Learning),
            "alignment" => Ok(Self::Alignment),
            _ => Err(format!(
                "agent-ink mode must be learning or alignment; got {value:?}"
            )),
        }
    }
}

pub(super) fn project_mode(string: &impl Fn(&str) -> String) -> Result<AgentInkMode, AtlasError> {
    AgentInkMode::parse(&string(K_MODE))
}

/// A document-wide record that this document's mode did not start as
/// whatever [`AgentInkMode::as_str`] currently reports: it was promoted
/// there by [`super::promote::promote_to_alignment`], from `promoted_from`,
/// by whoever [`super::step::step_recorded_by`]-style `created_by`
/// attribution names on this object. Present only after a promotion has
/// run; a document that has always been in its current mode has none.
///
/// This is what lets a reader of a finished alignment document tell "born a
/// record" from "became one" — see the module doc comment on
/// [`super::promote`] for why that distinction has to be visible on the
/// document itself, not just true of its history somewhere off to the side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromotionRecord {
    pub promoted_from: AgentInkMode,
    pub promoted_by: String,
}

pub(super) fn project_promotion(
    string: &impl Fn(&str) -> String,
) -> Result<PromotionRecord, AtlasError> {
    let promoted_from = AgentInkMode::parse(&string(K_PROMOTED_FROM))?;
    let promoted_by = string("created_by");
    if promoted_by.is_empty() {
        return Err("stored agent-ink promotion record has no recorded author".to_string());
    }
    Ok(PromotionRecord {
        promoted_from,
        promoted_by,
    })
}

/// Set the document's agent-ink mode.
///
/// Refused once any step has been recorded. Flipping the mode of a document
/// that already has a timeline would turn a brainstorm sketch into a signed
/// record (or the reverse) without anyone re-examining what was captured
/// under the old rules, so there is no code path here that allows it. The
/// real path around this guard is [`super::promote::promote_to_alignment`],
/// which earns the mode change by re-solving and re-recording every stored
/// step rather than skipping this check.
pub fn set_agent_ink_mode(
    scene: &mut Scene,
    mode: AgentInkMode,
    author: &Author,
) -> Result<(), AtlasError> {
    if !super::step::step_entries(scene)?.is_empty() {
        return Err("the agent-ink mode cannot change once a step has been recorded".to_string());
    }
    write_mode(scene, mode, author)
}

/// Write the mode object directly, without the "no step yet" guard
/// [`set_agent_ink_mode`] enforces for every other caller.
///
/// The one caller allowed to skip that guard is
/// [`super::promote::promote_to_alignment`]: promotion existing at all is
/// exactly what turns "change the mode of a timeline that already has
/// steps" from the flag flip the guard exists to block into a deliberate,
/// visible, costly act instead. Nothing else in this crate calls this.
pub(super) fn write_mode(
    scene: &mut Scene,
    mode: AgentInkMode,
    author: &Author,
) -> Result<(), AtlasError> {
    let mut ids = mode_object_ids(scene)?;
    if ids.len() > 1 {
        return Err("the document contains more than one agent-ink mode".to_string());
    }
    let props = vec![(K_MODE, PropValue::Str(mode.as_str().to_string()))];
    if let Some(id) = ids.pop() {
        scene
            .set_props(&ObjectId::from(id), &props)
            .map_err(|error| format!("could not update the agent-ink mode: {error}"))?;
    } else {
        scene
            .create_object_with_props(KIND_MODE, author.clone(), &props)
            .map_err(|error| format!("could not create the agent-ink mode: {error}"))?;
    }
    Ok(())
}

/// Record that promotion happened: from which mode, and (via `created_by`)
/// by whom. Refused if a promotion record already exists — a document is
/// promoted at most once, since [`super::promote::promote_to_alignment`]
/// itself refuses when the document is already in alignment mode.
pub(super) fn write_promotion_record(
    scene: &mut Scene,
    promoted_from: AgentInkMode,
    author: &Author,
) -> Result<(), AtlasError> {
    if !promotion_object_ids(scene)?.is_empty() {
        return Err("the document already carries an agent-ink promotion record".to_string());
    }
    let props = vec![(
        K_PROMOTED_FROM,
        PropValue::Str(promoted_from.as_str().to_string()),
    )];
    scene
        .create_object_with_props(KIND_PROMOTION, author.clone(), &props)
        .map_err(|error| format!("could not record the agent-ink promotion: {error}"))?;
    Ok(())
}

fn mode_object_ids(scene: &Scene) -> Result<Vec<String>, AtlasError> {
    object_ids_of_kind(scene, KIND_MODE)
}

fn promotion_object_ids(scene: &Scene) -> Result<Vec<String>, AtlasError> {
    object_ids_of_kind(scene, KIND_PROMOTION)
}

fn object_ids_of_kind(scene: &Scene, expected: &str) -> Result<Vec<String>, AtlasError> {
    let doc = scene.doc();
    let objects = doc.get_or_insert_map("objects");
    let txn = doc
        .try_transact()
        .map_err(|error| format!("atlas is busy: {error}"))?;
    let mut ids = Vec::new();
    for (id, value) in objects.iter(&txn) {
        let Out::YMap(map) = value else { continue };
        let kind = match map.get(&txn, "kind") {
            Some(Out::Any(Any::String(value))) => value.to_string(),
            _ => String::new(),
        };
        if kind == expected {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ink::{create_relation, create_variable, record_current_step, solve};
    use crate::read;

    #[test]
    fn mode_defaults_to_learning_and_persists_once_set() {
        let mut scene = Scene::new();
        assert_eq!(
            read(&scene).expect("read fresh document").agent_ink_mode,
            None,
            "a document that never set a mode has no mode object yet"
        );

        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Agent)
            .expect("set alignment mode before any step exists");
        let atlas = read(&scene).expect("read after setting mode");
        assert_eq!(atlas.agent_ink_mode, Some(AgentInkMode::Alignment));
    }

    #[test]
    fn mode_change_is_refused_once_a_step_is_recorded() {
        let mut scene = Scene::new();
        create_variable(
            &mut scene,
            "step",
            0.0,
            "scrubbing",
            None,
            None,
            &Author::Agent,
        )
        .expect("create step index");
        create_variable(
            &mut scene,
            "position",
            0.0,
            "free",
            None,
            None,
            &Author::Agent,
        )
        .expect("create solved position");
        create_relation(
            &mut scene,
            "position follows step",
            "equal",
            &["position", "step"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");
        solve(&mut scene, &Author::Agent).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Agent).expect("record step zero");

        let error = set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Agent)
            .expect_err("mode change must be refused once a step is recorded");
        assert!(
            error.contains("cannot change once a step has been recorded"),
            "{error}"
        );
        eprintln!("M0_MODE_LOCK error={error:?}");
    }
}
