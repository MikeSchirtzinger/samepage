//! Promoting a learning-mode timeline into an alignment-mode one.
//!
//! `mode.rs`'s module doc comment and `docs/design-agent-ink-modes.md`
//! already say what this has to be: one way, visible, and costly, never a
//! flag flip. Concretely, that means every stored step is genuinely
//! re-derived under alignment rules, not merely relabeled — a learning-mode
//! recording can hold values nothing ever actually checked (see
//! `step.rs`'s own account of `advance_step_inner`'s pre-mode-split bug,
//! where a pinned edit was discarded before the solver ever saw it), and
//! copying those bytes forward into an immutable, citable record would be
//! exactly the laundering the mode split exists to prevent.
//!
//! # The convergence policy
//!
//! Re-solving a stored step can come back [`SolveStatus::Exhausted`] or
//! [`SolveStatus::Relaxed`] instead of [`SolveStatus::Converged`] — both are
//! honest, recordable outcomes everywhere else in this crate (see
//! `step.rs`'s `validate_step_state`). Promotion treats them differently:
//! **any step that does not re-solve to `Converged` refuses the whole
//! promotion**, naming every failing step and its outcome, rather than
//! promoting the steps that did converge and silently dropping the rest.
//!
//! The reasoning is the same asymmetry `docs/design-agent-ink-modes.md`
//! spells out for learning mode's relaxed ceremony: a governed picture has
//! someone checking it by construction, so an unconverged step surviving
//! into an alignment timeline is not a quiet failure, it is a *signed*
//! commitment to a value nothing actually settled on. A half-promoted
//! timeline — three steps now citable, two silently left behind or, worse,
//! promoted anyway with an exhausted status baked in — is harder to catch
//! than an outright refusal, because it looks like the real thing until
//! someone reads the fine print on the exact step that matters. Refusing
//! outright keeps the failure where a human promoting the document cannot
//! miss it, and keeps "this document is in alignment mode" meaning "every
//! step in it was actually checked," full stop, with no per-step asterisk a
//! later reader has to go looking for.
//!
//! # What "re-solve" means here
//!
//! For each stored step, in index order: restore its own exact recorded
//! bits — every variable, pinned and solver-owned alike, byte for byte, via
//! [`super::step::restore_snapshot`] — and then run a fresh
//! [`super::solve::solve`] over that state. This checks the step's own
//! pinned/scrubbing values as a real commitment (exactly the alignment-mode
//! contract) rather than trusting whatever the learning-mode recording
//! happened to store for the free/derived side. The freshly solved values
//! and outcome, not the original ones, are what gets re-recorded.
//!
//! The live document is restored to exactly what it held before promotion
//! began once every step has been probed, regardless of outcome: promotion
//! inspects the *stored* timeline, not wherever the caller currently has it
//! scrubbed to, and a failed promotion must leave no trace.
//!
//! # Where the promotion is recorded
//!
//! [`super::mode::write_promotion_record`] stamps a document-wide
//! [`super::mode::PromotionRecord`] (`promoted_from`, plus `promoted_by` via
//! the same `created_by` attribution [`super::step::step_recorded_by`]
//! already relies on for steps) the moment promotion succeeds.
//! `describe.rs`'s read-back and `citable.rs`'s export both surface it, so a
//! reader of the finished document — whether reading `atlas_read` or citing
//! an individual step outside this document — can tell a timeline that was
//! always alignment-mode from one that became that way, and by whom.
//!
//! # Authorization lives at the host, not here
//!
//! This function takes `author: &Author` purely for attribution, the same
//! as every other agent-ink write in this crate — it does not itself check
//! *who* is allowed to call it. `docs/design-agent-ink-modes.md` insists
//! promotion is a human act, and that is enforced the way this codebase
//! enforces every other human/agent split: the host's tool declaration for
//! promotion is `.human_only()`, checked by `Caller::may_call` at dispatch,
//! before this function is ever reached. See `examples/same-page-atlas/src/atlas.rs`.
//!
//! # Going the other way
//!
//! `docs/design-agent-ink-modes.md` describes alignment-to-learning as a
//! copy that leaves the original record intact, not an in-place demotion.
//! That needs a second document to copy *into*, and this host manages
//! exactly one atlas per running process — there is no "create a sibling
//! document" operation anywhere in this crate or `examples/same-page-atlas`
//! to copy through. Building a real copy would mean inventing that
//! mechanism first, which is out of scope here; a demotion is not
//! implemented, and there is deliberately no in-place shortcut standing in
//! for it.

