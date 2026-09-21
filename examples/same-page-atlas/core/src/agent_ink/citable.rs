//! Turning a recorded step into something outside the document can cite.
//!
//! [`mode`]'s module doc comment already draws the line: a learning-mode step
//! is a re-derivable frame, free to change, while an alignment-mode step is a
//! commitment. That immutability is the whole reason alignment mode costs
//! more to use than learning mode — but right now nothing collects on the
//! promise. A recorded alignment step is immutable and attributed, and that
//! is *all* it is: nothing outside this document can point at it, check it,
//! or build a governance decision on top of it. This module is what closes
//! that gap.
//!
//! A citable step needs an identity that survives reload and can be checked,
//! not merely pointed at. Three candidates were on the table: the CRDT
//! object id, the commit cursor the step was written under, and a content
//! hash over what was actually stored. The first two only ever name a
//! *location* — a caller has to trust that whatever currently lives at that
//! location is still what was cited, which is exactly the kind of trust an
//! immutable record is supposed to make unnecessary. A content hash is
//! checkable instead of merely locatable: an outside caller who was handed a
//! `citable_id` can re-derive it from a fresh export and confirm the cited
//! step has not moved, without needing to trust this document's bookkeeping
//! at all. So [`CitableStep::citable_id`] is a sha256 over exactly the bytes
//! this document actually persists for the step — [`StoredStep`], the same
//! struct [`super::step::record_current_step`] already treats as the
//! immutable unit — plus the mode, so a citation is unambiguous about which
//! regime produced it even though (per [`mode`]'s lock-once rule) every step
//! in one document always shares the same mode.

use ag_ui_canvas::scene::Scene;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::mode::AgentInkMode;
use super::solve::{DivergenceFault, SolveStatus};
use super::step::{step_recorded_by, stored_step, StoredStep};
use super::variable::VarState;
use crate::{read, AtlasError};

/// One variable's value inside a citable step, joined with the document-wide
/// facts about it (`unit`, `state`) that [`StoredStep`] itself does not carry
/// — a variable's unit and ownership are set once at
/// [`super::variable::create_variable`] time and nothing in this crate ever
/// changes them afterward, so joining against the *current* atlas is honest
/// rather than a guess about what they were when the step was recorded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitableVariable {
    pub name: String,
    pub value: f64,
    pub unit: Option<String>,
    pub state: VarState,
}

/// One alignment-mode step, exported in the shape an outside governance
/// layer would need to cite it: which values were committed, under which
/// solve outcome, by whom, bound to a checkable identity.
///
/// A step whose solve did not converge exports exactly the same as one that
/// did — `status`/`dropped`/`divergence` carry that fact loudly rather than
/// this type filtering unconverged steps out. Hiding an unsettled decision
/// behind a successful-looking export would defeat the entire point of
/// making a step citable: the reason to cite it is so a governance layer can
/// tell a settled decision from an unsettled one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CitableStep {
    /// A sha256 hex digest over the mode and the exact [`StoredStep`] bytes.
    /// See the module doc comment for why a content hash, not an id or a
    /// cursor, is the honest identity here.
    pub citable_id: String,
    pub mode: AgentInkMode,
    pub index: u32,
    /// The byline that first recorded this step's object — see
    /// [`step_recorded_by`] for why this can never be whoever most recently
    /// replayed it.
    pub recorded_by: String,
    pub variables: Vec<CitableVariable>,
    pub status: SolveStatus,
    pub residual: f64,
    pub iterations: u32,
    pub dropped: Vec<String>,
    pub divergence: Option<DivergenceFault>,
    /// The mode this document was promoted from, when it was promoted
    /// rather than born in alignment mode — see
    /// [`super::mode::PromotionRecord`]. `None` for a document that has
    /// always been alignment-mode: a citation has to be able to tell those
    /// two histories apart, not just report that the step is alignment-mode
    /// now.
    pub promoted_from: Option<AgentInkMode>,
    /// Who promoted the document, mirroring `promoted_from`. Distinct from
    /// `recorded_by`: that names whoever first drew this step as a
    /// learning-mode frame, this names whoever later stood behind the whole
    /// timeline becoming a signed record.
    pub promoted_by: Option<String>,
}

