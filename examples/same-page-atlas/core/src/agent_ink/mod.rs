//! Persisted numeric variables for agent ink.
//!
//! Variable names are document-wide identities. Authoring trims the name,
//! rejects an empty result, and rejects an exact duplicate of the persisted
//! UTF-8 string. Projection applies the same rule, so independently-created
//! duplicates that meet during a CRDT merge fail closed instead of producing
//! ambiguous read-back. Names remain case-sensitive.

mod citable;
mod dimension;
pub mod divergence_probe;
mod formula;
mod geometry;
mod measure;
mod mode;
mod promote;
#[path = "describe.rs"]
mod read_back;
mod relation;
mod solve;
mod step;
mod variable;

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{PropValue, Scene};
use yrs::{Any, Map, Out, Transact};

use crate::{Atlas, AtlasError};
pub use citable::{citable_step, CitableStep, CitableVariable};
pub use geometry::{capture_represented_property, REPRESENTED_PROPERTIES};
pub use measure::{create_measured_variable, Measure, MeasureKind, MEASURE_KINDS};
pub use mode::{set_agent_ink_mode, AgentInkMode, PromotionRecord};
pub use promote::{promote_to_alignment, PromotedStep, PromotionReport};
pub use read_back::VariableDigest;
pub use relation::{create_relation, Relation, KIND_RELATION, RELATION_OPS};
pub use solve::{
    report_replica_solution, set_variable_value, solve, solve_with_options, solved_values,
    DivergenceFault, SolveOptions, SolveState, SolveStatus, SolvedValue, DIVERGENCE_TOLERANCE,
};
pub use step::{
    advance_step, advance_step_with_options, record_current_step, scrub_step, stored_step,
    StepSolveState, StoredStep, KIND_STEP, MAX_STORED_STEPS,
};
pub use variable::{create_variable, VarState, Variable, VAR_STATES};

/// Resolve the one variable the shared page exposes as its timeline index.
///
/// `scrubbing` is the semantic marker used by both the browser control and
/// the agent tools. The variable's display name is not part of the protocol.
/// A merged document with two such variables is ambiguous and fails closed
/// instead of letting each surface silently choose a different timeline.
pub fn step_index_variable(atlas: &Atlas) -> Result<Option<&Variable>, AtlasError> {
    let mut candidates = atlas
        .variables
        .iter()
        .filter(|variable| variable.state == VarState::Scrubbing);
    let first = candidates.next();
    if let Some(second) = candidates.next() {
        return Err(format!(
            "the document has more than one scrubbing variable ({:?} and {:?}); the timeline index is ambiguous",
            first.map(|variable| variable.name.as_str()).unwrap_or(""),
            second.name
        ));
    }
    Ok(first)
}