use std::collections::BTreeMap;

use ag_ui_canvas::scene::{Author, Scene};

use super::mode::{self, AgentInkMode};
use super::solve::{self, SolveStatus, SolvedValue};
use super::step::{self, StepSolveState, StoredStep};
use crate::{read, AtlasError};

/// One stored step's outcome once promotion re-solved it. Always
/// [`SolveStatus::Converged`] — see the module doc comment's convergence
/// policy for why a step that came back anything else never reaches this
/// type at all.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotedStep {
    pub index: u32,
    pub status: SolveStatus,
}

/// What happened when a learning-mode timeline was promoted into alignment
/// mode. Returned only on success — see [`promote_to_alignment`]'s
/// convergence policy for what a refusal reports instead.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionReport {
    /// The `created_by` byline the new document-wide promotion record was
    /// stamped with — the same string a reader gets back from
    /// [`super::mode::PromotionRecord::promoted_by`].
    pub promoted_by: String,
    /// Every step now stored under alignment semantics, in index order.
    pub steps: Vec<PromotedStep>,
}

/// Promote this document's agent-ink timeline from learning mode into
/// alignment mode. See the module doc comment for the full contract; in
/// short: every stored step is re-solved from its own recorded values and
/// re-recorded under alignment semantics, the whole promotion is refused
/// (naming every step that failed and why) if any step does not converge,
/// and a successful promotion leaves a permanent, readable record of itself.
pub fn promote_to_alignment(
    scene: &mut Scene,
    index_variable: &str,
    author: &Author,
) -> Result<PromotionReport, AtlasError> {
    let atlas = read(scene)?;
    let mode = atlas.agent_ink_mode.unwrap_or_default();
    if mode == AgentInkMode::Alignment {
        return Err(
            "the document is already in alignment mode; there is nothing to promote".to_string(),
        );
    }

    let entries = step::step_entries(scene)?;
    if entries.is_empty() {
        return Err(
            "promotion re-solves and re-records every stored step, and this document has \
             none yet; call atlas_mode_set directly instead, before any step is recorded"
                .to_string(),
        );
    }

    // Exactly what every variable and the solve state hold right now,
    // captured before any probing begins. `index` is unused by
    // `restore_snapshot` beyond bounds-checking (it only ever writes the
    // values in `values`), so 0 is fine here even though this is not really
    // "step 0" — this snapshot exists purely to put the live document back
    // where it started, not to be recorded as a step.
    let before = StoredStep {
        index: 0,
        values: atlas
            .variables
            .iter()
            .map(|variable| SolvedValue {
                name: variable.name.clone(),
                value: variable.value,
            })
            .collect(),
        state: StepSolveState::from(&atlas.solve_state),
    };

    let (promoted, failures, hard_error) =
        probe_every_step(scene, &entries, index_variable, author);

    // Always try to restore the pre-promotion state, no matter how probing
    // went: a failed promotion must leave no trace, and a successful one
    // still commits through the stored-step objects below, not by leaving
    // the canvas parked on whichever step it last probed.
    let restore_result = step::restore_snapshot(scene, &before, author);

    if let Some(error) = hard_error {
        return Err(match restore_result {
            Ok(()) => error,
            Err(rollback) => format!(
                "{error}; additionally, the live document could not be restored to its \
                 pre-promotion state: {rollback}"
            ),
        });
    }
    if let Err(rollback) = restore_result {
        return Err(format!(
            "promotion probing finished but the live document could not be restored to its \
             pre-promotion state: {rollback}"
        ));
    }
    if !failures.is_empty() {
        return Err(format!(
            "promotion refused: {} of {} stored steps could not be re-solved to convergence \
             under alignment semantics ({}). Promoting only the steps that converged would \
             record a half-checked timeline as though every step in it had been checked, so \
             nothing was promoted and the document remains in learning mode.",
            failures.len(),
            entries.len(),
            failures.join("; ")
        ));
    }

    for (object_id, snapshot) in &promoted {
        step::overwrite_stored_step(scene, object_id, snapshot)?;
    }
    // Ordered so the mode flip and the record of *why* the mode is now
    // different land together: by the time a reader can see `MODE:
    // alignment`, the promotion record explaining it already exists too.
    mode::write_mode(scene, AgentInkMode::Alignment, author)?;
    mode::write_promotion_record(scene, AgentInkMode::Learning, author)?;

    Ok(PromotionReport {
        promoted_by: author.as_str().to_string(),
        steps: promoted
            .into_iter()
            .map(|(_, snapshot)| PromotedStep {
                index: snapshot.index,
                status: snapshot.state.status,
            })
            .collect(),
    })
}