/// Export step `index` as a citable alignment-mode record.
///
/// Refused outright when the document is in learning mode: a learning-mode
/// step is a sketch that re-derives on every advance, and letting it export
/// through the same path as a signed alignment step would let a sketch
/// launder itself into a record without anyone ever committing to it. The
/// intended path for a learning-mode timeline is
/// [`super::promote::promote_to_alignment`] — not a relaxation of this
/// check.
pub fn citable_step(scene: &Scene, index: u32) -> Result<CitableStep, AtlasError> {
    let atlas = read(scene)?;
    let mode = atlas.agent_ink_mode.unwrap_or_default();
    if mode != AgentInkMode::Alignment {
        return Err(format!(
            "step {index} cannot be exported as a citable commitment: the document \
             is in learning mode, where a step is a re-derivable frame that is free \
             to change, not a signed record. Promoting a learning-mode timeline into \
             alignment mode first is the intended path here — call promote_to_alignment \
             (the host's atlas_mode_promote_to_alignment tool) to re-solve and \
             re-record every step under alignment semantics before exporting it."
        ));
    }

    let stored = stored_step(scene, index)?;
    let recorded_by = step_recorded_by(scene, index)?;

    // `restore_snapshot` runs the same check before replaying a step's
    // values into live objects; this export runs it before reading them back
    // out, for the same reason — a variable added or removed since this step
    // was recorded means there is no honest way to join today's units and
    // ownership states onto yesterday's stored values.
    let current_names = atlas
        .variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect::<Vec<_>>();
    let stored_names = stored
        .values
        .iter()
        .map(|value| value.name.as_str())
        .collect::<Vec<_>>();
    if current_names != stored_names {
        return Err(format!(
            "stored step {index} variable names do not match the current document; \
             stored={stored_names:?}, current={current_names:?}; cannot export units \
             and ownership state for a variable set that has since changed"
        ));
    }

    let variables = atlas
        .variables
        .iter()
        .zip(&stored.values)
        .map(|(variable, value)| CitableVariable {
            name: variable.name.clone(),
            value: value.value,
            unit: variable.unit.clone(),
            state: variable.state,
        })
        .collect();

    let citable_id = citable_identity(mode, &stored);

    Ok(CitableStep {
        citable_id,
        mode,
        index: stored.index,
        recorded_by,
        variables,
        status: stored.state.status,
        residual: stored.state.residual,
        iterations: stored.state.iterations,
        dropped: stored.state.dropped.clone(),
        divergence: stored.state.divergence.clone(),
        promoted_from: atlas
            .agent_ink_promotion
            .as_ref()
            .map(|promotion| promotion.promoted_from),
        promoted_by: atlas
            .agent_ink_promotion
            .as_ref()
            .map(|promotion| promotion.promoted_by.clone()),
    })
}

