use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{AgentInkMode, Measure, SolveStatus, StoredStep, VarState, Variable};
use crate::Atlas;

/// One agent-ink variable in the between-read digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariableDigest {
    pub name: String,
    pub value: f64,
    pub state: VarState,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub touched_by: String,
    pub represents: Option<(String, String)>,
    pub unit: Option<String>,
    /// Whether `represents` currently names a real shape or node property.
    /// Always `false` when `represents` is `None`.
    pub represents_live: bool,
    /// What this variable measures about the drawing, if anything — see
    /// `measure.rs`.
    pub measure: Option<Measure>,
    /// Whether `measure` currently resolves against real geometry. Always
    /// `false` when `measure` is `None`.
    pub measured_live: bool,
}

pub(crate) fn digest_variables(variables: &[Variable]) -> Vec<VariableDigest> {
    variables
        .iter()
        .map(|variable| VariableDigest {
            name: variable.name.clone(),
            value: variable.value,
            state: variable.state,
            created_by: variable.created_by.clone(),
            touched_by: variable.touched_by.clone(),
            represents: variable.represents.clone(),
            unit: variable.unit.clone(),
            represents_live: variable.represents_live,
            measure: variable.measure.clone(),
            measured_live: variable.measured_live,
        })
        .collect()
}

/// The numeric read-back inserted between CONSTRAINTS and DRAWING.
pub(crate) fn describe(atlas: &Atlas) -> String {
    if atlas.variables.is_empty() {
        return String::new();
    }

    let mode = atlas.agent_ink_mode.unwrap_or_default();
    let mut out = String::from("\nVARIABLES\n");
    // The mode is what makes a recorded step a re-derivable frame or a
    // signed commitment; a reader must never have to guess which one this
    // document is keeping, so it always leads the section, not just when a
    // caller happens to ask. When the document was promoted rather than
    // born in its current mode, that fact rides along on the same line —
    // "alignment" alone does not say whether every step here was checked
    // from the start or only checked once, later, at promotion.
    match &atlas.agent_ink_promotion {
        Some(promotion) => out.push_str(&format!(
            "MODE: {} (promoted from {} by {})\n",
            mode.as_str(),
            promotion.promoted_from.as_str(),
            promotion.promoted_by
        )),
        None => out.push_str(&format!("MODE: {}\n", mode.as_str())),
    }
    // The mode line says which rules this document is under; this line says
    // what that means for the numbers that follow, so a reader who has not
    // memorized the distinction still cannot mistake one for the other. See
    // M5 in docs/design-agent-ink-modes.md.
    out.push_str(match mode {
        AgentInkMode::Learning => "READING: this is what I am showing you, not a settled record.\n",
        AgentInkMode::Alignment => {
            "READING: this is what we agreed, except where a value is marked current instead of agreed.\n"
        }
    });
    if !atlas.solve_state.is_projected() {
        out.push_str("SOLVE NOT RUN: these values have not been verified by a solve.\n");
    } else {
        match atlas.solve_state.status {
            SolveStatus::Converged => {}
            SolveStatus::Exhausted => out.push_str(&format!(
                "SOLVE EXHAUSTED after {} iterations: no converged values were committed; residual={}.\n",
                atlas.solve_state.iterations,
                number(atlas.solve_state.residual)
            )),
            SolveStatus::Relaxed => out.push_str(&format!(
                "SOLVE RELAXED after {} iterations: dropped relations: {}; residual={}.\n",
                atlas.solve_state.iterations,
                atlas.solve_state.dropped.join(", "),
                number(atlas.solve_state.residual)
            )),
        }
    }

    if let Some(fault) = &atlas.solve_state.divergence {
        out.push_str(&format!(
            "REPLICA DIVERGENCE: {} differs by {} (local {}, remote {}, tolerance {}). Values were not reconciled.\n",
            fault.variable,
            number(fault.delta),
            number(fault.local),
            number(fault.remote),
            number(fault.tolerance)
        ));
    }

    for variable in &atlas.variables {
        out.push_str(&format!(
            "- {}",
            variable_clause(variable, mode, atlas.agent_ink_committed.as_ref())
        ));
        if let Some((object, property)) = &variable.represents {
            if variable.represents_live {
                out.push_str(&format!(", represents {object}.{property}"));
            } else {
                out.push_str(&format!(
                    ", represents {object}.{property} (DANGLING: no such shape or node)"
                ));
            }
        }
        // A measured variable's number is a claim about the drawing, not a
        // claim someone made, and a reader must be able to tell the two
        // apart on sight — hence "measured" here rather than folding it
        // into the plain value the way a declared variable's number reads.
        // See `measure.rs`'s module doc comment.
        if let Some(measure) = &variable.measure {
            out.push_str(&format!(
                ", measured {} of {}",
                measure.kind.as_str(),
                measure.objects.join(", ")
            ));
            if !variable.measured_live {
                let reason = super::measure::compute(atlas, measure)
                    .err()
                    .unwrap_or_else(|| "no longer resolves against the drawing".to_string());
                out.push_str(&format!(" (DANGLING: {reason})"));
            }
        }
        if variable.measure.is_none()
            && variable.touched_by != variable.created_by
            && !variable.touched_by.is_empty()
        {
            out.push_str(&format!(
                ", LAST-EDITED-BY={} (changed after {} created it)",
                variable.touched_by, variable.created_by
            ));
        }
        out.push('\n');
    }

    // A `formula` relation is the whole point of this feature: a curve a
    // human can see and argue with, not one buried in a solver's internals.
    // `members[0]` is the dependent quantity by the same convention `linear`
    // uses (see `Relation`'s doc comment), so it reads left-of-`=` here too.
    for relation in &atlas.relations {
        if relation.op != "formula" {
            continue;
        }
        let expression = relation.expression.as_deref().unwrap_or("");
        out.push_str(&format!(
            "- formula {:?}: {} = {expression}\n",
            relation.name, relation.members[0]
        ));
    }

    // Every relation created through `create_relation` already passed
    // `relation::dimension_check` before it was ever written (see that
    // function's doc comment for exactly what each op requires), so this
    // loop finds nothing to say about a document built entirely through
    // this crate's own API. It exists for the relation that did not go
    // through that gate: one restored from a document written before this
    // check existed, or one whose member later gained an incompatible unit
    // through some other write path. A reader is never left to guess that
    // a diagram's numbers no longer mean what its picture implies.
    for relation in &atlas.relations {
        if let Err(problem) = super::relation::dimension_check(
            atlas,
            &relation.name,
            &relation.op,
            &relation.members,
            relation.expression.as_deref(),
        ) {
            out.push_str(&format!(
                "- relation {:?} ({}): DIMENSION MISMATCH: {problem}\n",
                relation.name, relation.op
            ));
        }
    }
    out
}