/// Restore and re-solve every stored step in index order, without mutating
/// anything a caller can observe on error: this only ever writes into the
/// live scene as scratch space for the probe, never into a persisted step
/// object. Returns the steps that converged (staged, not yet written), a
/// human-readable reason for every step that did not, and — separately —
/// the first hard [`AtlasError`] encountered, if re-solving itself failed
/// rather than merely disagreeing with convergence.
///
/// Split out of [`promote_to_alignment`] so that function can guarantee the
/// live-document restore always runs after this returns, on every exit path
/// including a hard error, instead of a `?` inside the loop skipping it.
fn probe_every_step(
    scene: &mut Scene,
    entries: &BTreeMap<u32, (String, StoredStep)>,
    index_variable: &str,
    author: &Author,
) -> (Vec<(String, StoredStep)>, Vec<String>, Option<AtlasError>) {
    let mut promoted = Vec::new();
    let mut failures = Vec::new();

    for (&index, (object_id, stored)) in entries {
        if let Err(error) = step::ensure_snapshot_index_variable(stored, index_variable) {
            return (promoted, failures, Some(error));
        }
        if let Err(error) = step::restore_snapshot(scene, stored, author) {
            return (
                promoted,
                failures,
                Some(format!("could not probe stored step {index}: {error}")),
            );
        }
        let solved = match solve::solve(scene, author) {
            Ok(solved) => solved,
            Err(error) => {
                return (
                    promoted,
                    failures,
                    Some(format!("could not re-solve stored step {index}: {error}")),
                )
            }
        };
        if solved.status != SolveStatus::Converged {
            let dropped = if solved.dropped.is_empty() {
                String::new()
            } else {
                format!(" (dropped: {})", solved.dropped.join(", "))
            };
            failures.push(format!(
                "step {index} re-solved to {}{dropped}",
                solved.status.as_str()
            ));
            continue;
        }
        let refreshed = match read(scene) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                return (
                    promoted,
                    failures,
                    Some(format!(
                        "could not read the document after re-solving step {index}: {error}"
                    )),
                )
            }
        };
        promoted.push((
            object_id.clone(),
            StoredStep {
                index,
                values: refreshed
                    .variables
                    .iter()
                    .map(|variable| SolvedValue {
                        name: variable.name.clone(),
                        value: variable.value,
                    })
                    .collect(),
                state: StepSolveState::from(&solved),
            },
        ));
    }

    (promoted, failures, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ink::{
        citable_step, create_relation, create_variable, record_current_step, set_agent_ink_mode,
        set_variable_value, solve as run_solve, stored_step,
    };

    /// A learning-mode timeline whose steps carry *distinct* pinned widths,
    /// built the way a learning-mode document actually can: by pointing the
    /// index at the next step and recording directly, rather than through
    /// `advance_step` (which would restore the previous step's pinned value
    /// first, per `step.rs`'s own module doc comment, discarding exactly the
    /// edit this fixture needs to keep). Each recorded step is independently
    /// `Converged`, so promotion is expected to succeed on this fixture.
    fn learning_timeline_with_pinned_edits() -> Scene {
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
            "width",
            10.0,
            "pinned",
            None,
            None,
            &Author::Agent,
        )
        .expect("create pinned width");
        create_variable(
            &mut scene,
            "height",
            0.0,
            "free",
            None,
            None,
            &Author::Agent,
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
            &Author::Agent,
        )
        .expect("create equal relation");
        run_solve(&mut scene, &Author::Agent).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Agent).expect("record step zero");

        for (index, width) in [(1u32, 20.0), (2, 30.0), (3, 40.0)] {
            set_variable_value(&scene, "step", f64::from(index), &Author::Agent)
                .expect("point at the next step");
            set_variable_value(&scene, "width", width, &Author::Agent).expect("pin the new width");
            run_solve(&mut scene, &Author::Agent).expect("solve with the new width");
            record_current_step(&mut scene, "step", &Author::Agent)
                .unwrap_or_else(|_| panic!("record step {index}"));
        }
        scene
    }

    #[test]
    fn promotion_resolves_and_rerecords_every_step_and_the_result_is_immutable_and_citable() {
        let mut scene = learning_timeline_with_pinned_edits();

        let report =
            promote_to_alignment(&mut scene, "step", &Author::Human).expect("promotion succeeds");
        assert_eq!(report.promoted_by, "human");
        assert_eq!(report.steps.len(), 4, "steps 0 through 3 all converge");
        assert!(
            report
                .steps
                .iter()
                .all(|step| step.status == SolveStatus::Converged),
            "{:?}",
            report.steps
        );

        let atlas = read(&scene).expect("read after promotion");
        assert_eq!(atlas.agent_ink_mode, Some(AgentInkMode::Alignment));
        let promotion = atlas
            .agent_ink_promotion
            .as_ref()
            .expect("a promotion record exists");
        assert_eq!(promotion.promoted_from, AgentInkMode::Learning);
        assert_eq!(promotion.promoted_by, "human");

        // Every step still holds its own width, not step 0's — promotion
        // re-solved from each step's own recorded commitment, it did not
        // copy learning-mode bytes forward.
        for (index, width) in [(0u32, 10.0), (1, 20.0), (2, 30.0), (3, 40.0)] {
            let stored = stored_step(&scene, index).expect("stored step");
            let named = |name: &str| {
                stored
                    .values
                    .iter()
                    .find(|value| value.name == name)
                    .unwrap_or_else(|| panic!("step {index} is missing {name:?}"))
                    .value
            };
            assert_eq!(named("width"), width, "step {index} keeps its own width");
            assert!((named("height") - width).abs() < 1.0e-6);
        }

        // Immutable: an alignment-mode step now refuses a re-record with a
        // different value, exactly as it does for a step that was never
        // promoted.
        set_variable_value(&scene, "width", 999.0, &Author::Agent).expect("pin a different width");
        set_variable_value(&scene, "step", 2.0, &Author::Agent).expect("point the index at step 2");
        run_solve(&mut scene, &Author::Human).expect("solve the different width");
        let overwrite = record_current_step(&mut scene, "step", &Author::Human)
            .expect_err("a promoted step must refuse a different re-record");
        assert!(
            overwrite.contains("already stored") && overwrite.contains("cannot be overwritten"),
            "{overwrite}"
        );

        // Exportable as citable, and the export itself says this document
        // was promoted, not born aligned.
        let citable = citable_step(&scene, 2).expect("a promoted step exports as citable");
        assert_eq!(citable.mode, AgentInkMode::Alignment);
        assert_eq!(citable.promoted_from, Some(AgentInkMode::Learning));
        assert_eq!(citable.promoted_by.as_deref(), Some("human"));
        eprintln!(
            "PROMOTE_OK steps={:?} citable_promoted_from={:?}",
            report.steps, citable.promoted_from
        );
    }

    /// A step whose two relations conflict — `held` is pinned, so nothing
    /// can move to satisfy both `held = zero` and `held = ten` at once — is
    /// the same fixture `describe.rs`'s own read-back test uses to reach
    /// [`SolveStatus::Relaxed`] deterministically (dropping one relation),
    /// with no dependence on iteration counts the way forcing
    /// [`SolveStatus::Exhausted`] would need. Recording it and then
    /// promoting re-solves the identical conflict, so the failure is
    /// reproduced rather than merely asserted.
    #[test]
    fn a_step_that_cannot_reconverge_refuses_the_whole_promotion_and_names_it() {
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
            "held",
            0.0,
            "pinned",
            None,
            None,
            &Author::Agent,
        )
        .expect("create pinned held");
        create_variable(
            &mut scene,
            "zero",
            0.0,
            "pinned",
            None,
            None,
            &Author::Agent,
        )
        .expect("create pinned zero");
        create_variable(
            &mut scene,
            "ten",
            10.0,
            "pinned",
            None,
            None,
            &Author::Agent,
        )
        .expect("create pinned ten");
        create_relation(
            &mut scene,
            "keep-zero",
            "equal",
            &["held", "zero"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create satisfiable relation");
        create_relation(
            &mut scene,
            "drop-ten",
            "equal",
            &["held", "ten"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create conflicting relation");
        let solved = run_solve(&mut scene, &Author::Agent).expect("solve the conflict");
        assert_eq!(
            solved.status,
            SolveStatus::Relaxed,
            "the fixture must not converge, or this test proves nothing"
        );
        record_current_step(&mut scene, "step", &Author::Agent).expect("record the relaxed step");

        let refused = promote_to_alignment(&mut scene, "step", &Author::Human)
            .expect_err("a relaxed re-solve must refuse the whole promotion");
        assert!(
            refused.contains("promotion refused") && refused.contains("1 of 1"),
            "{refused}"
        );
        assert!(
            refused.contains("step 0") && refused.contains("relaxed"),
            "the refusal must name the failing step and its outcome: {refused}"
        );

        // Refused as a whole: no step was re-recorded, and the document is
        // still in learning mode, unchanged. This fixture never called
        // `set_agent_ink_mode`, so the mode stays `None` (unset), which
        // every reader treats as learning per `AgentInkMode::default`.
        let atlas = read(&scene).expect("read after the refused promotion");
        assert_eq!(atlas.agent_ink_mode, None);
        assert!(atlas.agent_ink_promotion.is_none());
        let still_relaxed = stored_step(&scene, 0).expect("step 0 is still stored");
        assert_eq!(still_relaxed.state.status, SolveStatus::Relaxed);
        eprintln!("PROMOTE_REFUSED error={refused:?}");
    }

    #[test]
    fn promotion_is_not_reversible_by_flipping_the_mode_back() {
        let mut scene = learning_timeline_with_pinned_edits();
        promote_to_alignment(&mut scene, "step", &Author::Human).expect("promotion succeeds");
        assert_eq!(
            read(&scene).expect("read after promotion").agent_ink_mode,
            Some(AgentInkMode::Alignment)
        );

        // The only public door back to learning mode is `set_agent_ink_mode`,
        // and it refuses to change the mode of any document that already has
        // a recorded step — the exact same guard that made promotion
        // necessary in the first place, so it cannot be sidestepped by
        // simply asking for the opposite direction.
        let error = set_agent_ink_mode(&mut scene, AgentInkMode::Learning, &Author::Human)
            .expect_err("flipping a promoted document back to learning must be refused");
        assert!(
            error.contains("cannot change once a step has been recorded"),
            "{error}"
        );

        let atlas = read(&scene).expect("read after the refused flip-back");
        assert_eq!(atlas.agent_ink_mode, Some(AgentInkMode::Alignment));
        assert!(
            atlas.agent_ink_promotion.is_some(),
            "the promotion record survives the refused flip-back attempt"
        );
        eprintln!("PROMOTE_IRREVERSIBLE flip_back_error={error:?}");
    }
}