/// Project one object owned by agent ink.
///
/// B2 extends this dispatch with relation projection without another edit to
/// the atlas's central object loop.
pub(crate) fn project_object(
    kind: &str,
    string: &impl Fn(&str) -> String,
    number: &impl Fn(&str) -> Option<f64>,
    atlas: &mut Atlas,
) -> Result<bool, AtlasError> {
    match kind {
        crate::KIND_VARIABLE => {
            let variable = variable::project(string, number)?;
            match atlas
                .variables
                .binary_search_by(|existing| existing.name.cmp(&variable.name))
            {
                Ok(_) => Err(format!(
                    "variable name {:?} already exists; variable names must be unique",
                    variable.name
                )),
                Err(index) => {
                    atlas.variables.insert(index, variable);
                    Ok(true)
                }
            }
        }
        relation::KIND_RELATION => {
            let relation = relation::project(string, number)?;
            match atlas
                .relations
                .binary_search_by(|existing| existing.name.cmp(&relation.name))
            {
                Ok(_) => Err(format!(
                    "relation name {:?} already exists; relation names must be unique",
                    relation.name
                )),
                Err(index) => {
                    atlas.relations.insert(index, relation);
                    Ok(true)
                }
            }
        }
        solve::KIND_SOLVE_STATE => {
            if atlas.solve_state.is_projected() {
                return Err("the document contains more than one agent-ink solve state".to_string());
            }
            atlas.solve_state = solve::project_state(string, number)?;
            Ok(true)
        }
        mode::KIND_MODE => {
            if atlas.agent_ink_mode.is_some() {
                return Err("the document contains more than one agent-ink mode".to_string());
            }
            atlas.agent_ink_mode = Some(mode::project_mode(string)?);
            Ok(true)
        }
        mode::KIND_PROMOTION => {
            if atlas.agent_ink_promotion.is_some() {
                return Err(
                    "the document contains more than one agent-ink promotion record".to_string(),
                );
            }
            atlas.agent_ink_promotion = Some(mode::project_promotion(string)?);
            Ok(true)
        }
        step::KIND_STEP => {
            // Collected here, in the same pass and the same already-open
            // transaction as everything else, so `finalize_committed_step`
            // can work out which step backs an "agreed" reading without
            // opening a second transaction on the same document (see that
            // function's doc comment for why a second one hangs rather than
            // erring). `step_entries` re-parses these same objects through
            // its own transaction for the handful of callers that run
            // outside of `read`'s loop; this is that same duplicate-index
            // guard, applied here instead.
            let candidate = step::project(string, number)?;
            if atlas
                .agent_ink_steps
                .iter()
                .any(|existing| existing.index == candidate.index)
            {
                return Err(format!(
                    "the document contains more than one agent-ink step {}",
                    candidate.index
                ));
            }
            atlas.agent_ink_steps.push(candidate);
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn describe(atlas: &Atlas) -> String {
    read_back::describe(atlas)
}

pub(crate) use read_back::{describe_changes, digest_variables};

/// Mark every variable's binding live or dangling against the atlas that was
/// just finished projecting. [`crate::read`] calls this once, after its main
/// object loop, so a binding is judged against the whole document rather
/// than however much of it had been visited when the variable itself came up
/// in CRDT iteration order.
pub(crate) use geometry::finalize_bindings;

/// Recompute every measured variable's value from the same just-assembled
/// atlas, right after `finalize_bindings` — see `measure.rs`'s module doc
/// comment for why a measured variable is refreshed here instead of on
/// every write to the object it measures.
pub(crate) use measure::finalize_measurements;

/// Work out which stored step, if any, backs the document's current live
/// values, so `describe` can tell alignment mode's "this is what we agreed"
/// from "this is only current" (M5 in `docs/design-agent-ink-modes.md`).
///
/// Runs from `read`, after `finalize_measurements`, over `atlas`'s own
/// `agent_ink_steps` — the same `agent_ink_step` objects `project_object`
/// already collected in the same pass, under the transaction `read` still
/// holds open. An earlier version of this function re-read those objects
/// itself through a second `Scene` transaction; on the same document that
/// re-enters the first transaction's lock and hangs instead of erroring, so
/// this one takes only the already-projected `Atlas` and touches the scene
/// not at all, the same way `finalize_bindings` and `finalize_measurements`
/// do.
pub(crate) fn finalize_committed_step(atlas: &mut Atlas) {
    atlas.agent_ink_committed = step::latest_committed(&atlas.agent_ink_steps);
}

/// The object kinds this layer wrote before it was renamed from "living ink"
/// to "agent ink", paired with the kind each is now.
///
/// A document saved under the old name still carries objects tagged with the
/// left-hand strings. Rather than teach every kind comparison in this module a
/// second spelling, the host rewrites them once when it opens a saved
/// document ([`migrate_legacy_kinds`]), so everything downstream keeps
/// comparing against one name.
pub const LEGACY_KINDS: &[(&str, &str)] = &[
    ("living_ink_step", step::KIND_STEP),
    ("living_ink_solve_state", solve::KIND_SOLVE_STATE),
    ("living_ink_mode", mode::KIND_MODE),
    ("living_ink_promotion", mode::KIND_PROMOTION),
];

/// Rewrite every object carrying a pre-rename kind to its current kind and
/// return how many were rewritten. Safe to run on every open: a document with
/// no legacy objects is not written to at all.
pub fn migrate_legacy_kinds(scene: &Scene) -> Result<usize, AtlasError> {
    let mut pending: Vec<(String, &'static str)> = Vec::new();
    {
        let doc = scene.doc();
        let objects = doc.get_or_insert_map("objects");
        let txn = doc
            .try_transact()
            .map_err(|error| format!("atlas is busy: {error}"))?;
        for (id, value) in objects.iter(&txn) {
            let Out::YMap(map) = value else { continue };
            let Some(Out::Any(Any::String(kind))) = map.get(&txn, "kind") else {
                continue;
            };
            if let Some((_, current)) = LEGACY_KINDS
                .iter()
                .find(|(legacy, _)| *legacy == kind.as_ref())
            {
                pending.push((id.to_string(), current));
            }
        }
    }
    for (id, kind) in &pending {
        scene
            .set_prop(
                &ObjectId::from(id.as_str()),
                "kind",
                PropValue::Str((*kind).to_string()),
            )
            .map_err(|error| format!("could not migrate object {id} to kind {kind}: {error}"))?;
    }
    Ok(pending.len())
}

#[cfg(test)]
mod legacy_kind_tests {
    use super::*;
    use ag_ui_canvas::scene::Author;

    /// A document saved before the rename, with its mode, promotion record,
    /// solve state, and one stored step all tagged the old way. Written with
    /// the raw kind strings on purpose: the point is what a saved file holds,
    /// not what today's writers produce.
    fn legacy_document() -> Scene {
        let mut scene = Scene::new();
        scene
            .create_object_with_props(
                "living_ink_mode",
                Author::Agent,
                &[("mode", PropValue::Str("alignment".to_string()))],
            )
            .expect("legacy mode");
        scene
            .create_object_with_props(
                "living_ink_promotion",
                Author::Human,
                &[("promoted_from", PropValue::Str("learning".to_string()))],
            )
            .expect("legacy promotion");
        scene
    }

    #[test]
    fn legacy_kinds_are_rewritten_once_and_then_read_as_current() {
        let scene = legacy_document();

        // Before migration the projection sees none of it: the kinds are
        // foreign strings and `project_object` returns false for them.
        let before = crate::read(&scene).expect("read before");
        assert_eq!(before.agent_ink_mode, None);
        assert!(before.agent_ink_promotion.is_none());

        assert_eq!(migrate_legacy_kinds(&scene).expect("migrate"), 2);

        let after = crate::read(&scene).expect("read after");
        assert_eq!(after.agent_ink_mode, Some(AgentInkMode::Alignment));
        assert!(after.agent_ink_promotion.is_some());

        // Idempotent: nothing left to rewrite, and nothing is written.
        assert_eq!(migrate_legacy_kinds(&scene).expect("second migrate"), 0);
    }

    #[test]
    fn a_document_without_legacy_kinds_is_untouched() {
        let mut scene = Scene::new();
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Agent)
            .expect("current mode");
        assert_eq!(migrate_legacy_kinds(&scene).expect("migrate"), 0);
        let atlas = crate::read(&scene).expect("read");
        assert_eq!(atlas.agent_ink_mode, Some(AgentInkMode::Alignment));
    }
}