/// One natural-language line for a numeric step between two reads.
pub(crate) fn describe_changes(before: &[VariableDigest], after: &[VariableDigest]) -> Vec<String> {
    if before.is_empty() && after.is_empty() {
        return Vec::new();
    }

    let before_by_name = before
        .iter()
        .map(|variable| (variable.name.as_str(), variable))
        .collect::<BTreeMap<_, _>>();
    let after_by_name = after
        .iter()
        .map(|variable| (variable.name.as_str(), variable))
        .collect::<BTreeMap<_, _>>();
    let mut changed = Vec::new();
    let mut held = Vec::new();

    for variable in after {
        match before_by_name.get(variable.name.as_str()) {
            None => changed.push(format!(
                "the {} appeared at {}, state={}",
                variable.name,
                value_with_unit(variable.value, variable.unit.as_deref()),
                variable.state.as_str()
            )),
            Some(old) if old.value.to_bits() != variable.value.to_bits() => {
                changed.push(value_change(old, variable));
            }
            Some(old) if old.state != variable.state => changed.push(format!(
                "the {} changed from state={} to state={} at {}",
                variable.name,
                old.state.as_str(),
                variable.state.as_str(),
                value_with_unit(variable.value, variable.unit.as_deref())
            )),
            Some(_) => held.push(format!(
                "the {} held at {}",
                variable.name,
                value_with_unit(variable.value, variable.unit.as_deref())
            )),
        }
    }
    for variable in before {
        if !after_by_name.contains_key(variable.name.as_str()) {
            changed.push(format!("the {} is no longer present", variable.name));
        }
    }

    if changed.is_empty() {
        return Vec::new();
    }
    changed.extend(held);
    vec![format!("- {}", changed.join("; "))]
}

