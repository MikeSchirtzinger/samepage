use std::collections::{BTreeMap, BTreeSet};

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use yrs::{Any, Map, Out, Transact};

use super::mode::AgentInkMode;
use super::relation::variable_object_ids;
use super::solve::{
    solve, solve_with_options, DivergenceFault, SolveOptions, SolveState, SolveStatus, SolvedValue,
    DIVERGENCE_TOLERANCE, KIND_SOLVE_STATE,
};
use crate::{read, Atlas, AtlasError, VarState};

/// Persisted object kind for one solved point in an agent-ink timeline.
pub const KIND_STEP: &str = "agent_ink_step";

/// Step indices are bounded to 0 through 127.
///
/// The bound keeps browser-synchronized CRDT history finite. An advance from
/// step 127 is rejected before the index, solve state, or variable values are
/// changed.
pub const MAX_STORED_STEPS: u32 = 128;

const K_INDEX: &str = "index";
const K_NAMES: &str = "names";
const K_VALUES: &str = "values";
const K_STATUS: &str = "status";
const K_DROPPED: &str = "dropped";
const K_RESIDUAL_BITS: &str = "residual_bits";
const K_ITERATIONS: &str = "iterations";
const K_FAULT_VARIABLE: &str = "fault_variable";
const K_FAULT_LOCAL_BITS: &str = "fault_local_bits";
const K_FAULT_REMOTE_BITS: &str = "fault_remote_bits";
const K_FAULT_DELTA_BITS: &str = "fault_delta_bits";
const K_FAULT_TOLERANCE_BITS: &str = "fault_tolerance_bits";

/// The solve outcome originally recorded for one step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepSolveState {
    pub status: SolveStatus,
    pub dropped: Vec<String>,
    pub residual: f64,
    pub iterations: u32,
    pub divergence: Option<DivergenceFault>,
}

impl From<&SolveState> for StepSolveState {
    fn from(state: &SolveState) -> Self {
        Self {
            status: state.status,
            dropped: state.dropped.clone(),
            residual: state.residual,
            iterations: state.iterations,
            divergence: state.divergence.clone(),
        }
    }
}

/// Exact named values and solve outcome persisted for one step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredStep {
    pub index: u32,
    pub values: Vec<SolvedValue>,
    pub state: StepSolveState,
}