/// Hash exactly the bytes this document persists for the step: the mode plus
/// the whole [`StoredStep`] — index, every variable's exact value bits (via
/// `f64`'s deterministic JSON round trip), and the full solve outcome
/// including any divergence fault. Two exports of the same immutable step,
/// from two different processes or after a save/reload, serialize identical
/// bytes and therefore hash identical; changing anything about the step (its
/// values, its status, a dropped relation, the fault it recorded) changes the
/// bytes and therefore the hash.
fn citable_identity(mode: AgentInkMode, stored: &StoredStep) -> String {
    let bytes = serde_json::to_vec(&(mode, stored))
        .expect("AgentInkMode and StoredStep always serialize to JSON");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ink::{
        advance_step_with_options, create_relation, create_variable, record_current_step,
        set_agent_ink_mode, solve, SolveOptions,
    };
    use ag_ui_canvas::scene::Author;

    /// A minimal alignment-mode timeline with one recorded step: a pinned
    /// "width" and a derived "height" tied together by "equal", the same
    /// shape `step.rs`'s own alignment fixture uses.
    fn alignment_timeline() -> Scene {
        let mut scene = Scene::new();
        create_variable(
            &mut scene,
            "width",
            10.0,
            "pinned",
            None,
            None,
            &Author::Human,
        )
        .expect("create pinned width");
        create_variable(
            &mut scene,
            "height",
            0.0,
            "free",
            None,
            None,
            &Author::Human,
        )
        .expect("create free height");
        create_relation(
            &mut scene,
            "width equals height",
            "equal",
            &["width", "height"],
            None,
            None,
            None,
            &Author::Human,
        )
        .expect("create equal relation");
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Human)
            .expect("set alignment mode before any step exists");
        solve(&mut scene, &Author::Human).expect("solve the fixture");
        record_current_step(&mut scene, "width", &Author::Human).expect("record step 10");
        scene
    }

    #[test]
    fn citable_id_is_unchanged_across_a_save_and_reload() {
        let scene = alignment_timeline();
        let before = citable_step(&scene, 10).expect("export before reload");

        let encoded = scene.encode_full().expect("encode the timeline");
        let reloaded = Scene::from_state(&encoded).expect("reload the timeline");
        let after = citable_step(&reloaded, 10).expect("export after reload");

        assert_eq!(
            before.citable_id, after.citable_id,
            "the same immutable step must export the same citable_id after a save \
             and reload, since nothing about its stored content changed"
        );
        eprintln!("CITABLE_STABLE id={:?}", before.citable_id);
    }

    #[test]
    fn citable_id_changes_when_the_stored_step_would_change() {
        let scene = alignment_timeline();
        let original = citable_step(&scene, 10).expect("export the original step");

        // A second, independent alignment timeline whose only difference is
        // the pinned value recorded at step 10 — everything else about the
        // fixture (names, units, mode, relation) is identical.
        let mut scene = Scene::new();
        create_variable(
            &mut scene,
            "width",
            20.0,
            "pinned",
            None,
            None,
            &Author::Human,
        )
        .expect("create pinned width");
        create_variable(
            &mut scene,
            "height",
            0.0,
            "free",
            None,
            None,
            &Author::Human,
        )
        .expect("create free height");
        create_relation(
            &mut scene,
            "width equals height",
            "equal",
            &["width", "height"],
            None,
            None,
            None,
            &Author::Human,
        )
        .expect("create equal relation");
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Human)
            .expect("set alignment mode before any step exists");
        solve(&mut scene, &Author::Human).expect("solve the fixture");
        record_current_step(&mut scene, "width", &Author::Human).expect("record step 20");
        let changed = citable_step(&scene, 20).expect("export the changed step");

        assert_ne!(
            original.citable_id, changed.citable_id,
            "a different pinned value recorded for the step must bind to a \
             different identity, or the identity is not actually checking content"
        );
        eprintln!(
            "CITABLE_BINDING original={:?} changed={:?}",
            original.citable_id, changed.citable_id
        );
    }

    #[test]
    fn learning_mode_step_is_refused_for_export() {
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
        // Learning is the default mode, so this timeline never calls
        // `set_agent_ink_mode` at all — the refusal must hold even when no
        // mode object has ever been projected, not only when one explicitly
        // says "learning".
        solve(&mut scene, &Author::Agent).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Agent).expect("record step zero");

        let error = citable_step(&scene, 0).expect_err("a learning-mode step must refuse export");
        assert!(
            error.contains("learning mode") && error.contains("promote_to_alignment"),
            "{error}"
        );
        eprintln!("CITABLE_LEARNING_REFUSED error={error:?}");
    }

    /// Step 0 starts step=position=0, which already satisfies "equal" before
    /// a single iteration runs, so exhaustion has to be forced on the step
    /// *after* it — the same reason `step.rs`'s own
    /// `exhausted_status_survives_scrub` advances first and only then
    /// starves the solver, rather than starving the initial solve.
    #[test]
    fn exhausted_solve_exports_and_says_so() {
        let mut scene = Scene::new();
        create_variable(
            &mut scene,
            "step",
            0.0,
            "scrubbing",
            None,
            None,
            &Author::Human,
        )
        .expect("create step index");
        create_variable(
            &mut scene,
            "position",
            0.0,
            "free",
            None,
            None,
            &Author::Human,
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
            &Author::Human,
        )
        .expect("create relation");
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Human)
            .expect("set alignment mode before any step exists");
        solve(&mut scene, &Author::Human).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Human).expect("record step zero");

        let exhausted = advance_step_with_options(
            &mut scene,
            "step",
            SolveOptions {
                max_iterations: 0,
                ..SolveOptions::default()
            },
            &Author::Human,
        )
        .expect("record the exhausted step");
        assert_eq!(exhausted.state.status, SolveStatus::Exhausted);

        let exported =
            citable_step(&scene, exhausted.index).expect("an exhausted step must still export");
        assert_eq!(
            exported.status,
            SolveStatus::Exhausted,
            "the export must carry the real exhausted status, not silently pass as settled"
        );
        eprintln!(
            "CITABLE_EXHAUSTED status={:?} id={:?}",
            exported.status, exported.citable_id
        );
    }
}