fn value_change(before: &VariableDigest, after: &VariableDigest) -> String {
    let from = value_with_unit(before.value, before.unit.as_deref());
    let to = value_with_unit(after.value, after.unit.as_deref());
    let amount = value_with_unit(
        (after.value - before.value).abs(),
        matching_unit(before, after),
    );
    let lower_name = after.name.to_ascii_lowercase();
    let (subject, verb) = if let Some(subject) = lower_name.strip_suffix(" length") {
        let subject = if subject.is_empty() {
            after.name.as_str()
        } else {
            &after.name[..subject.len()]
        };
        (
            subject,
            if after.value > before.value {
                "lengthened"
            } else {
                "shortened"
            },
        )
    } else if lower_name == "step" || lower_name.ends_with(" step") {
        (
            after.name.as_str(),
            if after.value > before.value {
                "advanced"
            } else {
                "moved back"
            },
        )
    } else {
        (
            after.name.as_str(),
            if after.value > before.value {
                "increased"
            } else {
                "decreased"
            },
        )
    };
    format!("the {subject} {verb} from {from} to {to} (by {amount})")
}

fn matching_unit<'a>(before: &'a VariableDigest, after: &'a VariableDigest) -> Option<&'a str> {
    match (before.unit.as_deref(), after.unit.as_deref()) {
        (Some(before), Some(after)) if before == after => Some(after),
        _ => None,
    }
}

/// The opening clause of one variable's read-back line — everything up to
/// (not including) the `represents`/`measured` suffixes, which stay
/// mode-independent.
///
/// A measured variable reports the drawing rather than a claim anyone made
/// (see `measure.rs`'s module doc comment), and that is true regardless of
/// mode: it is neither a teaching illustration nor a commitment, so its
/// verb is "observed" in both voices rather than "shown" or "agreed". Every
/// other variable gets the mode's actual voice: learning always says it is
/// shown, alignment says "agreed" only when the live value matches the
/// latest converged step and "current" otherwise — a value nobody has
/// recorded into a step is not something anyone agreed to, it merely
/// happens to be what the document holds right now.
fn variable_clause(
    variable: &Variable,
    mode: AgentInkMode,
    committed: Option<&StoredStep>,
) -> String {
    let value = value_with_unit(variable.value, variable.unit.as_deref());
    let state = state_description(variable.state);
    let name = &variable.name;
    if variable.measure.is_some() {
        return format!("{name} is observed at {value}, state={state}");
    }
    match mode {
        AgentInkMode::Learning => format!("{name} is shown at {value}, state={state}"),
        AgentInkMode::Alignment if committed_matches(committed, variable) => {
            format!("{name} is agreed at {value}, state={state}")
        }
        AgentInkMode::Alignment => {
            format!("{name} is currently {value}, not yet recorded into a step, state={state}")
        }
    }
}

/// Whether `variable`'s live value is exactly what the reference step
/// (see [`super::step::latest_committed_step`]) stored for that name. A
/// variable absent from that step — one added since the last recording — is
/// not a match either, for the same reason: nobody has agreed to it yet.
fn committed_matches(committed: Option<&StoredStep>, variable: &Variable) -> bool {
    committed.is_some_and(|step| {
        step.values.iter().any(|value| {
            value.name == variable.name && value.value.to_bits() == variable.value.to_bits()
        })
    })
}

fn state_description(state: VarState) -> &'static str {
    match state {
        VarState::Pinned => "pinned",
        VarState::Scrubbing => "scrubbing (gesture-owned)",
        VarState::Free => "free (solver-owned)",
        VarState::Derived => "derived (follows its linear relation)",
        VarState::Measured => "measured (reports the drawing)",
    }
}

fn value_with_unit(value: f64, unit: Option<&str>) -> String {
    match unit {
        Some(unit) => format!("{} {unit}", number(value)),
        None => number(value),
    }
}

