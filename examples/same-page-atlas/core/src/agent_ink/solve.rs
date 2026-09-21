use std::collections::{BTreeMap, BTreeSet};

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use yrs::{Any, Map, Out, Transact};

use super::dimension;
use super::formula;
use super::relation::{variable_object_ids, Relation};
use crate::{read, text, Atlas, AtlasError, VarState, Variable, MAX_LABEL};

pub(super) const KIND_SOLVE_STATE: &str = "agent_ink_solve_state";

/// B0 measured `7.42920990043127770e-11` of drift after 400 warm starts. A
/// `1e-9` band is more than thirteen times that measured path while remaining
/// small enough that different geometric branches cannot be silently merged.
pub const DIVERGENCE_TOLERANCE: f64 = 1.0e-9;

const DEFAULT_MAX_ITERATIONS: u32 = 512;
const DEFAULT_RESIDUAL_TOLERANCE: f64 = 1.0e-12;
const FINITE_DIFFERENCE_SCALE: f64 = 1.0e-6;
const GRADIENT_NORM_SQUARED_TOLERANCE: f64 = 1.0e-24;
const MAX_LINE_SEARCH_STEPS: usize = 32;

const K_STATUS: &str = "status";
const K_DROPPED: &str = "dropped";
const K_RESIDUAL: &str = "residual";
const K_ITERATIONS: &str = "iterations";
const K_FAULT_VARIABLE: &str = "fault_variable";
const K_FAULT_LOCAL: &str = "fault_local";
const K_FAULT_REMOTE: &str = "fault_remote";
const K_FAULT_DELTA: &str = "fault_delta";
const K_FAULT_TOLERANCE: &str = "fault_tolerance";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SolveStatus {
    Converged,
    Exhausted,
    Relaxed,
}