/// Store the current projected values and solve outcome under the current
/// value of `index_variable`.
///
/// The index is an ordinary pinned or scrubbing variable, so relations can
/// reference it. A recorded index is immutable. Re-recording the exact same
/// bits is idempotent, while a different value or status is rejected.
pub fn record_current_step(
    scene: &mut Scene,
    index_variable: &str,
    author: &Author,
) -> Result<StoredStep, AtlasError> {
    let atlas = read(scene)?;
    let mode = atlas.agent_ink_mode.unwrap_or_default();
    let index = current_step_index(&atlas, index_variable)?;
    if !atlas.solve_state.is_projected() {
        return Err("a step cannot be recorded before the document has a solve state".to_string());
    }
    let snapshot = StoredStep {
        index,
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
    validate_snapshot(&snapshot)?;

    let entries = step_entries(scene)?;
    if let Some((existing_id, existing)) = entries.get(&index) {
        if snapshots_have_same_bits(existing, &snapshot) {
            return Ok(existing.clone());
        }
        return match mode {
            // A step is a commitment in alignment mode, so an already-stored
            // step keeps refusing a different value or status exactly as it
            // always has: overwriting it silently would rewrite a decision
            // that was already signed off on.
            AgentInkMode::Alignment => Err(format!(
                "step {index} is already stored and cannot be overwritten with different values or status"
            )),
            // A step is a re-derivable frame in learning mode, so recording
            // it again with different values replaces what was there. This
            // is what lets scrubbing back and advancing down a different
            // path update a frame instead of freezing its first replay
            // forever.
            AgentInkMode::Learning => {
                let props = snapshot_props(&snapshot);
                scene
                    .set_props(&ObjectId::from(existing_id.clone()), &props)
                    .map_err(|error| {
                        format!("could not update agent-ink step {index}: {error}")
                    })?;
                Ok(snapshot)
            }
        };
    }

    let props = snapshot_props(&snapshot);
    scene
        .create_object_with_props(KIND_STEP, author.clone(), &props)
        .map_err(|error| format!("could not store agent-ink step {index}: {error}"))?;
    Ok(snapshot)
}

/// Overwrite an already-stored step's persisted bytes with `snapshot`,
/// without any of [`record_current_step`]'s mode-based immutability checks.
///
/// This exists for exactly one caller: [`super::promote::promote_to_alignment`].
/// Promotion re-solves every stored step under alignment semantics and has
/// to replace what a learning-mode recording put there with the freshly
/// checked result — the same raw overwrite `record_current_step`'s
/// learning-mode branch already performs on itself, just invoked once, for
/// every step, from outside the recording path, at the one moment the
/// design note carves out for it. Nothing else may call this: an
/// alignment-mode step stays immutable through every other path in this
/// module.
pub(super) fn overwrite_stored_step(
    scene: &mut Scene,
    existing_id: &str,
    snapshot: &StoredStep,
) -> Result<(), AtlasError> {
    validate_snapshot(snapshot)?;
    let props = snapshot_props(snapshot);
    scene
        .set_props(&ObjectId::from(existing_id.to_string()), &props)
        .map_err(|error| {
            format!(
                "could not overwrite agent-ink step {} during promotion: {error}",
                snapshot.index
            )
        })
}

/// Return one persisted step without changing the current scene.
pub fn stored_step(scene: &Scene, index: u32) -> Result<StoredStep, AtlasError> {
    validate_index(index)?;
    step_entries(scene)?
        .remove(&index)
        .map(|(_, step)| step)
        .ok_or_else(|| format!("agent-ink step {index} has not been stored"))
}

/// The highest-indexed stored step whose solve converged, if any, among
/// `steps` — the same `agent_ink_step` objects [`project`] already
/// collects into `Atlas::agent_ink_steps` during the ordinary projection
/// pass.
///
/// This is the read-back's reference point for what alignment mode's voice
/// can honestly call "agreed" (see M5 in
/// `docs/design-agent-ink-modes.md`). A step that exhausted or relaxed was
/// never checked, so it cannot back an "agreed" claim even though it is
/// stored; only `Converged` counts. `describe` compares each live variable
/// against this step's values rather than the document's whole history,
/// because a value that only matched a *superseded* commitment is not
/// currently agreed, it just happens to coincide with one.
///
/// Deliberately takes the already-projected slice rather than the `Scene`:
/// this runs from inside `crate::read`, which already holds the document's
/// one read transaction open for the whole projection pass, and opening a
/// second one here (as an earlier version of this function did, via
/// `step_entries`) re-enters the transaction lock and hangs rather than
/// erroring. `project` below is what feeds this from that same pass.
pub(super) fn latest_committed(steps: &[StoredStep]) -> Option<StoredStep> {
    steps
        .iter()
        .filter(|step| step.state.status == SolveStatus::Converged)
        .max_by_key(|step| step.index)
        .cloned()
}

/// Parse one `agent_ink_step` object during the main projection loop,
/// exactly the closures `project_object` already has in hand for every
/// other kind. This is the projection-time counterpart to [`step_entries`],
/// which re-reads every step object through its own transaction for the
/// handful of callers (`record_current_step`, `advance_step`, `scrub_step`,
/// `stored_step`, `step_recorded_by`) that run standalone, outside of
/// `read`'s loop, and so can safely open one.
pub(super) fn project(
    string: &impl Fn(&str) -> String,
    number: &impl Fn(&str) -> Option<f64>,
) -> Result<StoredStep, AtlasError> {
    parse_snapshot(string, number)
}

/// Who first recorded this step's persisted object.
///
/// This is `"created_by"`, the field `Scene::create_object_configured` stamps
/// at creation and refuses to accept as a caller-supplied prop (see its
/// reserved-field guard) — so it is exactly the author who first stored the
/// step, never whoever most recently replayed a value into it.
/// `record_current_step`'s learning-mode branch updates an existing step with
/// `set_props`, which only ever writes the flat keys `snapshot_props` builds,
/// so it cannot touch this field either. In alignment mode a step is
/// immutable once stored (see the module doc comment), so this is doubly
/// stable there: the object is never rewritten at all.
pub(super) fn step_recorded_by(scene: &Scene, index: u32) -> Result<String, AtlasError> {
    validate_index(index)?;
    let doc = scene.doc();
    let objects = doc.get_or_insert_map("objects");
    let txn = doc
        .try_transact()
        .map_err(|error| format!("atlas is busy: {error}"))?;
    for (_, value) in objects.iter(&txn) {
        let Out::YMap(map) = value else { continue };
        let string = |key: &str| match map.get(&txn, key) {
            Some(Out::Any(Any::String(value))) => value.to_string(),
            _ => String::new(),
        };
        let number = |key: &str| match map.get(&txn, key) {
            Some(Out::Any(Any::Number(value))) => Some(value),
            Some(Out::Any(Any::BigInt(value))) => Some(value as f64),
            _ => None,
        };
        if string("kind") != KIND_STEP {
            continue;
        }
        if number(K_INDEX).map(|value| value as u32) != Some(index) {
            continue;
        }
        return Ok(string("created_by"));
    }
    Err(format!("agent-ink step {index} has not been stored"))
}

/// Advance to the next new step using the previous stored values as the warm
/// start. If the next step already exists, advancing replays it instead of
/// solving it again.
pub fn advance_step(
    scene: &mut Scene,
    index_variable: &str,
    author: &Author,
) -> Result<StoredStep, AtlasError> {
    advance_step_inner(scene, index_variable, None, author)
}

/// Advance with an explicit solver budget. This is useful when a caller needs
/// an exhausted step to remain part of the ordered history.
pub fn advance_step_with_options(
    scene: &mut Scene,
    index_variable: &str,
    options: SolveOptions,
    author: &Author,
) -> Result<StoredStep, AtlasError> {
    advance_step_inner(scene, index_variable, Some(options), author)
}

/// Restore a persisted step byte for byte without invoking the solver.
pub fn scrub_step(
    scene: &mut Scene,
    index_variable: &str,
    index: u32,
    author: &Author,
) -> Result<StoredStep, AtlasError> {
    let snapshot = stored_step(scene, index)?;
    ensure_snapshot_index_variable(&snapshot, index_variable)?;
    restore_snapshot(scene, &snapshot, author)?;
    Ok(snapshot)
}

fn advance_step_inner(
    scene: &mut Scene,
    index_variable: &str,
    options: Option<SolveOptions>,
    author: &Author,
) -> Result<StoredStep, AtlasError> {
    let atlas = read(scene)?;
    let mode = atlas.agent_ink_mode.unwrap_or_default();
    let current_index = current_step_index(&atlas, index_variable)?;
    let next_index = current_index
        .checked_add(1)
        .ok_or_else(|| format!("agent-ink step index {current_index} cannot be incremented"))?;
    if next_index >= MAX_STORED_STEPS {
        return Err(format!(
            "agent-ink stores at most {MAX_STORED_STEPS} steps; step {next_index} is outside 0..{}",
            MAX_STORED_STEPS - 1
        ));
    }

    let entries = step_entries(scene)?;
    let previous = entries
        .get(&current_index)
        .map(|(_, step)| step.clone())
        .ok_or_else(|| {
            format!(
                "agent-ink step {current_index} must be recorded before advancing to step {next_index}"
            )
        })?;
    ensure_snapshot_index_variable(&previous, index_variable)?;
    if entries.contains_key(&next_index) {
        drop(entries);
        return scrub_step(scene, index_variable, next_index, author);
    }
    drop(entries);

    match mode {
        AgentInkMode::Learning => {
            // Replay the prior snapshot first. This makes its persisted
            // values, not any incidental current projection, the seed for
            // the next solve. A step here is a frame in an explanation, so
            // every advance re-derives cleanly from the index no matter
            // what a caller changed in the meantime.
            restore_snapshot(scene, &previous, author)?;
        }
        AgentInkMode::Alignment => {
            // A step here is a commitment, so only the solver-owned
            // (free/derived) variables are seeded from the previous step as
            // a warm start. Pinned and scrubbing variables are left at
            // whatever the caller currently holds them at, because that
            // live edit is the decision this step exists to record; a full
            // restore would discard it the instant before it could land.
            seed_solver_owned(scene, &atlas, &previous)?;
        }
    }
    if let Err(error) =
        super::solve::set_variable_value(scene, index_variable, f64::from(next_index), author)
    {
        return Err(format!(
            "could not advance agent-ink step index {index_variable:?}: {error}"
        ));
    }

    let solved = invoke_solve(scene, options, author);
    if let Err(error) = solved {
        return match restore_snapshot(scene, &previous, author) {
            Ok(()) => Err(format!("could not solve agent-ink step {next_index}: {error}")),
            Err(rollback) => Err(format!(
                "could not solve agent-ink step {next_index}: {error}; could not restore step {current_index}: {rollback}"
            )),
        };
    }

    match record_current_step(scene, index_variable, author) {
        Ok(step) => Ok(step),
        Err(error) => match restore_snapshot(scene, &previous, author) {
            Ok(()) => Err(format!("could not record agent-ink step {next_index}: {error}")),
            Err(rollback) => Err(format!(
                "could not record agent-ink step {next_index}: {error}; could not restore step {current_index}: {rollback}"
            )),
        },
    }
}

fn invoke_solve(
    scene: &mut Scene,
    options: Option<SolveOptions>,
    author: &Author,
) -> Result<SolveState, AtlasError> {
    #[cfg(test)]
    SOLVE_INVOCATIONS.with(|count| count.set(count.get() + 1));

    match options {
        Some(options) => solve_with_options(scene, options, author),
        None => solve(scene, author),
    }
}

fn current_step_index(atlas: &Atlas, index_variable: &str) -> Result<u32, AtlasError> {
    let variable = atlas
        .variables
        .iter()
        .find(|variable| variable.name == index_variable)
        .ok_or_else(|| format!("no step-index variable named {index_variable:?}"))?;
    if !matches!(variable.state, VarState::Pinned | VarState::Scrubbing) {
        return Err(format!(
            "step-index variable {index_variable:?} must be pinned or scrubbing; got {}",
            variable.state.as_str()
        ));
    }
    whole_step_index(variable.value, index_variable)
}

fn whole_step_index(value: f64, name: &str) -> Result<u32, AtlasError> {
    if !value.is_finite()
        || value < 0.0
        || value.fract() != 0.0
        || value >= f64::from(MAX_STORED_STEPS)
    {
        return Err(format!(
            "step-index variable {name:?} must be a whole number in 0..{}; got {value}",
            MAX_STORED_STEPS - 1
        ));
    }
    Ok(value as u32)
}

fn validate_index(index: u32) -> Result<(), AtlasError> {
    if index >= MAX_STORED_STEPS {
        return Err(format!(
            "agent-ink step {index} is outside the stored range 0..{}",
            MAX_STORED_STEPS - 1
        ));
    }
    Ok(())
}

pub(super) fn ensure_snapshot_index_variable(
    snapshot: &StoredStep,
    index_variable: &str,
) -> Result<(), AtlasError> {
    let value = snapshot
        .values
        .iter()
        .find(|value| value.name == index_variable)
        .ok_or_else(|| {
            format!(
                "stored step {} does not contain step-index variable {index_variable:?}",
                snapshot.index
            )
        })?;
    if value.value.to_bits() != f64::from(snapshot.index).to_bits() {
        return Err(format!(
            "stored step {} has step-index variable {index_variable:?} set to {}",
            snapshot.index, value.value
        ));
    }
    Ok(())
}

/// Restore every variable and the solve state to exactly what `snapshot`
/// holds, syncing bound geometry to match. `pub(super)` rather than private:
/// [`super::promote::promote_to_alignment`] reuses this both to seed each
/// stored step's re-solve and, with a synthetic before-promotion snapshot, to
/// put the live document back where it started once probing every step is
/// done.
pub(super) fn restore_snapshot(
    scene: &mut Scene,
    snapshot: &StoredStep,
    author: &Author,
) -> Result<(), AtlasError> {
    validate_snapshot(snapshot)?;
    let atlas = read(scene)?;
    let current_names = atlas
        .variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect::<Vec<_>>();
    let stored_names = snapshot
        .values
        .iter()
        .map(|value| value.name.as_str())
        .collect::<Vec<_>>();
    if current_names != stored_names {
        return Err(format!(
            "stored step {} variable names do not match the current document; stored={stored_names:?}, current={current_names:?}",
            snapshot.index
        ));
    }

    let mut ids = variable_object_ids(scene)?;
    let mut written = Vec::new();
    for (variable, stored) in atlas.variables.iter().zip(&snapshot.values) {
        if variable.value.to_bits() == stored.value.to_bits() {
            continue;
        }
        let id = ids
            .remove(&variable.name)
            .map(ObjectId::from)
            .ok_or_else(|| format!("variable {:?} has no persisted object", variable.name))?;
        if let Err(error) = scene.set_props(
            &id,
            &[
                ("value", PropValue::Num(stored.value)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        ) {
            rollback_values(scene, &written)?;
            return Err(format!(
                "could not restore variable {:?} from step {}: {error}",
                stored.name, snapshot.index
            ));
        }
        written.push((id, variable.value, variable.touched_by.clone()));
    }

    if let Err(error) = write_solve_state(scene, &snapshot.state, author) {
        rollback_values(scene, &written)?;
        return Err(format!(
            "could not restore solve state from step {}: {error}",
            snapshot.index
        ));
    }

    // Scrubbing the timeline has to move the actual shapes, not just the
    // panel's numbers, so every bound variable's restored value is written
    // straight into the object it represents. This covers both the direct
    // `scrub_step` replay and learning mode's warm-start restore inside
    // `advance_step_inner`; a solve immediately afterward would write the
    // same properties again, which is harmless.
    let restored_atlas = read(scene)?;
    super::geometry::sync_represented_geometry(scene, &restored_atlas, author)?;
    Ok(())
}

fn rollback_values(scene: &Scene, written: &[(ObjectId, f64, String)]) -> Result<(), AtlasError> {
    for (id, value, touched_by) in written.iter().rev() {
        scene
            .set_props(
                id,
                &[
                    ("value", PropValue::Num(*value)),
                    ("touched_by", PropValue::Str(touched_by.clone())),
                ],
            )
            .map_err(|error| format!("could not roll back an incomplete step replay: {error}"))?;
    }
    Ok(())
}

/// Alignment mode's per-step handoff: reset every solver-owned (free or
/// derived) variable to its value in the previous recorded step, giving the
/// solve about to run the same warm start [`restore_snapshot`] would give it
/// in learning mode, without touching a single pinned or scrubbing
/// variable. Those are left exactly where the caller put them, because that
/// live value is the commitment this step exists to record.
fn seed_solver_owned(
    scene: &mut Scene,
    current: &Atlas,
    previous: &StoredStep,
) -> Result<(), AtlasError> {
    let stored_by_name = previous
        .values
        .iter()
        .map(|value| (value.name.as_str(), value.value))
        .collect::<BTreeMap<_, _>>();
    let mut ids = variable_object_ids(scene)?;
    let mut written = Vec::new();
    for variable in &current.variables {
        if !matches!(variable.state, VarState::Free | VarState::Derived) {
            continue;
        }
        let stored_value = *stored_by_name.get(variable.name.as_str()).ok_or_else(|| {
            format!(
                "stored step {} does not contain variable {:?}",
                previous.index, variable.name
            )
        })?;
        let id = ids
            .remove(&variable.name)
            .map(ObjectId::from)
            .ok_or_else(|| format!("variable {:?} has no persisted object", variable.name))?;
        if let Err(error) = scene.set_prop(&id, "value", PropValue::Num(stored_value)) {
            rollback_values(scene, &written)?;
            return Err(format!(
                "could not seed variable {:?} from step {}: {error}",
                variable.name, previous.index
            ));
        }
        written.push((id, variable.value, variable.touched_by.clone()));
    }
    Ok(())
}

fn write_solve_state(
    scene: &mut Scene,
    state: &StepSolveState,
    author: &Author,
) -> Result<(), AtlasError> {
    validate_step_state(state)?;
    let mut ids = object_ids_of_kind(scene, KIND_SOLVE_STATE)?;
    if ids.len() > 1 {
        return Err("the document contains more than one agent-ink solve state".to_string());
    }
    let fault = state.divergence.as_ref();
    let props = vec![
        ("status", PropValue::Str(state.status.as_str().to_string())),
        ("dropped", PropValue::Str(state.dropped.join(","))),
        ("residual", PropValue::Num(state.residual)),
        ("iterations", PropValue::Num(f64::from(state.iterations))),
        (
            "fault_variable",
            PropValue::Str(
                fault
                    .map(|fault| fault.variable.clone())
                    .unwrap_or_default(),
            ),
        ),
        (
            "fault_local",
            PropValue::Num(fault.map(|fault| fault.local).unwrap_or(0.0)),
        ),
        (
            "fault_remote",
            PropValue::Num(fault.map(|fault| fault.remote).unwrap_or(0.0)),
        ),
        (
            "fault_delta",
            PropValue::Num(fault.map(|fault| fault.delta).unwrap_or(0.0)),
        ),
        (
            "fault_tolerance",
            PropValue::Num(
                fault
                    .map(|fault| fault.tolerance)
                    .unwrap_or(DIVERGENCE_TOLERANCE),
            ),
        ),
    ];
    if let Some(id) = ids.pop() {
        scene
            .set_props(&ObjectId::from(id), &props)
            .map_err(|error| format!("could not update agent-ink solve state: {error}"))?;
    } else {
        scene
            .create_object_with_props(KIND_SOLVE_STATE, author.clone(), &props)
            .map_err(|error| format!("could not create agent-ink solve state: {error}"))?;
    }
    Ok(())
}

fn snapshot_props(snapshot: &StoredStep) -> Vec<(&'static str, PropValue)> {
    let fault = snapshot.state.divergence.as_ref();
    vec![
        (K_INDEX, PropValue::Num(f64::from(snapshot.index))),
        (
            K_NAMES,
            PropValue::Str(
                snapshot
                    .values
                    .iter()
                    .map(|value| encode_name(&value.name))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ),
        (
            K_VALUES,
            PropValue::Str(
                snapshot
                    .values
                    .iter()
                    .map(|value| format!("{:016x}", value.value.to_bits()))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ),
        (
            K_STATUS,
            PropValue::Str(snapshot.state.status.as_str().to_string()),
        ),
        (K_DROPPED, PropValue::Str(snapshot.state.dropped.join(","))),
        (
            K_RESIDUAL_BITS,
            PropValue::Str(format!("{:016x}", snapshot.state.residual.to_bits())),
        ),
        (
            K_ITERATIONS,
            PropValue::Num(f64::from(snapshot.state.iterations)),
        ),
        (
            K_FAULT_VARIABLE,
            PropValue::Str(
                fault
                    .map(|fault| fault.variable.clone())
                    .unwrap_or_default(),
            ),
        ),
        (
            K_FAULT_LOCAL_BITS,
            PropValue::Str(format!(
                "{:016x}",
                fault.map(|fault| fault.local).unwrap_or(0.0).to_bits()
            )),
        ),
        (
            K_FAULT_REMOTE_BITS,
            PropValue::Str(format!(
                "{:016x}",
                fault.map(|fault| fault.remote).unwrap_or(0.0).to_bits()
            )),
        ),
        (
            K_FAULT_DELTA_BITS,
            PropValue::Str(format!(
                "{:016x}",
                fault.map(|fault| fault.delta).unwrap_or(0.0).to_bits()
            )),
        ),
        (
            K_FAULT_TOLERANCE_BITS,
            PropValue::Str(format!(
                "{:016x}",
                fault
                    .map(|fault| fault.tolerance)
                    .unwrap_or(DIVERGENCE_TOLERANCE)
                    .to_bits()
            )),
        ),
    ]
}

pub(super) fn step_entries(
    scene: &Scene,
) -> Result<BTreeMap<u32, (String, StoredStep)>, AtlasError> {
    let doc = scene.doc();
    let objects = doc.get_or_insert_map("objects");
    let txn = doc
        .try_transact()
        .map_err(|error| format!("atlas is busy: {error}"))?;
    let mut entries = BTreeMap::new();
    for (id, value) in objects.iter(&txn) {
        let Out::YMap(map) = value else { continue };
        let string = |key: &str| match map.get(&txn, key) {
            Some(Out::Any(Any::String(value))) => value.to_string(),
            _ => String::new(),
        };
        let number = |key: &str| match map.get(&txn, key) {
            Some(Out::Any(Any::Number(value))) => Some(value),
            Some(Out::Any(Any::BigInt(value))) => Some(value as f64),
            _ => None,
        };
        if string("kind") != KIND_STEP {
            continue;
        }
        let step = parse_snapshot(&string, &number)?;
        let index = step.index;
        if entries.insert(index, (id.to_string(), step)).is_some() {
            return Err(format!(
                "the document contains more than one agent-ink step {index}"
            ));
        }
    }
    Ok(entries)
}

fn parse_snapshot(
    string: &impl Fn(&str) -> String,
    number: &impl Fn(&str) -> Option<f64>,
) -> Result<StoredStep, AtlasError> {
    let index_number = number(K_INDEX)
        .filter(|value| value.is_finite() && *value >= 0.0 && value.fract() == 0.0)
        .ok_or_else(|| {
            "stored agent-ink step index is missing or not a whole number".to_string()
        })?;
    if index_number > f64::from(u32::MAX) {
        return Err("stored agent-ink step index does not fit in u32".to_string());
    }
    let index = index_number as u32;
    validate_index(index)?;

    let names = split_required(&string(K_NAMES), "stored step variable names")?
        .into_iter()
        .map(|name| decode_name(&name))
        .collect::<Result<Vec<_>, _>>()?;
    if names.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("stored step variable names must be unique and sorted".to_string());
    }
    let value_bits = split_required(&string(K_VALUES), "stored step values")?;
    if names.len() != value_bits.len() {
        return Err(format!(
            "stored step {index} has {} variable names and {} values",
            names.len(),
            value_bits.len()
        ));
    }
    let values = names
        .into_iter()
        .zip(value_bits)
        .map(|(name, bits)| {
            let value = f64::from_bits(parse_bits(&bits, "stored variable value")?);
            if !value.is_finite() {
                return Err(format!(
                    "stored step {index} value for variable {name:?} is not finite"
                ));
            }
            Ok(SolvedValue { name, value })
        })
        .collect::<Result<Vec<_>, AtlasError>>()?;

    let status = parse_status(&string(K_STATUS))?;
    let dropped = split_optional(&string(K_DROPPED));
    let residual = f64::from_bits(parse_bits(
        &string(K_RESIDUAL_BITS),
        "stored solve residual",
    )?);
    let iterations = whole_u32(number(K_ITERATIONS), "stored solve iterations")?;
    let fault_variable = string(K_FAULT_VARIABLE);
    let divergence = if fault_variable.is_empty() {
        None
    } else {
        Some(DivergenceFault {
            variable: fault_variable,
            local: f64::from_bits(parse_bits(
                &string(K_FAULT_LOCAL_BITS),
                "stored local divergent value",
            )?),
            remote: f64::from_bits(parse_bits(
                &string(K_FAULT_REMOTE_BITS),
                "stored remote divergent value",
            )?),
            delta: f64::from_bits(parse_bits(
                &string(K_FAULT_DELTA_BITS),
                "stored divergence delta",
            )?),
            tolerance: f64::from_bits(parse_bits(
                &string(K_FAULT_TOLERANCE_BITS),
                "stored divergence tolerance",
            )?),
        })
    };
    let snapshot = StoredStep {
        index,
        values,
        state: StepSolveState {
            status,
            dropped,
            residual,
            iterations,
            divergence,
        },
    };
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn validate_snapshot(snapshot: &StoredStep) -> Result<(), AtlasError> {
    validate_index(snapshot.index)?;
    if snapshot.values.is_empty() {
        return Err(format!("stored step {} has no variables", snapshot.index));
    }
    if snapshot
        .values
        .windows(2)
        .any(|pair| pair[0].name >= pair[1].name)
    {
        return Err(format!(
            "stored step {} variable names must be unique and sorted",
            snapshot.index
        ));
    }
    if let Some(value) = snapshot
        .values
        .iter()
        .find(|value| !value.value.is_finite())
    {
        return Err(format!(
            "stored step {} value for variable {:?} is not finite",
            snapshot.index, value.name
        ));
    }
    validate_step_state(&snapshot.state)
}

fn validate_step_state(state: &StepSolveState) -> Result<(), AtlasError> {
    if !state.residual.is_finite() || state.residual < 0.0 {
        return Err("stored solve residual must be finite and non-negative".to_string());
    }
    if state
        .dropped
        .iter()
        .any(|name| name.is_empty() || name.contains(','))
    {
        return Err("stored dropped relation names must be non-empty and comma-free".to_string());
    }
    if state.dropped.iter().collect::<BTreeSet<_>>().len() != state.dropped.len() {
        return Err("stored solve state cannot repeat a dropped relation".to_string());
    }
    match state.status {
        SolveStatus::Relaxed if state.dropped.is_empty() => {
            return Err(
                "a stored relaxed solve must name at least one dropped relation".to_string(),
            )
        }
        SolveStatus::Converged | SolveStatus::Exhausted if !state.dropped.is_empty() => {
            return Err(format!(
                "stored {} solve cannot carry dropped relations",
                state.status.as_str()
            ))
        }
        _ => {}
    }
    if let Some(fault) = &state.divergence {
        if fault.variable.is_empty()
            || !fault.local.is_finite()
            || !fault.remote.is_finite()
            || !fault.delta.is_finite()
            || !fault.tolerance.is_finite()
            || fault.delta < 0.0
            || fault.tolerance < 0.0
            || fault.delta <= fault.tolerance
        {
            return Err("stored divergence fault is incomplete or invalid".to_string());
        }
    }
    Ok(())
}

fn snapshots_have_same_bits(left: &StoredStep, right: &StoredStep) -> bool {
    left.index == right.index
        && left.values.len() == right.values.len()
        && left.values.iter().zip(&right.values).all(|(left, right)| {
            left.name == right.name && left.value.to_bits() == right.value.to_bits()
        })
        && step_states_have_same_bits(&left.state, &right.state)
}

fn step_states_have_same_bits(left: &StepSolveState, right: &StepSolveState) -> bool {
    left.status == right.status
        && left.dropped == right.dropped
        && left.residual.to_bits() == right.residual.to_bits()
        && left.iterations == right.iterations
        && match (&left.divergence, &right.divergence) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.variable == right.variable
                    && left.local.to_bits() == right.local.to_bits()
                    && left.remote.to_bits() == right.remote.to_bits()
                    && left.delta.to_bits() == right.delta.to_bits()
                    && left.tolerance.to_bits() == right.tolerance.to_bits()
            }
            _ => false,
        }
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

fn encode_name(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(name.len() * 2);
    for byte in name.as_bytes() {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_name(encoded: &str) -> Result<String, AtlasError> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        return Err("stored step variable name is not valid hexadecimal UTF-8".to_string());
    }
    let bytes = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair)
                .map_err(|_| "stored step variable name is not hexadecimal".to_string())?;
            u8::from_str_radix(pair, 16)
                .map_err(|_| "stored step variable name is not hexadecimal".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    String::from_utf8(bytes).map_err(|_| "stored step variable name is not valid UTF-8".to_string())
}

fn parse_bits(value: &str, field: &str) -> Result<u64, AtlasError> {
    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{field} must be 16 hexadecimal digits"));
    }
    u64::from_str_radix(value, 16).map_err(|_| format!("{field} is not valid hexadecimal"))
}

fn split_required(value: &str, field: &str) -> Result<Vec<String>, AtlasError> {
    if value.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    let parts = value.split(',').map(str::to_string).collect::<Vec<_>>();
    if parts.iter().any(String::is_empty) {
        return Err(format!("{field} cannot contain an empty item"));
    }
    Ok(parts)
}

fn split_optional(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split(',').map(str::to_string).collect()
    }
}

fn parse_status(value: &str) -> Result<SolveStatus, AtlasError> {
    match value {
        "converged" => Ok(SolveStatus::Converged),
        "exhausted" => Ok(SolveStatus::Exhausted),
        "relaxed" => Ok(SolveStatus::Relaxed),
        _ => Err(format!(
            "stored solve status must be converged, exhausted, or relaxed; got {value:?}"
        )),
    }
}

fn whole_u32(value: Option<f64>, field: &str) -> Result<u32, AtlasError> {
    let value = value
        .filter(|value| {
            value.is_finite()
                && *value >= 0.0
                && value.fract() == 0.0
                && *value <= f64::from(u32::MAX)
        })
        .ok_or_else(|| format!("{field} must be a whole u32 value"))?;
    Ok(value as u32)
}

#[cfg(test)]
std::thread_local! {
    static SOLVE_INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn solve_invocations() -> usize {
    SOLVE_INVOCATIONS.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ink::{
        create_relation, create_variable, set_agent_ink_mode, set_variable_value,
    };

    fn timeline(step_value: f64, position_value: f64) -> Scene {
        let mut scene = Scene::new();
        create_variable(
            &mut scene,
            "step",
            step_value,
            "scrubbing",
            None,
            None,
            &Author::Agent,
        )
        .expect("create step index");
        create_variable(
            &mut scene,
            "position",
            position_value,
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
        .expect("create time relation");
        solve(&mut scene, &Author::Agent).expect("solve initial step");
        record_current_step(&mut scene, "step", &Author::Agent).expect("store initial step");
        scene
    }

    fn value_bits(step: &StoredStep) -> Vec<(&str, u64)> {
        step.values
            .iter()
            .map(|value| (value.name.as_str(), value.value.to_bits()))
            .collect()
    }

    #[test]
    fn step_12_replays_bit_identically_after_step_40_without_solving() {
        let mut scene = timeline(0.0, 0.0);
        let mut first_step_12 = None;
        for expected in 1..=40 {
            let step = advance_step(&mut scene, "step", &Author::Agent).expect("advance step");
            assert_eq!(step.index, expected);
            if expected == 12 {
                first_step_12 = Some(step);
            }
        }
        let first_step_12 = first_step_12.expect("first visit to step 12");
        let serialized = scene.encode_full().expect("serialize step history");
        let mut reloaded = Scene::from_state(&serialized).expect("reload step history");
        let solves_before = solve_invocations();
        scrub_step(&mut reloaded, "step", 0, &Author::Agent).expect("scrub to step zero");
        scrub_step(&mut reloaded, "step", 40, &Author::Agent).expect("scrub forward to step 40");
        let replayed =
            scrub_step(&mut reloaded, "step", 12, &Author::Agent).expect("scrub back to step 12");
        let solves_after = solve_invocations();

        assert_eq!(value_bits(&replayed), value_bits(&first_step_12));
        assert_eq!(solves_before, solves_after);
        let projected = read(&reloaded).expect("read replayed step");
        let projected_bits = projected
            .variables
            .iter()
            .map(|variable| (variable.name.as_str(), variable.value.to_bits()))
            .collect::<Vec<_>>();
        assert_eq!(projected_bits, value_bits(&first_step_12));
        eprintln!(
            "B4_REPLAY path=0,40,12 step=12 first={:?} replay={:?} bit_identical=true solver_calls_before={} solver_calls_after={}",
            value_bits(&first_step_12),
            value_bits(&replayed),
            solves_before,
            solves_after
        );
    }

    #[test]
    fn warm_start_converges_in_fewer_iterations_than_cold_start() {
        let mut warm_scene = timeline(0.0, 0.0);
        let mut warm_step_40 = None;
        for _ in 1..=40 {
            warm_step_40 = Some(
                advance_step(&mut warm_scene, "step", &Author::Agent)
                    .expect("advance warm-started step"),
            );
        }
        let warm_step_40 = warm_step_40.expect("warm step 40");
        assert_eq!(warm_step_40.state.status, SolveStatus::Converged);

        let cold_scene = &mut timeline(40.0, 0.0);
        let cold = read(cold_scene).expect("read cold solve").solve_state;
        assert_eq!(cold.status, SolveStatus::Converged);
        assert!(
            warm_step_40.state.iterations < cold.iterations,
            "warm={} cold={}",
            warm_step_40.state.iterations,
            cold.iterations
        );
        eprintln!(
            "B4_WARM_START step=40 warm_iterations={} cold_iterations={} warm_less=true",
            warm_step_40.state.iterations, cold.iterations
        );
    }

    #[test]
    fn exhausted_status_survives_scrub() {
        let mut scene = timeline(0.0, 0.0);
        let exhausted = advance_step_with_options(
            &mut scene,
            "step",
            SolveOptions {
                max_iterations: 0,
                ..SolveOptions::default()
            },
            &Author::Agent,
        )
        .expect("store exhausted step");
        assert_eq!(exhausted.state.status, SolveStatus::Exhausted);

        scrub_step(&mut scene, "step", 0, &Author::Agent).expect("scrub to step zero");
        let replayed =
            scrub_step(&mut scene, "step", 1, &Author::Agent).expect("replay exhausted step");
        let projected = read(&scene).expect("read exhausted replay");
        assert_eq!(replayed.state.status, SolveStatus::Exhausted);
        assert_eq!(projected.solve_state.status, SolveStatus::Exhausted);
        eprintln!("B4_STATUS step=1 first=exhausted replay=exhausted projected=exhausted");
    }

    #[test]
    fn bound_rejects_before_mutation_or_solve() {
        let mut scene = timeline(
            f64::from(MAX_STORED_STEPS - 1),
            f64::from(MAX_STORED_STEPS - 1),
        );
        let before = scene.encode_full().expect("encode before bound rejection");
        let solves_before = solve_invocations();
        let error = advance_step(&mut scene, "step", &Author::Agent)
            .expect_err("advance beyond bound must fail");
        let solves_after = solve_invocations();
        let after = scene.encode_full().expect("encode after bound rejection");
        assert!(error.contains("at most 128 steps"), "{error}");
        assert_eq!(before, after);
        assert_eq!(solves_before, solves_after);
        eprintln!(
            "B4_BOUND max_steps={} last_index={} unchanged=true solver_calls_before={} solver_calls_after={}",
            MAX_STORED_STEPS,
            MAX_STORED_STEPS - 1,
            solves_before,
            solves_after
        );
    }

    /// The exact live failure: a pinned edit made between two steps must
    /// survive into the step it produces. `advance_step_inner`'s
    /// unconditional `restore_snapshot` used to discard it before the
    /// solve ever saw it, so five consecutive stored steps came back
    /// bit-identical clones of step 0. In alignment mode this must no
    /// longer happen: each recorded step holds its own pinned width and
    /// the height the solver derived from it, and scrubbing back to any of
    /// them replays that step's own values, not step 0's.
    #[test]
    fn alignment_mode_step_keeps_its_own_pinned_edit_not_step_zeros() {
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
        set_agent_ink_mode(&mut scene, AgentInkMode::Alignment, &Author::Agent)
            .expect("set alignment mode before any step exists");
        solve(&mut scene, &Author::Agent).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Agent).expect("record step zero");

        for width in [20.0, 30.0, 40.0] {
            set_variable_value(&scene, "width", width, &Author::Agent).expect("pin the new width");
            solve(&mut scene, &Author::Agent).expect("solve with the new width");
            advance_step(&mut scene, "step", &Author::Agent).expect("advance to the next step");
        }

        let mut stored_by_index = BTreeMap::new();
        for (index, width) in [(1u32, 20.0), (2, 30.0), (3, 40.0)] {
            let stored = stored_step(&scene, index).expect("stored step");
            let named = |name: &str| {
                stored
                    .values
                    .iter()
                    .find(|value| value.name == name)
                    .unwrap_or_else(|| panic!("step {index} is missing {name:?}"))
                    .value
            };
            // width is pinned, so it must be exactly what was set: nothing
            // in either mode is allowed to round a caller's own edit.
            assert_eq!(
                named("width"),
                width,
                "step {index} must hold its own width"
            );
            // height is solver-owned; `equal` converges it to width within
            // the solver's own numeric tolerance, not necessarily bit for
            // bit, so this checks the honest thing the solve promises.
            assert!(
                (named("height") - width).abs() < 1.0e-6,
                "step {index} height {} must track its own width {width}, not step 0's 10",
                named("height")
            );
            stored_by_index.insert(index, stored);
        }

        // Scrub back over the timeline and confirm step 2 replays exactly
        // what was stored for step 2, not step 0's width=10, height=10 —
        // that bit-identical clone of step 0 is exactly what the live bug
        // produced for every step in the run.
        scrub_step(&mut scene, "step", 0, &Author::Agent).expect("scrub to step zero");
        let replayed_step_2 =
            scrub_step(&mut scene, "step", 2, &Author::Agent).expect("scrub to step two");
        assert_eq!(
            value_bits(&replayed_step_2),
            value_bits(&stored_by_index[&2]),
            "step 2 must scrub back to its own recorded values, not step 0's"
        );
        eprintln!(
            "M1_ALIGNMENT_STEP width_series=[20,30,40] step2_stored={:?} step2_replay={:?}",
            value_bits(&stored_by_index[&2]),
            value_bits(&replayed_step_2)
        );
    }
}