fn number(value: f64) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use ag_ui_canvas::scene::{Author, PropValue, Scene};

    use super::*;
    use crate::agent_ink::{
        advance_step, create_relation, create_variable, record_current_step, set_agent_ink_mode,
        solve, solve_with_options, AgentInkMode, SolveOptions, KIND_RELATION,
    };
    use crate::{describe_changes_for, read, ShapePatch, RELATIONS};

    /// A real shape for a `represents` binding to name. `represents_object`
    /// now has to resolve to something on the atlas the moment the variable
    /// is created, so a fixture that only ever put a literal string like
    /// `"swing-arm"` in the field no longer creates a variable at all.
    fn a_shape(scene: &mut Scene) -> String {
        crate::place_shape(
            scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(10.0),
                h: Some(10.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("create shape for a represents binding")
    }

    fn variable(
        scene: &mut Scene,
        name: &str,
        value: f64,
        state: &str,
        represents: Option<(&str, &str)>,
        unit: Option<&str>,
    ) {
        create_variable(scene, name, value, state, represents, unit, &Author::Agent)
            .expect("create variable");
    }

    fn variables_section(text: &str) -> &str {
        text.split_once("\nVARIABLES\n")
            .map(|(_, section)| section)
            .expect("VARIABLES section")
    }

    #[test]
    fn solved_document_describes_names_values_states_units_and_representation() {
        let mut scene = Scene::new();
        let swing_arm = a_shape(&mut scene);
        variable(&mut scene, "base", 50.0, "pinned", None, Some("mm"));
        variable(
            &mut scene,
            "swing arm length",
            120.0,
            "free",
            Some((&swing_arm, "w")),
            Some("mm"),
        );
        variable(&mut scene, "target", 120.0, "pinned", None, Some("mm"));
        create_relation(
            &mut scene,
            "arm meets target",
            "equal",
            &["swing arm length", "target"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");
        solve(&mut scene, &Author::Agent).expect("solve variables");

        let text = read(&scene).expect("read solved variables").describe();
        let section = variables_section(&text);
        // Learning mode's voice: every value is shown, never agreed or
        // current, because a learning document makes no commitments. See
        // M5 in docs/design-agent-ink-modes.md.
        assert!(
            section.contains("- base is shown at 50 mm, state=pinned\n"),
            "{text}"
        );
        assert!(
            section.contains(&format!(
                "- swing arm length is shown at 120 mm, state=free (solver-owned), represents {swing_arm}.w\n"
            )),
            "{text}"
        );
        assert!(
            section.contains("- target is shown at 120 mm, state=pinned\n"),
            "{text}"
        );
        eprintln!("B3_VARIABLES_START\nVARIABLES\n{section}B3_VARIABLES_END");
    }

    #[test]
    fn variable_edits_keep_the_causal_author_through_solve_and_geometry() {
        let mut scene = Scene::new();
        let final_right = Author::Named("final-right".to_string());
        let bar = crate::place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(9.0),
                h: Some(20.0),
                ..Default::default()
            },
            &final_right,
        )
        .expect("create a bound bar");
        create_variable(
            &mut scene,
            "likelihood ratio",
            9.0,
            "pinned",
            None,
            None,
            &final_right,
        )
        .expect("create the human-owned input");
        create_variable(
            &mut scene,
            "posterior bar width",
            9.0,
            "free",
            Some((&bar, "w")),
            None,
            &final_right,
        )
        .expect("create the solver-owned output");
        create_relation(
            &mut scene,
            "bar follows likelihood",
            "equal",
            &["posterior bar width", "likelihood ratio"],
            None,
            None,
            None,
            &final_right,
        )
        .expect("relate the input and output");
        solve(&mut scene, &final_right).expect("solve the initial agent-authored state");
        let before = read(&scene).expect("read the baseline").digest();

        crate::agent_ink::set_variable_value(&scene, "likelihood ratio", 8.0, &Author::Human)
            .expect("write the browser input as the human");
        solve(&mut scene, &Author::Human).expect("propagate the human input");

        let after = read(&scene).expect("read the human-authored result");
        assert!(
            after
                .variables
                .iter()
                .all(|variable| variable.touched_by == "human"),
            "{:#?}",
            after.variables
        );
        assert_eq!(after.shape(&bar).expect("bound bar").touched_by, "human");
        assert!(
            after
                .describe()
                .contains("LAST-EDITED-BY=human (changed after final-right created it)"),
            "{}",
            after.describe()
        );

        let delta = describe_changes_for(&before, &after.digest(), "provenance-reader");
        assert!(
            delta.contains("what the human did while you were thinking"),
            "{delta}"
        );
        assert!(
            !delta.contains("these are writes by final-right — not the human"),
            "{delta}"
        );
        assert_eq!(
            describe_changes_for(&before, &after.digest(), "human"),
            "",
            "the browser must not be shown its own variable write as news"
        );

        let human_state = after.digest();
        crate::agent_ink::set_variable_value(&scene, "likelihood ratio", 7.0, &final_right)
            .expect("write the negative control as the agent");
        solve(&mut scene, &final_right).expect("propagate the agent input");
        let agent_state = read(&scene)
            .expect("read the agent-authored result")
            .digest();
        let agent_delta = describe_changes_for(&human_state, &agent_state, "provenance-reader");
        assert!(
            agent_delta.contains("these are writes by final-right — not the human"),
            "{agent_delta}"
        );
        assert_eq!(
            describe_changes_for(&human_state, &agent_state, "final-right"),
            "",
            "the agent must not be shown its own variable write as news"
        );
    }

    /// A binding that resolved cleanly at creation can still go dangling
    /// later — here, by the bound shape being deleted out from under it. The
    /// read-back must say so, distinctly from a binding that is still
    /// driving geometry, so a reader never mistakes one for the other.
    #[test]
    fn a_binding_whose_object_is_deleted_reads_back_as_dangling() {
        use ag_ui_canvas::ids::ObjectId;

        let mut scene = Scene::new();
        let swing_arm = a_shape(&mut scene);
        variable(
            &mut scene,
            "swing arm length",
            120.0,
            "free",
            Some((&swing_arm, "w")),
            Some("mm"),
        );
        let live = read(&scene).expect("read while the shape still exists");
        assert!(
            live.variables[0].represents_live,
            "the binding must be live while its shape still exists"
        );
        assert!(!live.describe().contains("DANGLING"), "{}", live.describe());

        scene
            .delete_object(&ObjectId::from(swing_arm.as_str()))
            .expect("delete the bound shape");
        let dangling = read(&scene).expect("read after the shape is deleted");
        assert!(
            !dangling.variables[0].represents_live,
            "the binding must go dangling once its shape is gone"
        );
        let text = dangling.describe();
        let section = variables_section(&text);
        assert!(
            section.contains(&format!(
                "represents {swing_arm}.w (DANGLING: no such shape or node)"
            )),
            "{text}"
        );
    }

    #[test]
    fn exhausted_and_relaxed_statuses_lead_the_variable_list() {
        let mut exhausted = Scene::new();
        variable(&mut exhausted, "free", 100.0, "free", None, None);
        variable(&mut exhausted, "target", 0.0, "pinned", None, None);
        create_relation(
            &mut exhausted,
            "meet target",
            "equal",
            &["free", "target"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create exhausting relation");
        solve_with_options(
            &mut exhausted,
            SolveOptions {
                max_iterations: 0,
                ..SolveOptions::default()
            },
            &Author::Agent,
        )
        .expect("record exhaustion");
        let exhausted_text = read(&exhausted).expect("read exhaustion").describe();
        let exhausted_section = variables_section(&exhausted_text);
        // The READING line (M5's voice) is new and sits between MODE and the
        // failure line; the failure line's own wording is untouched, and it
        // still leads straight into the variable list, which is this test's
        // actual intent.
        assert!(
            exhausted_section.starts_with(
                "MODE: learning\nREADING: this is what I am showing you, not a settled record.\n\
                 SOLVE EXHAUSTED after 0 iterations: no converged values were committed; residual=10000.\n- free is shown at"
            ),
            "{exhausted_text}"
        );

        let mut relaxed = Scene::new();
        variable(&mut relaxed, "held", 0.0, "pinned", None, None);
        variable(&mut relaxed, "ten", 10.0, "pinned", None, None);
        variable(&mut relaxed, "zero", 0.0, "pinned", None, None);
        create_relation(
            &mut relaxed,
            "keep-zero",
            "equal",
            &["held", "zero"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create satisfied relation");
        create_relation(
            &mut relaxed,
            "drop-ten",
            "equal",
            &["held", "ten"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create conflicting relation");
        solve(&mut relaxed, &Author::Agent).expect("record relaxation");
        let relaxed_text = read(&relaxed).expect("read relaxation").describe();
        let relaxed_section = variables_section(&relaxed_text);
        assert!(
            relaxed_section.starts_with(
                "MODE: learning\nREADING: this is what I am showing you, not a settled record.\n\
                 SOLVE RELAXED after 0 iterations: dropped relations: drop-ten; residual=0.\n- held is shown at"
            ),
            "{relaxed_text}"
        );
        eprintln!(
            "B3_NON_CONVERGED exhausted_first={} relaxed_first={} dropped=drop-ten",
            exhausted_section.lines().next().unwrap_or_default(),
            relaxed_section.lines().next().unwrap_or_default()
        );
    }

    #[test]
    fn real_step_advance_describes_motion_amount_and_held_values() {
        let mut scene = Scene::new();
        let swing_arm = a_shape(&mut scene);
        variable(&mut scene, "base", 50.0, "pinned", None, Some("mm"));
        variable(&mut scene, "step", 0.0, "scrubbing", None, None);
        variable(
            &mut scene,
            "swing arm length",
            0.0,
            "free",
            Some((&swing_arm, "w")),
            Some("mm"),
        );
        create_relation(
            &mut scene,
            "arm follows step",
            "linear",
            &["swing arm length", "step"],
            Some(20.0),
            Some(120.0),
            None,
            &Author::Agent,
        )
        .expect("create step relation");
        solve(&mut scene, &Author::Agent).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Agent).expect("store step zero");
        let before = read(&scene).expect("read step zero").digest();

        advance_step(&mut scene, "step", &Author::Agent).expect("advance to step one");
        let after = read(&scene).expect("read step one").digest();
        let delta = describe_changes_for(&before, &after, "observer");
        let expected = "- the step advanced from 0 to 1 (by 1); the swing arm lengthened from 120 mm to 140 mm (by 20 mm); the base held at 50 mm";
        assert!(delta.contains(expected), "{delta}");
        assert_eq!(
            describe_changes_for(&before, &after, "agent"),
            "",
            "the author must not be shown its own step advance as news"
        );
        eprintln!("B3_DELTA {expected}");
    }

    #[test]
    fn documents_without_variables_add_no_agent_ink_text() {
        assert_eq!(describe(&Atlas::default()), "");
    }

    #[test]
    fn the_mode_leads_the_variables_section_so_neither_seat_has_to_guess_it() {
        let mut learning = Scene::new();
        variable(&mut learning, "base", 1.0, "pinned", None, None);
        let learning_text = read(&learning).expect("read learning document").describe();
        assert!(
            variables_section(&learning_text).starts_with("MODE: learning\n"),
            "{learning_text}"
        );

        let mut alignment = Scene::new();
        variable(&mut alignment, "base", 1.0, "pinned", None, None);
        set_agent_ink_mode(&mut alignment, AgentInkMode::Alignment, &Author::Agent)
            .expect("set alignment mode before any step exists");
        let alignment_text = read(&alignment)
            .expect("read alignment document")
            .describe();
        assert!(
            variables_section(&alignment_text).starts_with("MODE: alignment\n"),
            "{alignment_text}"
        );
        eprintln!("M0_READBACK learning={learning_text:?} alignment={alignment_text:?}");
    }

    /// M5, requirement 1: the same values, read in each mode, must produce
    /// materially different prose, not just a different MODE line — a
    /// reader who skips that one line still cannot mistake one for the
    /// other.
    #[test]
    fn the_same_values_read_differently_in_each_mode() {
        let mut learning = Scene::new();
        variable(&mut learning, "base", 50.0, "pinned", None, Some("mm"));
        let learning_text = read(&learning).expect("read learning document").describe();

        let mut alignment = Scene::new();
        variable(&mut alignment, "base", 50.0, "pinned", None, Some("mm"));
        set_agent_ink_mode(&mut alignment, AgentInkMode::Alignment, &Author::Agent)
            .expect("set alignment mode before any step exists");
        let alignment_text = read(&alignment)
            .expect("read alignment document")
            .describe();

        assert_ne!(
            learning_text, alignment_text,
            "{learning_text} vs {alignment_text}"
        );
        assert!(
            variables_section(&learning_text).contains("- base is shown at 50 mm, state=pinned\n"),
            "{learning_text}"
        );
        assert!(
            variables_section(&alignment_text).contains(
                "- base is currently 50 mm, not yet recorded into a step, state=pinned\n"
            ),
            "{alignment_text}"
        );
    }

    /// M5, requirement 2: a value sitting in an alignment-mode document that
    /// nobody has recorded into a step is current, not agreed. Saying
    /// "agreed" over it would be a lie about who committed to what.
    #[test]
    fn an_uncommitted_value_in_alignment_mode_is_not_described_as_agreed() {
        let mut scene = Scene::new();
        variable(&mut scene, "width", 40.0, "pinned", None, Some("mm"));
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Agent)
            .expect("set alignment mode before any step exists");

        let text = read(&scene)
            .expect("read before any step is recorded")
            .describe();
        let section = variables_section(&text);
        assert!(
            section.contains(
                "- width is currently 40 mm, not yet recorded into a step, state=pinned\n"
            ),
            "{text}"
        );
        assert!(!text.contains("is agreed"), "{text}");
    }

    /// M5, requirement 3: once a step is recorded and its solve converges,
    /// the value that step captured reads back as agreed, not merely
    /// current — this is the whole point of the distinction.
    #[test]
    fn a_recorded_converged_step_is_described_as_agreed() {
        let mut scene = Scene::new();
        variable(&mut scene, "width", 40.0, "pinned", None, Some("mm"));
        variable(&mut scene, "index", 0.0, "pinned", None, None);
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Agent)
            .expect("set alignment mode before any step exists");
        solve(&mut scene, &Author::Agent).expect("solve so a step can be recorded");
        record_current_step(&mut scene, "index", &Author::Agent).expect("record step zero");

        let text = read(&scene).expect("read after recording").describe();
        let section = variables_section(&text);
        assert!(
            section.contains("- width is agreed at 40 mm, state=pinned\n"),
            "{text}"
        );
        assert!(
            section.contains("- index is agreed at 0, state=pinned\n"),
            "{text}"
        );

        // Move the pinned value without recording a new step: the document
        // now holds something nobody has agreed to yet, and the voice must
        // say so instead of continuing to call the old number agreed.
        crate::agent_ink::set_variable_value(&scene, "width", 55.0, &Author::Human)
            .expect("change width without recording a new step");
        let moved_text = read(&scene)
            .expect("read after an uncommitted edit")
            .describe();
        let moved_section = variables_section(&moved_text);
        assert!(
            moved_section.contains(
                "- width is currently 55 mm, not yet recorded into a step, state=pinned, \
                 LAST-EDITED-BY=human (changed after agent created it)\n"
            ),
            "{moved_text}"
        );
        assert!(!moved_text.contains("width is agreed"), "{moved_text}");
    }

    /// M5, requirement 4: every failure line — SOLVE EXHAUSTED, SOLVE
    /// RELAXED (with its dropped-relation names), and REPLICA DIVERGENCE —
    /// prints in exactly the same words in both modes. The voice changes
    /// what a value claims, never whether a failure is reported; a teaching
    /// document has nobody else checking it, so honesty there is the whole
    /// point (see docs/design-agent-ink-modes.md, "Relaxed is not
    /// unverified").
    #[test]
    fn every_failure_line_is_worded_identically_in_both_modes() {
        fn failure_lines(mode: AgentInkMode) -> (String, String) {
            let mut exhausted = Scene::new();
            variable(&mut exhausted, "free", 100.0, "free", None, None);
            variable(&mut exhausted, "target", 0.0, "pinned", None, None);
            if mode == AgentInkMode::Alignment {
                set_agent_ink_mode(&mut exhausted, mode, &Author::Agent)
                    .expect("set mode before any step exists");
            }
            create_relation(
                &mut exhausted,
                "meet target",
                "equal",
                &["free", "target"],
                None,
                None,
                None,
                &Author::Agent,
            )
            .expect("create exhausting relation");
            solve_with_options(
                &mut exhausted,
                SolveOptions {
                    max_iterations: 0,
                    ..SolveOptions::default()
                },
                &Author::Agent,
            )
            .expect("record exhaustion");
            let exhausted_text = read(&exhausted).expect("read exhaustion").describe();
            let exhausted_line = exhausted_text
                .lines()
                .find(|line| line.starts_with("SOLVE EXHAUSTED"))
                .expect("an exhausted line")
                .to_string();

            let mut relaxed = Scene::new();
            variable(&mut relaxed, "held", 0.0, "pinned", None, None);
            variable(&mut relaxed, "ten", 10.0, "pinned", None, None);
            variable(&mut relaxed, "zero", 0.0, "pinned", None, None);
            if mode == AgentInkMode::Alignment {
                set_agent_ink_mode(&mut relaxed, mode, &Author::Agent)
                    .expect("set mode before any step exists");
            }
            create_relation(
                &mut relaxed,
                "keep-zero",
                "equal",
                &["held", "zero"],
                None,
                None,
                None,
                &Author::Agent,
            )
            .expect("create satisfied relation");
            create_relation(
                &mut relaxed,
                "drop-ten",
                "equal",
                &["held", "ten"],
                None,
                None,
                None,
                &Author::Agent,
            )
            .expect("create conflicting relation");
            solve(&mut relaxed, &Author::Agent).expect("record relaxation");
            let relaxed_text = read(&relaxed).expect("read relaxation").describe();
            let relaxed_line = relaxed_text
                .lines()
                .find(|line| line.starts_with("SOLVE RELAXED"))
                .expect("a relaxed line")
                .to_string();

            (exhausted_line, relaxed_line)
        }

        let (learning_exhausted, learning_relaxed) = failure_lines(AgentInkMode::Learning);
        let (alignment_exhausted, alignment_relaxed) = failure_lines(AgentInkMode::Alignment);
        assert_eq!(learning_exhausted, alignment_exhausted);
        assert_eq!(learning_relaxed, alignment_relaxed);
    }

    #[test]
    fn reading_relation_vocabulary_is_closed() {
        assert_eq!(
            RELATIONS,
            [
                "encloses",
                "crosses-out",
                "points-at",
                "starts-at",
                "connects",
                "says",
                "free",
            ]
        );
    }

    /// The whole point of a `formula` relation is that a human can read the
    /// model and argue with it, so the read-back must show the actual
    /// expression text, not just note that a relation exists.
    #[test]
    fn a_formula_relation_shows_its_expression_in_the_readback() {
        let mut scene = Scene::new();
        variable(&mut scene, "x", 0.0, "pinned", None, None);
        variable(&mut scene, "decay", 1.0, "free", None, None);
        create_relation(
            &mut scene,
            "decay follows x",
            "formula",
            &["decay", "x"],
            None,
            None,
            Some("exp(-2*x)"),
            &Author::Agent,
        )
        .expect("create formula relation");

        let text = read(&scene).expect("read formula document").describe();
        let section = variables_section(&text);
        assert!(
            section.contains("- formula \"decay follows x\": decay = exp(-2*x)\n"),
            "{text}"
        );
    }

    /// A backtick-quoted, spaced member name must reach the read-back
    /// exactly as it was written — no re-serialization is allowed to drop,
    /// re-space, or re-quote it, since the whole point is that a human can
    /// compare the printed model against the expression they authored.
    #[test]
    fn a_quoted_spaced_variable_name_round_trips_unchanged_through_the_readback() {
        let mut scene = Scene::new();
        variable(&mut scene, "barrier width", 10.0, "pinned", None, None);
        variable(&mut scene, "transmitted amplitude", 0.0, "free", None, None);
        create_relation(
            &mut scene,
            "amplitude decays exponentially with barrier width",
            "formula",
            &["transmitted amplitude", "barrier width"],
            None,
            None,
            Some("120 * exp(-0.03 * `barrier width`)"),
            &Author::Agent,
        )
        .expect("create formula relation with a spaced variable name");

        let text = read(&scene).expect("read formula document").describe();
        let section = variables_section(&text);
        assert!(
            section.contains(
                "- formula \"amplitude decays exponentially with barrier width\": \
                 transmitted amplitude = 120 * exp(-0.03 * `barrier width`)\n"
            ),
            "{text}"
        );
    }

    /// `create_relation` refuses a dimension mismatch outright, so the only
    /// way one can ever reach `describe()` is a relation written some other
    /// way — standing in here for one restored from a document that
    /// predates the dimension check, or hand-edited outside this crate's
    /// API. The read-back must say so plainly rather than silently keep
    /// printing numbers that no longer mean what the picture implies.
    #[test]
    fn a_dimension_mismatch_that_predates_this_check_is_reported_at_read_time() {
        let mut scene = Scene::new();
        variable(&mut scene, "length", 10.0, "pinned", None, Some("nm"));
        variable(&mut scene, "pixels", 20.0, "pinned", None, Some("px"));
        scene
            .create_object_with_props(
                KIND_RELATION,
                Author::Agent,
                &[
                    ("name", PropValue::Str("legacy mismatch".to_string())),
                    ("op", PropValue::Str("equal".to_string())),
                    ("members", PropValue::Str("length,pixels".to_string())),
                ],
            )
            .expect("write legacy relation object directly, bypassing dimension_check");

        let text = read(&scene)
            .expect("a pre-existing mismatch must not block loading")
            .describe();
        assert!(text.contains("DIMENSION MISMATCH"), "{text}");
        assert!(text.contains("legacy mismatch"), "{text}");
        assert!(text.contains("nm"), "{text}");
        assert!(text.contains("px"), "{text}");
    }
}