impl SolveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Converged => "converged",
            Self::Exhausted => "exhausted",
            Self::Relaxed => "relaxed",
        }
    }

    fn parse(value: &str) -> Result<Self, AtlasError> {
        match value {
            "converged" => Ok(Self::Converged),
            "exhausted" => Ok(Self::Exhausted),
            "relaxed" => Ok(Self::Relaxed),
            _ => Err(format!(
                "solve status must be converged, exhausted, or relaxed; got {value:?}"
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DivergenceFault {
    pub variable: String,
    pub local: f64,
    pub remote: f64,
    pub delta: f64,
    pub tolerance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SolveState {
    pub status: SolveStatus,
    pub dropped: Vec<String>,
    pub residual: f64,
    pub iterations: u32,
    pub divergence: Option<DivergenceFault>,
    #[serde(skip)]
    projected: bool,
}

impl Default for SolveState {
    fn default() -> Self {
        Self {
            status: SolveStatus::Converged,
            dropped: Vec::new(),
            residual: 0.0,
            iterations: 0,
            divergence: None,
            projected: false,
        }
    }
}

impl SolveState {
    pub(super) fn is_projected(&self) -> bool {
        self.projected
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolveOptions {
    pub max_iterations: u32,
    pub residual_tolerance: f64,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
            residual_tolerance: DEFAULT_RESIDUAL_TOLERANCE,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SolvedValue {
    pub name: String,
    pub value: f64,
}

pub fn solved_values(scene: &Scene) -> Result<Vec<SolvedValue>, AtlasError> {
    let atlas = read(scene)?;
    if !atlas.solve_state.is_projected()
        || !matches!(
            atlas.solve_state.status,
            SolveStatus::Converged | SolveStatus::Relaxed
        )
    {
        return Err("the document has no converged or relaxed solve result to compare".to_string());
    }
    Ok(atlas
        .variables
        .into_iter()
        .map(|variable| SolvedValue {
            name: variable.name,
            value: variable.value,
        })
        .collect())
}

pub fn set_variable_value(
    scene: &Scene,
    name: &str,
    value: f64,
    author: &Author,
) -> Result<(), AtlasError> {
    if !value.is_finite() {
        return Err("variable value must be a finite number".to_string());
    }
    let atlas = read(scene)?;
    let variable = atlas
        .variables
        .iter()
        .find(|variable| variable.name == name)
        .ok_or_else(|| format!("no variable named {name:?}"))?;
    match variable.state {
        VarState::Pinned | VarState::Scrubbing => {}
        VarState::Free => {
            return Err(format!(
                "free variable {name:?} is solver-owned and cannot be written directly"
            ))
        }
        VarState::Derived => {
            return Err(format!(
                "derived variable {name:?} follows its linear relation and cannot be written directly"
            ))
        }
        VarState::Measured => {
            return Err(format!(
                "measured variable {name:?} is computed from the drawing and cannot be written directly"
            ))
        }
    }
    crate::living_ink::write_variable_value(scene, &atlas, name, variable.value, value, author)
}

pub fn solve(scene: &mut Scene, author: &Author) -> Result<SolveState, AtlasError> {
    solve_with_options(scene, SolveOptions::default(), author)
}

pub fn solve_with_options(
    scene: &mut Scene,
    options: SolveOptions,
    author: &Author,
) -> Result<SolveState, AtlasError> {
    if !options.residual_tolerance.is_finite() || options.residual_tolerance <= 0.0 {
        return Err("solve residual tolerance must be a positive finite number".to_string());
    }

    let atlas = read(scene)?;
    let system = System::from_atlas(&atlas)?;
    let initial = atlas
        .variables
        .iter()
        .map(|variable| variable.value)
        .collect::<Vec<_>>();
    let active = (0..system.residuals.len()).collect::<Vec<_>>();
    let initial_residual = system.objective(&initial, &active)?;

    match optimize(&system, &initial, &active, options)? {
        Optimization::Converged {
            values,
            residual,
            iterations,
        } => {
            let solved = system.materialize(&values)?;
            let state = SolveState {
                status: SolveStatus::Converged,
                dropped: Vec::new(),
                residual,
                iterations,
                divergence: None,
                projected: true,
            };
            commit_solution(scene, &atlas.variables, &initial, &solved, state, author)
        }
        Optimization::Exhausted { iterations, .. } => write_state(
            scene,
            SolveState {
                status: SolveStatus::Exhausted,
                dropped: Vec::new(),
                residual: initial_residual,
                iterations,
                divergence: None,
                projected: true,
            },
            author,
        ),
        Optimization::Stationary {
            iterations: stationary_iterations,
            ..
        } => {
            let relaxed = relax(&system, &initial, &active, options)?;
            let solved = system.materialize(&relaxed.values)?;
            let state = SolveState {
                status: SolveStatus::Relaxed,
                dropped: relaxed.dropped,
                residual: relaxed.residual,
                iterations: stationary_iterations.saturating_add(relaxed.iterations),
                divergence: None,
                projected: true,
            };
            commit_solution(scene, &atlas.variables, &initial, &solved, state, author)
        }
    }
}

/// Compare a local projection with another replica's named solve result.
///
/// A mismatch records a fault in the document-level solve state. It never
/// averages, rounds, or writes either replica's variable values.
pub fn report_replica_solution(
    scene: &mut Scene,
    local: &[SolvedValue],
    remote: &[SolvedValue],
    author: &Author,
) -> Result<SolveState, AtlasError> {
    let atlas = read(scene)?;
    if !atlas.solve_state.is_projected()
        || !matches!(
            atlas.solve_state.status,
            SolveStatus::Converged | SolveStatus::Relaxed
        )
    {
        return Err("the document has no converged or relaxed local solve state".to_string());
    }
    let projected_names = atlas
        .variables
        .iter()
        .map(|variable| variable.name.as_str())
        .collect::<BTreeSet<_>>();
    let local_by_name = named_values(local, "local")?;
    let remote_by_name = named_values(remote, "remote")?;
    let local_names = local_by_name.keys().copied().collect::<BTreeSet<_>>();
    let remote_names = remote_by_name.keys().copied().collect::<BTreeSet<_>>();
    if projected_names != local_names || projected_names != remote_names {
        return Err(format!(
            "replica solution names do not match the document projection; projected={projected_names:?}, local={local_names:?}, remote={remote_names:?}"
        ));
    }

    let divergence = projected_names
        .iter()
        .filter_map(|&name| {
            let local = local_by_name[name];
            let remote = remote_by_name[name];
            let delta = (local - remote).abs();
            (delta > DIVERGENCE_TOLERANCE).then_some(DivergenceFault {
                variable: name.to_string(),
                local,
                remote,
                delta,
                tolerance: DIVERGENCE_TOLERANCE,
            })
        })
        .max_by(|left, right| {
            left.delta
                .total_cmp(&right.delta)
                .then_with(|| right.variable.cmp(&left.variable))
        });

    let mut state = atlas.solve_state;
    state.divergence = divergence;
    state.projected = true;
    write_state(scene, state, author)
}

fn named_values<'a>(
    values: &'a [SolvedValue],
    seat: &str,
) -> Result<BTreeMap<&'a str, f64>, AtlasError> {
    let mut by_name = BTreeMap::new();
    for value in values {
        if !value.value.is_finite() {
            return Err(format!(
                "{seat} value for variable {:?} is not finite",
                value.name
            ));
        }
        if by_name.insert(value.name.as_str(), value.value).is_some() {
            return Err(format!(
                "{seat} solution contains variable {:?} more than once",
                value.name
            ));
        }
    }
    Ok(by_name)
}

pub(super) fn project_state(
    string: &impl Fn(&str) -> String,
    number: &impl Fn(&str) -> Option<f64>,
) -> Result<SolveState, AtlasError> {
    let status = SolveStatus::parse(&string(K_STATUS))?;
    let dropped = match string(K_DROPPED) {
        value if value.is_empty() => Vec::new(),
        value => value
            .split(',')
            .map(|name| {
                let canonical = text(name, MAX_LABEL, "dropped relation name")?;
                if canonical.is_empty() || canonical != name {
                    return Err(
                        "dropped relation names must be non-empty and canonical".to_string()
                    );
                }
                Ok(canonical)
            })
            .collect::<Result<Vec<_>, AtlasError>>()?,
    };
    if dropped.iter().collect::<BTreeSet<_>>().len() != dropped.len() {
        return Err("solve state cannot name a dropped relation more than once".to_string());
    }
    match status {
        SolveStatus::Relaxed if dropped.is_empty() => {
            return Err("a relaxed solve state must name at least one dropped relation".to_string())
        }
        SolveStatus::Converged | SolveStatus::Exhausted if !dropped.is_empty() => {
            return Err(format!(
                "solve status {} cannot carry dropped relations",
                status.as_str()
            ))
        }
        _ => {}
    }
    let residual = finite_non_negative(number(K_RESIDUAL), "solve residual")?;
    let iterations = whole_u32(number(K_ITERATIONS), "solve iterations")?;
    let fault_variable = string(K_FAULT_VARIABLE);
    let divergence = if fault_variable.is_empty() {
        None
    } else {
        let variable = text(&fault_variable, MAX_LABEL, "divergent variable name")?;
        if variable != fault_variable {
            return Err("divergent variable name must be canonical".to_string());
        }
        let local = finite(number(K_FAULT_LOCAL), "local divergent value")?;
        let remote = finite(number(K_FAULT_REMOTE), "remote divergent value")?;
        let delta = finite_non_negative(number(K_FAULT_DELTA), "divergence delta")?;
        let tolerance = finite_non_negative(number(K_FAULT_TOLERANCE), "divergence tolerance")?;
        if delta <= tolerance {
            return Err(format!(
                "divergence fault delta {delta} does not exceed tolerance {tolerance}"
            ));
        }
        Some(DivergenceFault {
            variable,
            local,
            remote,
            delta,
            tolerance,
        })
    };

    Ok(SolveState {
        status,
        dropped,
        residual,
        iterations,
        divergence,
        projected: true,
    })
}

fn finite(value: Option<f64>, field: &str) -> Result<f64, AtlasError> {
    value
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("{field} is missing or not finite"))
}

fn finite_non_negative(value: Option<f64>, field: &str) -> Result<f64, AtlasError> {
    let value = finite(value, field)?;
    if value < 0.0 {
        return Err(format!("{field} cannot be negative"));
    }
    Ok(value)
}

fn whole_u32(value: Option<f64>, field: &str) -> Result<u32, AtlasError> {
    let value = finite_non_negative(value, field)?;
    if value.fract() != 0.0 || value > f64::from(u32::MAX) {
        return Err(format!("{field} must be a whole u32 value"));
    }
    Ok(value as u32)
}

#[derive(Debug, Clone)]
struct DerivedSpec {
    relation: String,
    target: usize,
    source: usize,
    m: f64,
    b: f64,
}

#[derive(Debug, Clone)]
struct ResidualSpec {
    name: String,
    kind: ResidualKind,
}

#[derive(Debug, Clone)]
enum ResidualKind {
    /// `right`'s raw value is multiplied by `right_to_left_scale` —
    /// `right`'s own `dimension::quantity_scale` divided by `left`'s —
    /// before the two are compared, so `equal` can genuinely relate
    /// "3 cm" to "30 mm" (`right_to_left_scale` is `10`, so `3 cm` reads as
    /// `30` in `left`'s frame) and not just variables that happen to share
    /// a unit string. `relation.rs`'s `dimension_check` has already
    /// refused this relation at create time if the two do not share a
    /// quantity at all, so `right_to_left_scale` is always a real, finite
    /// ratio by the time a solve runs.
    ///
    /// This converts `right` into `left`'s own unit rather than converting
    /// both sides into their quantity's canonical unit (meters, say): the
    /// finite-difference gradient in [`System::gradient`] probes a step
    /// sized relative to a free variable's own raw value, so a residual
    /// that stayed scaled down by a physically tiny canonical factor (an
    /// `"nm"` variable's canonical scale is `1e-9`) would report a
    /// gradient too small for the solver to trust was ever moving, and
    /// wrongly report `Exhausted` on a relation that a plain unit-for-unit
    /// comparison solves easily. Converting into one side's own unit keeps
    /// the residual's magnitude — and therefore the solver's sensitivity
    /// to it — the same order as an ordinary same-unit `equal` relation
    /// always had, for any pair of units this module recognizes.
    Equal {
        left: usize,
        right: usize,
        right_to_left_scale: f64,
    },
    /// `dimension_check` requires all five of `members` (four coordinates
    /// and the target) to share one quantity; `scales` converts each of
    /// the other four into `members[0]`'s (`ax`'s) own unit — the same
    /// "one side's frame, not the canonical unit" choice `Equal` makes,
    /// and for the same reason: it keeps the residual's magnitude at the
    /// scale of `ax`'s own raw value instead of shrinking it by whatever
    /// physically tiny canonical factor `ax`'s unit happens to carry.
    /// `scales[0]` (`ax` relative to itself) is always exactly `1.0`.
    Distance {
        members: [usize; 5],
        scales: [f64; 5],
    },
    /// `dimension_check` requires the six position members (`members[0..6]`)
    /// to share one quantity and the seventh, `target`, to be an angle or
    /// bare. `scales[0..6]` convert the five other positions into
    /// `members[0]`'s (`ax`'s) own unit, the same as `Distance`'s `scales`
    /// does — and this conversion is exact regardless of which position is
    /// chosen as the reference, because `atan2`'s angle is invariant to a
    /// uniform rescaling of every position it is given. `scales[6]`
    /// converts `target` into radians using *its own* absolute
    /// `dimension::quantity_scale` (`deg`'s is `pi/180`; `rad`'s and a bare
    /// unit's are both `1.0`) rather than a ratio against a position,
    /// because `target` is a different physical quantity (an angle) than
    /// the positions are, with no shared frame to convert relative to; the
    /// `atan2` result it is compared against is always already in radians.
    Angle {
        members: [usize; 7],
        scales: [f64; 7],
    },
    /// `target` is `members[0]` (the dependent quantity, in the same sense
    /// `linear`'s first member is dependent) and `inputs` pairs each of
    /// `members[1..]` with its variable index, in the order the relation's
    /// expression was validated against at create time. The residual is
    /// `expression(inputs) - values[target]`, evaluated on raw values with
    /// no unit conversion at all: `relation.rs`'s `dimension_check` only
    /// requires the arguments of `exp`/`ln`/`log10`/`sin`/`cos`/`tan` to be
    /// dimensionless (see its `"formula"` arm's doc comment for exactly
    /// what that does and does not catch), so there is no per-member scale
    /// to apply here the way there is for `Equal`, `Distance`, and `Angle`.
    Formula {
        expr: formula::Expr,
        target: usize,
        inputs: Vec<(String, usize)>,
    },
}

struct System {
    variables: Vec<Variable>,
    free: Vec<usize>,
    derived: Vec<DerivedSpec>,
    residuals: Vec<ResidualSpec>,
}

impl System {
    fn from_atlas(atlas: &Atlas) -> Result<Self, AtlasError> {
        let indexes = atlas
            .variables
            .iter()
            .enumerate()
            .map(|(index, variable)| (variable.name.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        // A legacy variable whose stored unit no longer parses (see
        // `dimension.rs`'s `VariableDimension::Unrecognized`) falls back to
        // a scale of `1.0` here rather than failing the solve: a relation
        // naming it could only have been created before this module
        // existed, since `relation.rs`'s `dimension_check` refuses any
        // *new* relation that would need to resolve it. A solve that
        // already runs today keeps running in the same raw units it always
        // used.
        let variable_scale =
            |index: usize| dimension::quantity_scale(atlas.variables[index].unit.as_deref());
        let member_indexes = |relation: &Relation| {
            relation
                .members
                .iter()
                .map(|name| {
                    indexes.get(name.as_str()).copied().ok_or_else(|| {
                        format!(
                            "relation {:?} member {name:?} does not resolve to an agent-ink variable",
                            relation.name
                        )
                    })
                })
                .collect::<Result<Vec<_>, AtlasError>>()
        };

        let mut derived = Vec::new();
        let mut residuals = Vec::new();
        let mut derived_targets = BTreeMap::new();
        for relation in &atlas.relations {
            let members = member_indexes(relation)?;
            match relation.op.as_str() {
                "linear" => {
                    let m = relation.m.ok_or_else(|| {
                        format!("linear relation {:?} is missing m", relation.name)
                    })?;
                    let b = relation.b.ok_or_else(|| {
                        format!("linear relation {:?} is missing b", relation.name)
                    })?;
                    let spec = DerivedSpec {
                        relation: relation.name.clone(),
                        target: members[0],
                        source: members[1],
                        m,
                        b,
                    };
                    if let Some(existing) =
                        derived_targets.insert(spec.target, spec.relation.clone())
                    {
                        return Err(format!(
                            "variable {:?} is derived by both {existing:?} and {:?}",
                            atlas.variables[spec.target].name, spec.relation
                        ));
                    }
                    derived.push(spec);
                }
                "equal" => residuals.push(ResidualSpec {
                    name: relation.name.clone(),
                    kind: ResidualKind::Equal {
                        left: members[0],
                        right: members[1],
                        right_to_left_scale: variable_scale(members[1])
                            / variable_scale(members[0]),
                    },
                }),
                "distance" => {
                    let members: [usize; 5] = members.try_into().map_err(|members: Vec<_>| {
                        format!(
                            "distance relation {:?} has {} members instead of 5",
                            relation.name,
                            members.len()
                        )
                    })?;
                    // Every other position (and the target) converts into
                    // `ax`'s own unit — see `ResidualKind::Distance`'s doc
                    // comment for why that beats canonicalizing everyone to
                    // the quantity's absolute base unit.
                    let anchor = variable_scale(members[0]);
                    let scales = members.map(|index| variable_scale(index) / anchor);
                    residuals.push(ResidualSpec {
                        name: relation.name.clone(),
                        kind: ResidualKind::Distance { members, scales },
                    });
                }
                "angle" => {
                    let members: [usize; 7] = members.try_into().map_err(|members: Vec<_>| {
                        format!(
                            "angle relation {:?} has {} members instead of 7",
                            relation.name,
                            members.len()
                        )
                    })?;
                    // `members[0..6]` (the positions) convert into `ax`'s
                    // own unit, same as `Distance`; `members[6]` (the
                    // angle target) converts by its own absolute scale
                    // into radians instead — see `ResidualKind::Angle`'s
                    // doc comment for why the target needs a different
                    // rule than the positions do.
                    let anchor = variable_scale(members[0]);
                    let mut scales = [0.0; 7];
                    for (index, scale) in scales.iter_mut().enumerate().take(6) {
                        *scale = variable_scale(members[index]) / anchor;
                    }
                    scales[6] = variable_scale(members[6]);
                    residuals.push(ResidualSpec {
                        name: relation.name.clone(),
                        kind: ResidualKind::Angle { members, scales },
                    });
                }
                "formula" => {
                    let text = relation.expression.as_deref().ok_or_else(|| {
                        format!(
                            "formula relation {:?} is missing its expression",
                            relation.name
                        )
                    })?;
                    let expr = formula::parse(text).map_err(|error| {
                        format!(
                            "formula relation {:?} has an invalid expression: {error}",
                            relation.name
                        )
                    })?;
                    let inputs = relation.members[1..]
                        .iter()
                        .cloned()
                        .zip(members[1..].iter().copied())
                        .collect::<Vec<_>>();
                    residuals.push(ResidualSpec {
                        name: relation.name.clone(),
                        kind: ResidualKind::Formula {
                            expr,
                            target: members[0],
                            inputs,
                        },
                    });
                }
                _ => return Err(format!("unknown relation op {:?}", relation.op)),
            }
        }

        for (index, variable) in atlas.variables.iter().enumerate() {
            match (variable.state, derived_targets.contains_key(&index)) {
                (VarState::Derived, false) => {
                    return Err(format!(
                        "derived variable {:?} has no linear relation",
                        variable.name
                    ))
                }
                (VarState::Derived, true) => {}
                (_, true) => {
                    return Err(format!(
                        "linear dependent variable {:?} is not projected as derived",
                        variable.name
                    ))
                }
                _ => {}
            }
        }

        let free = atlas
            .variables
            .iter()
            .enumerate()
            .filter_map(|(index, variable)| (variable.state == VarState::Free).then_some(index))
            .collect();
        let system = Self {
            variables: atlas.variables.clone(),
            free,
            derived,
            residuals,
        };
        system.materialize(
            &system
                .variables
                .iter()
                .map(|variable| variable.value)
                .collect::<Vec<_>>(),
        )?;
        Ok(system)
    }

    fn materialize(&self, base: &[f64]) -> Result<Vec<f64>, AtlasError> {
        let mut values = base.to_vec();
        let mut resolved = self
            .variables
            .iter()
            .map(|variable| variable.state != VarState::Derived)
            .collect::<Vec<_>>();
        let mut remaining = self.derived.len();
        while remaining > 0 {
            let mut advanced = false;
            for spec in &self.derived {
                if resolved[spec.target] || !resolved[spec.source] {
                    continue;
                }
                let value = spec.m * values[spec.source] + spec.b;
                if !value.is_finite() {
                    return Err(format!(
                        "linear relation {:?} produced a non-finite value",
                        spec.relation
                    ));
                }
                values[spec.target] = value;
                resolved[spec.target] = true;
                remaining -= 1;
                advanced = true;
            }
            if !advanced {
                let cycle = self
                    .derived
                    .iter()
                    .filter(|spec| !resolved[spec.target])
                    .map(|spec| spec.relation.clone())
                    .collect::<Vec<_>>();
                return Err(format!(
                    "linear derivation cycle prevents substitution: {}",
                    cycle.join(", ")
                ));
            }
        }
        Ok(values)
    }

    fn objective(&self, base: &[f64], active: &[usize]) -> Result<f64, AtlasError> {
        let values = self.materialize(base)?;
        let mut objective = 0.0;
        for &index in active {
            let residual = self.residual(&values, &self.residuals[index]);
            if !residual.is_finite() {
                return Err(format!(
                    "relation {:?} produced a non-finite residual",
                    self.residuals[index].name
                ));
            }
            objective += residual * residual;
            if !objective.is_finite() {
                return Err("solve objective overflowed to a non-finite value".to_string());
            }
        }
        Ok(objective)
    }

    fn residual(&self, values: &[f64], residual: &ResidualSpec) -> f64 {
        // `Formula` carries an `Expr` and a `Vec`, neither `Copy`, so this
        // matches the enum by reference now instead of by value; the other
        // arms only need one extra `*` each to keep reading the same way.
        match &residual.kind {
            ResidualKind::Equal {
                left,
                right,
                right_to_left_scale,
            } => values[*left] - values[*right] * right_to_left_scale,
            ResidualKind::Distance {
                members: [ax, ay, bx, by, target],
                scales: [sax, say, sbx, sby, starget],
            } => {
                let dx = values[*ax] * sax - values[*bx] * sbx;
                let dy = values[*ay] * say - values[*by] * sby;
                (dx * dx + dy * dy).sqrt() - values[*target] * starget
            }
            ResidualKind::Angle {
                members: [ax, ay, px, py, bx, by, target],
                scales: [sax, say, spx, spy, sbx, sby, starget],
            } => {
                let ux = values[*ax] * sax - values[*px] * spx;
                let uy = values[*ay] * say - values[*py] * spy;
                let vx = values[*bx] * sbx - values[*px] * spx;
                let vy = values[*by] * sby - values[*py] * spy;
                let mut delta =
                    (ux * vy - uy * vx).atan2(ux * vx + uy * vy) - values[*target] * starget;
                while delta > std::f64::consts::PI {
                    delta -= std::f64::consts::TAU;
                }
                while delta < -std::f64::consts::PI {
                    delta += std::f64::consts::TAU;
                }
                delta
            }
            // `expr` is evaluated exactly as `formula::eval` computes it,
            // including any `NaN`/`inf` a domain error (division by zero,
            // `ln` of a negative number, and the like) produces. That
            // residual then flows straight into `objective`, which already
            // refuses any non-finite residual from any relation kind and
            // names the relation in its error. Formula relations lean on
            // that existing, uniform behavior rather than growing a special
            // "reject this candidate step and try a smaller one" path of
            // their own: every other relation kind reports honestly by
            // erroring the whole solve rather than continuing on a residual
            // it cannot trust, and a formula evaluated outside its domain
            // deserves the same treatment, not a quieter one.
            ResidualKind::Formula {
                expr,
                target,
                inputs,
            } => {
                let env = inputs
                    .iter()
                    .map(|(name, index)| (name.as_str(), values[*index]))
                    .collect::<BTreeMap<_, _>>();
                formula::eval(expr, &env) - values[*target]
            }
        }
    }

    fn gradient(&self, values: &[f64], active: &[usize]) -> Result<Vec<(usize, f64)>, AtlasError> {
        self.free
            .iter()
            .map(|&index| {
                let h = FINITE_DIFFERENCE_SCALE * (1.0 + values[index].abs());
                let mut lower = values.to_vec();
                let mut upper = values.to_vec();
                lower[index] -= h;
                upper[index] += h;
                let derivative =
                    (self.objective(&upper, active)? - self.objective(&lower, active)?) / (2.0 * h);
                if !derivative.is_finite() {
                    return Err(format!(
                        "solve gradient for variable {:?} is not finite",
                        self.variables[index].name
                    ));
                }
                Ok((index, derivative))
            })
            .collect()
    }
}

enum Optimization {
    Converged {
        values: Vec<f64>,
        residual: f64,
        iterations: u32,
    },
    Stationary {
        residual: f64,
        iterations: u32,
    },
    Exhausted {
        residual: f64,
        iterations: u32,
    },
}

impl Optimization {
    fn residual(&self) -> f64 {
        match self {
            Self::Converged { residual, .. }
            | Self::Stationary { residual, .. }
            | Self::Exhausted { residual, .. } => *residual,
        }
    }
}

fn optimize(
    system: &System,
    initial: &[f64],
    active: &[usize],
    options: SolveOptions,
) -> Result<Optimization, AtlasError> {
    let mut values = initial.to_vec();
    let mut residual = system.objective(&values, active)?;
    if residual <= options.residual_tolerance {
        return Ok(Optimization::Converged {
            values,
            residual,
            iterations: 0,
        });
    }
    if options.max_iterations == 0 {
        return Ok(Optimization::Exhausted {
            residual,
            iterations: 0,
        });
    }

    for iteration in 1..=options.max_iterations {
        let gradient = system.gradient(&values, active)?;
        let gradient_norm_squared = gradient
            .iter()
            .map(|(_, derivative)| derivative * derivative)
            .sum::<f64>();
        if gradient_norm_squared <= GRADIENT_NORM_SQUARED_TOLERANCE {
            return Ok(Optimization::Stationary {
                residual,
                iterations: iteration - 1,
            });
        }

        let mut scale = 0.25;
        let mut accepted = None;
        for _ in 0..MAX_LINE_SEARCH_STEPS {
            let mut candidate = values.clone();
            for &(index, derivative) in &gradient {
                candidate[index] -= scale * derivative;
            }
            let candidate_residual = system.objective(&candidate, active)?;
            if candidate_residual <= residual - 1.0e-4 * scale * gradient_norm_squared {
                accepted = Some((candidate, candidate_residual));
                break;
            }
            scale *= 0.5;
        }
        let Some((candidate, candidate_residual)) = accepted else {
            return Ok(Optimization::Exhausted {
                residual,
                iterations: iteration - 1,
            });
        };
        values = candidate;
        residual = candidate_residual;
        if residual <= options.residual_tolerance {
            return Ok(Optimization::Converged {
                values,
                residual,
                iterations: iteration,
            });
        }
    }

    Ok(Optimization::Exhausted {
        residual,
        iterations: options.max_iterations,
    })
}

struct RelaxedOutcome {
    values: Vec<f64>,
    dropped: Vec<String>,
    residual: f64,
    iterations: u32,
}

fn relax(
    system: &System,
    initial: &[f64],
    active: &[usize],
    options: SolveOptions,
) -> Result<RelaxedOutcome, AtlasError> {
    let mut retained = active.to_vec();
    let mut dropped = Vec::new();
    loop {
        let mut best: Option<(usize, Optimization)> = None;
        for position in 0..retained.len() {
            let trial = retained
                .iter()
                .enumerate()
                .filter_map(|(index, relation)| (index != position).then_some(*relation))
                .collect::<Vec<_>>();
            let outcome = optimize(system, initial, &trial, options)?;
            if matches!(outcome, Optimization::Converged { .. }) {
                best = Some((position, outcome));
                break;
            }
            let replace = best.as_ref().is_none_or(|(best_position, best_outcome)| {
                outcome
                    .residual()
                    .total_cmp(&best_outcome.residual())
                    .then_with(|| {
                        system.residuals[retained[position]]
                            .name
                            .cmp(&system.residuals[retained[*best_position]].name)
                    })
                    .is_lt()
            });
            if replace {
                best = Some((position, outcome));
            }
        }

        let Some((position, outcome)) = best else {
            return Err("a stationary solve has no residual relation to relax".to_string());
        };
        let removed = retained.remove(position);
        dropped.push(system.residuals[removed].name.clone());
        if let Optimization::Converged {
            values,
            residual,
            iterations,
        } = outcome
        {
            dropped.sort();
            return Ok(RelaxedOutcome {
                values,
                dropped,
                residual,
                iterations,
            });
        }
    }
}

fn commit_solution(
    scene: &mut Scene,
    variables: &[Variable],
    initial: &[f64],
    solved: &[f64],
    state: SolveState,
    author: &Author,
) -> Result<SolveState, AtlasError> {
    let ids = variable_object_ids(scene)?;
    let mut written = Vec::new();
    for (index, variable) in variables.iter().enumerate() {
        if !matches!(variable.state, VarState::Free | VarState::Derived)
            || initial[index].to_bits() == solved[index].to_bits()
        {
            continue;
        }
        let id = ids
            .get(&variable.name)
            .cloned()
            .map(ObjectId::from)
            .ok_or_else(|| format!("variable {:?} has no persisted object", variable.name))?;
        if let Err(error) = scene.set_props(
            &id,
            &[
                ("value", PropValue::Num(solved[index])),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        ) {
            rollback_values(scene, &written)?;
            return Err(format!(
                "could not write solved variable {:?}: {error}",
                variable.name
            ));
        }
        written.push((id, initial[index], variable.touched_by.clone()));
    }
    let state = match write_state(scene, state, author) {
        Ok(state) => state,
        Err(error) => {
            rollback_values(scene, &written)?;
            return Err(error);
        }
    };

    // The picture follows the numbers: any variable bound to a shape or
    // node property is written into that property now, using the values
    // this solve just committed. A dangling binding is skipped rather than
    // rolled back into — see `geometry::sync_represented_geometry` — so this
    // never turns an honest solve failure into a mysterious one over a stale
    // `represents` pointer.
    let solved_atlas = read(scene)?;
    super::geometry::sync_represented_geometry(scene, &solved_atlas, author)?;
    Ok(state)
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
            .map_err(|error| format!("could not roll back an incomplete solve: {error}"))?;
    }
    Ok(())
}

fn write_state(
    scene: &mut Scene,
    mut state: SolveState,
    author: &Author,
) -> Result<SolveState, AtlasError> {
    state.projected = true;
    let mut ids = solve_state_object_ids(scene)?;
    if ids.len() > 1 {
        return Err("the document contains more than one agent-ink solve state".to_string());
    }
    let fault = state.divergence.as_ref();
    let props = vec![
        (K_STATUS, PropValue::Str(state.status.as_str().to_string())),
        (K_DROPPED, PropValue::Str(state.dropped.join(","))),
        (K_RESIDUAL, PropValue::Num(state.residual)),
        (K_ITERATIONS, PropValue::Num(f64::from(state.iterations))),
        (
            K_FAULT_VARIABLE,
            PropValue::Str(
                fault
                    .map(|fault| fault.variable.clone())
                    .unwrap_or_default(),
            ),
        ),
        (
            K_FAULT_LOCAL,
            PropValue::Num(fault.map(|fault| fault.local).unwrap_or(0.0)),
        ),
        (
            K_FAULT_REMOTE,
            PropValue::Num(fault.map(|fault| fault.remote).unwrap_or(0.0)),
        ),
        (
            K_FAULT_DELTA,
            PropValue::Num(fault.map(|fault| fault.delta).unwrap_or(0.0)),
        ),
        (
            K_FAULT_TOLERANCE,
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
    Ok(state)
}

fn solve_state_object_ids(scene: &Scene) -> Result<Vec<String>, AtlasError> {
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
        if kind == KIND_SOLVE_STATE {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ink::{create_relation, create_variable};

    fn variable(scene: &mut Scene, name: &str, value: f64, state: &str) {
        create_variable(scene, name, value, state, None, None, &Author::Agent)
            .expect("create variable");
    }

    fn variable_with_unit(scene: &mut Scene, name: &str, value: f64, state: &str, unit: &str) {
        create_variable(scene, name, value, state, None, Some(unit), &Author::Agent)
            .expect("create variable with unit");
    }

    fn variable_value_bits(scene: &Scene) -> Vec<(String, u64)> {
        read(scene)
            .expect("project variable values")
            .variables
            .into_iter()
            .map(|variable| (variable.name, variable.value.to_bits()))
            .collect()
    }

    #[test]
    fn over_constrained_system_relaxes_and_names_the_dropped_relation() {
        let mut scene = Scene::new();
        variable(&mut scene, "held", 0.0, "pinned");
        variable(&mut scene, "zero", 0.0, "pinned");
        variable(&mut scene, "ten", 10.0, "pinned");
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
        .expect("create satisfied relation");
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

        let result = solve(&mut scene, &Author::Agent).expect("solve with named relaxation");
        let projected = read(&scene).expect("project solve state").solve_state;
        eprintln!(
            "status={} dropped={:?} residual={:.17e}",
            projected.status.as_str(),
            projected.dropped,
            projected.residual
        );
        assert_eq!(result.status, SolveStatus::Relaxed);
        assert_eq!(projected.status, SolveStatus::Relaxed);
        assert_eq!(projected.dropped, ["drop-ten"]);
        assert!(projected.residual <= DEFAULT_RESIDUAL_TOLERANCE);
    }

    #[test]
    fn exhausted_and_converged_are_distinct_projected_states() {
        let mut scene = Scene::new();
        variable(&mut scene, "free", 100.0, "free");
        variable(&mut scene, "target", 0.0, "pinned");
        create_relation(
            &mut scene,
            "meet-target",
            "equal",
            &["free", "target"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");
        let before = read(&scene).expect("read before").variables[0].value;

        solve_with_options(
            &mut scene,
            SolveOptions {
                max_iterations: 0,
                ..SolveOptions::default()
            },
            &Author::Agent,
        )
        .expect("report exhaustion");
        let exhausted = read(&scene).expect("project exhausted");
        let after_exhausted = exhausted
            .variables
            .iter()
            .find(|variable| variable.name == "free")
            .expect("free variable")
            .value;
        assert_eq!(exhausted.solve_state.status, SolveStatus::Exhausted);
        assert_eq!(before.to_bits(), after_exhausted.to_bits());

        solve(&mut scene, &Author::Agent).expect("converge with default cap");
        let converged = read(&scene).expect("project converged");
        eprintln!(
            "first_status={} first_value_bits=0x{:016x} second_status={} second_residual={:.17e}",
            exhausted.solve_state.status.as_str(),
            after_exhausted.to_bits(),
            converged.solve_state.status.as_str(),
            converged.solve_state.residual
        );
        assert_eq!(converged.solve_state.status, SolveStatus::Converged);
        assert_ne!(exhausted.solve_state.status, converged.solve_state.status);
    }

    #[test]
    fn positive_iteration_cap_projects_exhausted_without_committing_partial_values() {
        let mut scene = Scene::new();
        variable(&mut scene, "free", 100.0, "free");
        variable(&mut scene, "target", 0.0, "pinned");
        create_relation(
            &mut scene,
            "meet-target",
            "equal",
            &["free", "target"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");
        let before = variable_value_bits(&scene);
        let max_iterations = 2;

        let reported = solve_with_options(
            &mut scene,
            SolveOptions {
                max_iterations,
                residual_tolerance: 1.0e-30,
            },
            &Author::Agent,
        )
        .expect("report positive-cap exhaustion");
        let exhausted = read(&scene).expect("project positive-cap exhaustion");
        let after = variable_value_bits(&scene);

        assert_eq!(reported.status, SolveStatus::Exhausted);
        assert_eq!(reported.iterations, max_iterations);
        assert_eq!(exhausted.solve_state.status, SolveStatus::Exhausted);
        assert_eq!(exhausted.solve_state.iterations, max_iterations);
        assert_eq!(before, after, "an exhausted solve committed partial values");

        solve(&mut scene, &Author::Agent).expect("converge with the default cap");
        let converged = read(&scene).expect("project converged state");
        assert_eq!(converged.solve_state.status, SolveStatus::Converged);
        assert_ne!(exhausted.solve_state.status, converged.solve_state.status);
    }

    #[test]
    fn line_search_failure_projects_exhausted_without_committing_a_candidate() {
        let mut scene = Scene::new();
        variable(&mut scene, "source", 1.0, "free");
        variable(&mut scene, "dependent", 0.0, "free");
        variable(&mut scene, "target", 0.0, "pinned");
        create_relation(
            &mut scene,
            "amplify",
            "linear",
            &["dependent", "source"],
            Some(1.0e6),
            Some(0.0),
            None,
            &Author::Agent,
        )
        .expect("create high-curvature linear relation");
        create_relation(
            &mut scene,
            "meet-target",
            "equal",
            &["dependent", "target"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create residual relation");
        let before = variable_value_bits(&scene);
        let max_iterations = 64;

        let reported = solve_with_options(
            &mut scene,
            SolveOptions {
                max_iterations,
                ..SolveOptions::default()
            },
            &Author::Agent,
        )
        .expect("report line-search exhaustion");
        let projected = read(&scene).expect("project line-search exhaustion");
        let after = variable_value_bits(&scene);

        assert_eq!(reported.status, SolveStatus::Exhausted);
        assert_eq!(
            reported.iterations, 0,
            "a positive-cap exhaustion before the first accepted step pins the line-search failure path"
        );
        assert!(reported.iterations < max_iterations);
        assert_eq!(projected.solve_state.status, SolveStatus::Exhausted);
        assert_eq!(projected.solve_state.iterations, 0);
        assert_eq!(before, after, "line-search failure committed a candidate");
    }

    #[test]
    fn linear_relation_derives_dependent_and_refuses_direct_write() {
        let mut scene = Scene::new();
        variable(&mut scene, "source", 2.0, "pinned");
        variable(&mut scene, "dependent", 0.0, "free");
        create_relation(
            &mut scene,
            "triple-plus-one",
            "linear",
            &["dependent", "source"],
            Some(3.0),
            Some(1.0),
            None,
            &Author::Agent,
        )
        .expect("create linear relation");

        let atlas = read(&scene).expect("project derived variable");
        let dependent = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "dependent")
            .expect("dependent variable");
        assert_eq!(dependent.state, VarState::Derived);
        assert_eq!(dependent.value, 7.0);
        let error = set_variable_value(&scene, "dependent", 99.0, &Author::Agent)
            .expect_err("direct derived write must be refused");
        let after = read(&scene).expect("read after refused write");
        let unchanged = after
            .variables
            .iter()
            .find(|variable| variable.name == "dependent")
            .expect("dependent variable");
        eprintln!(
            "state={} value={} refused={error}",
            unchanged.state.as_str(),
            unchanged.value
        );
        assert!(error.contains("cannot be written directly"), "{error}");
        assert_eq!(unchanged.value, 7.0);
    }

    #[test]
    fn divergence_channel_faults_beyond_tolerance_and_clears_on_agreement() {
        let mut scene = Scene::new();
        variable(&mut scene, "x", 1.0, "pinned");
        solve(&mut scene, &Author::Human).expect("establish local solve state");
        let local = solved_values(&scene).expect("local solution");
        let remote = vec![SolvedValue {
            name: "x".to_string(),
            value: 1.0 + 2.0 * DIVERGENCE_TOLERANCE,
        }];

        report_replica_solution(&mut scene, &local, &remote, &Author::Human)
            .expect("record divergence fault");
        let faulted = read(&scene).expect("project fault");
        let fault = faulted
            .solve_state
            .divergence
            .as_ref()
            .expect("fault beyond tolerance");
        eprintln!(
            "fault_variable={} delta={:.17e} tolerance={:.17e}",
            fault.variable, fault.delta, fault.tolerance
        );
        assert!(fault.delta > DIVERGENCE_TOLERANCE);
        assert_eq!(faulted.variables[0].value, 1.0);

        report_replica_solution(&mut scene, &local, &local, &Author::Human)
            .expect("clear fault on agreement");
        let agreed = read(&scene).expect("project agreement");
        eprintln!("agreement_fault={:?}", agreed.solve_state.divergence);
        assert!(agreed.solve_state.divergence.is_none());
        assert_eq!(agreed.variables[0].value, 1.0);
    }

    #[test]
    fn distance_and_angle_relations_run_through_the_nonlinear_solver() {
        let mut scene = Scene::new();
        variable(&mut scene, "ax", 1.0, "pinned");
        variable(&mut scene, "ay", 0.0, "pinned");
        variable(&mut scene, "pivot-x", 0.0, "pinned");
        variable(&mut scene, "pivot-y", 0.0, "pinned");
        variable(&mut scene, "bx", 0.2, "free");
        variable(&mut scene, "by", 0.7, "free");
        variable(&mut scene, "radius", 1.0, "pinned");
        variable(
            &mut scene,
            "right-angle",
            std::f64::consts::FRAC_PI_2,
            "pinned",
        );
        create_relation(
            &mut scene,
            "unit-radius",
            "distance",
            &["pivot-x", "pivot-y", "bx", "by", "radius"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create distance relation");
        create_relation(
            &mut scene,
            "right-angle-at-pivot",
            "angle",
            &["ax", "ay", "pivot-x", "pivot-y", "bx", "by", "right-angle"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create angle relation");

        let state = solve(&mut scene, &Author::Agent).expect("solve nonlinear system");
        let atlas = read(&scene).expect("project nonlinear result");
        let value = |name: &str| {
            atlas
                .variables
                .iter()
                .find(|variable| variable.name == name)
                .expect("named variable")
                .value
        };
        eprintln!(
            "nonlinear_status={} residual={:.17e} bx={:.17e} by={:.17e}",
            state.status.as_str(),
            state.residual,
            value("bx"),
            value("by")
        );
        assert_eq!(state.status, SolveStatus::Converged);
        assert!(state.residual <= DEFAULT_RESIDUAL_TOLERANCE);
        assert!(value("bx").abs() < 1.0e-6);
        assert!((value("by") - 1.0).abs() < 1.0e-6);
    }

    /// A formula relation ties `decay` to `exp(-2*x)` for a pinned `x`; the
    /// solver never differentiates the expression symbolically (it only
    /// ever calls `objective`, which calls the expression evaluator), so
    /// this is really a test that `formula`'s residual and the central-
    /// difference gradient in `System::gradient` cooperate correctly, not
    /// just that the evaluator itself is right (that's `formula.rs`'s job).
    #[test]
    fn formula_relation_solves_exponential_decay_against_independently_computed_values() {
        for x in [0.0_f64, 1.0, 2.0, -1.5] {
            let mut scene = Scene::new();
            variable(&mut scene, "x", x, "pinned");
            variable(&mut scene, "decay", 0.0, "free");
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

            let state = solve(&mut scene, &Author::Agent).expect("solve exponential decay");
            let solved = read(&scene)
                .expect("project solved decay")
                .variables
                .into_iter()
                .find(|variable| variable.name == "decay")
                .expect("decay variable")
                .value;
            let expected = (-2.0 * x).exp();
            eprintln!(
                "formula_decay x={x} solved={solved:.17e} expected={expected:.17e} status={}",
                state.status.as_str()
            );
            assert_eq!(state.status, SolveStatus::Converged);
            // The solver stops once the sum of squared active residuals is
            // at or below `DEFAULT_RESIDUAL_TOLERANCE` (1e-12), and there is
            // exactly one active relation here, so the residual itself —
            // and therefore `|solved - expected|` — is bounded by its
            // square root, a little over 1e-6. 1e-5 leaves comfortable
            // headroom above that bound without being so loose it would
            // pass a genuinely broken evaluator.
            assert!(
                (solved - expected).abs() < 1.0e-5,
                "x={x} solved={solved} expected={expected}"
            );
        }
    }

    /// `ln` of a negative pinned input is a real domain error, not a
    /// transient one the solver could step around: `x` never moves (it is
    /// pinned), so every candidate the line search or the gradient probes
    /// evaluates `ln` at the same negative point and gets the same `NaN`.
    /// `objective` already refuses any non-finite residual from any
    /// relation kind (see the comment beside `ResidualKind::Formula`'s
    /// arm in `residual`), so the whole solve call fails loudly instead of
    /// reporting a converged, exhausted, or relaxed status it cannot
    /// stand behind.
    #[test]
    fn formula_domain_error_fails_the_solve_instead_of_reporting_a_status() {
        let mut scene = Scene::new();
        variable(&mut scene, "x", -1.0, "pinned");
        variable(&mut scene, "log", 0.0, "free");
        create_relation(
            &mut scene,
            "log follows x",
            "formula",
            &["log", "x"],
            None,
            None,
            Some("ln(x)"),
            &Author::Agent,
        )
        .expect("create formula relation with a domain error waiting in it");

        let error = solve(&mut scene, &Author::Agent).expect_err(
            "ln of a negative pinned input must fail the solve honestly, not converge on NaN",
        );
        eprintln!("formula_domain_error={error}");
        assert!(error.contains("log follows x"), "{error}");
        assert!(error.contains("non-finite"), "{error}");
        // No solve state was ever written: the document still reports
        // exactly what it did before the doomed solve was attempted.
        assert!(
            !read(&scene)
                .expect("project after failed solve")
                .solve_state
                .is_projected(),
            "a solve that errored out must not leave behind a status it never reached"
        );
    }

    /// Agent-ink variable names routinely have spaces in them ("barrier
    /// width"), and a formula has to be able to say so without ambiguity —
    /// see `formula.rs`'s module doc comment for why backticks. This is an
    /// end-to-end check that a backtick-quoted name in the expression
    /// resolves to the actual variable of that name when the solver runs,
    /// not just that the parser accepts it (`formula.rs` covers that).
    #[test]
    fn a_formula_with_a_backtick_quoted_spaced_variable_name_solves_correctly() {
        let mut scene = Scene::new();
        variable(&mut scene, "barrier width", 10.0, "pinned");
        variable(&mut scene, "transmitted amplitude", 0.0, "free");
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

        let state = solve(&mut scene, &Author::Agent).expect("solve with a spaced name");
        let solved = read(&scene)
            .expect("project solved amplitude")
            .variables
            .into_iter()
            .find(|variable| variable.name == "transmitted amplitude")
            .expect("transmitted amplitude variable")
            .value;
        let expected = 120.0 * (-0.03_f64 * 10.0).exp();
        eprintln!(
            "spaced_name_formula status={} solved={solved:.17e} expected={expected:.17e}",
            state.status.as_str()
        );
        assert_eq!(state.status, SolveStatus::Converged);
        assert!((solved - expected).abs() < 1.0e-5, "{solved} vs {expected}");
    }

    /// "3 cm equals 30 mm" must converge, and to the *correct* raw value —
    /// not just avoid being refused. `span` starts far from any value that
    /// would satisfy the relation in either unit, so a passing solve here
    /// is real evidence the residual is canonicalizing both sides (0.01 m
    /// on each) rather than comparing the raw numbers 3 and 30 directly
    /// (which would converge on 3, not 30 — the wrong answer in `span`'s
    /// own unit, `mm`).
    #[test]
    fn mm_and_cm_are_equated_with_the_scale_applied_and_converge_to_the_right_raw_value() {
        let mut scene = Scene::new();
        variable_with_unit(&mut scene, "span", 0.0, "free", "mm");
        variable_with_unit(&mut scene, "reference", 3.0, "pinned", "cm");
        create_relation(
            &mut scene,
            "span matches reference",
            "equal",
            &["span", "reference"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("mm and cm share a quantity and must be accepted");

        let state = solve(&mut scene, &Author::Agent).expect("solve the mm/cm equality");
        let solved = read(&scene)
            .expect("project solved span")
            .variables
            .into_iter()
            .find(|variable| variable.name == "span")
            .expect("span variable")
            .value;
        // Independently computed: 3 cm is 0.03 m; a value in mm equal to
        // that is 0.03 m / 0.001 (mm's scale) = 30 mm.
        let expected_mm = 3.0 * 0.01 / 0.001;
        eprintln!(
            "mm_cm_equal status={} solved={solved:.17e} expected={expected_mm:.17e}",
            state.status.as_str()
        );
        assert_eq!(state.status, SolveStatus::Converged);
        assert!(
            (solved - expected_mm).abs() < 1.0e-5,
            "solved={solved} expected={expected_mm}"
        );
    }
}
