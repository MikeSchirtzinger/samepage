use std::collections::{BTreeMap, BTreeSet};

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};
use yrs::{Any, Map, Out, Transact};

use super::{dimension, formula};
use crate::{capacity, one_of, read, text, Atlas, AtlasError, VarState, KIND_VARIABLE, MAX_PATH};

pub const KIND_RELATION: &str = "relation";
pub const RELATION_OPS: &[&str] = &["distance", "angle", "equal", "linear", "formula"];

const K_NAME: &str = "name";
const K_OP: &str = "op";
const K_MEMBERS: &str = "members";
const K_M: &str = "m";
const K_B: &str = "b";
const K_EXPRESSION: &str = "expression";

/// A named numeric relation over persisted variable names.
///
/// `distance` uses `[ax, ay, bx, by, target]`. `angle` uses
/// `[ax, ay, pivot_x, pivot_y, bx, by, target]`. `equal` and `linear` use two
/// members. For `linear`, the first member is the dependent quantity and the
/// second is the source in `dependent = m * source + b`.
///
/// `formula` follows the same "first member is dependent" convention as
/// `linear`: `members[0]` is the quantity the expression predicts and
/// `members[1..]` are the expression's inputs, bound by name. The solver
/// turns this into the residual `expression(members[1..]) - members[0]`
/// (see `solve.rs`'s `ResidualKind::Formula`), so unlike `linear` this is a
/// value the solver drives toward zero rather than a substitution it
/// performs outright — `members[0]` does not have to be free, and does not
/// become derived, just by appearing in a formula relation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Relation {
    pub name: String,
    pub op: String,
    pub members: Vec<String>,
    pub m: Option<f64>,
    pub b: Option<f64>,
    pub expression: Option<String>,
}

fn validated_identity(value: &str, field: &str) -> Result<String, AtlasError> {
    let value = text(value, MAX_PATH, field)?;
    if value.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    if value.contains(',') {
        return Err(format!("{field} cannot contain a comma"));
    }
    Ok(value)
}

fn validate_arity(op: &str, members: &[String]) -> Result<(), AtlasError> {
    let expected = match op {
        "distance" => 5,
        "angle" => 7,
        "equal" | "linear" => 2,
        "formula" if !members.is_empty() => return Ok(()),
        "formula" => {
            return Err("relation op \"formula\" needs at least one member; got 0".to_string())
        }
        _ => return Err(format!("unknown relation op {op:?}")),
    };
    if members.len() != expected {
        return Err(format!(
            "relation op {op:?} needs exactly {expected} members; got {}",
            members.len()
        ));
    }
    Ok(())
}

fn validated_coefficients(
    op: &str,
    m: Option<f64>,
    b: Option<f64>,
) -> Result<(Option<f64>, Option<f64>), AtlasError> {
    match op {
        "linear" => {
            let m = m
                .filter(|value| value.is_finite())
                .ok_or_else(|| "linear relation m must be a finite number".to_string())?;
            let b = b
                .filter(|value| value.is_finite())
                .ok_or_else(|| "linear relation b must be a finite number".to_string())?;
            Ok((Some(m), Some(b)))
        }
        _ if m.is_some() || b.is_some() => Err(format!(
            "relation op {op:?} cannot carry linear coefficients m or b"
        )),
        _ => Ok((None, None)),
    }
}

/// Validate and canonicalize a `formula` relation's expression text.
///
/// A `formula` relation must carry a parseable expression whose every
/// variable reference is one of `members[1..]` (`members[0]` is the
/// dependent quantity the expression predicts, never an input to it — see
/// the doc comment on [`Relation`]). Every other op must carry no
/// expression at all, the same way [`validated_coefficients`] refuses `m`
/// and `b` outside `linear`. Referencing a name the expression declares
/// nothing about — a typo, a variable that lives elsewhere in the document
/// but was never listed as a member here — is refused by name rather than
/// silently evaluated as `NaN`.
fn validated_expression(
    op: &str,
    expression: Option<&str>,
    members: &[String],
) -> Result<Option<String>, AtlasError> {
    match op {
        "formula" => {
            let expression =
                expression.ok_or_else(|| "formula relation needs an expression".to_string())?;
            let expression = text(expression, MAX_PATH, "formula expression")?;
            if expression.is_empty() {
                return Err("formula expression cannot be empty".to_string());
            }
            let parsed = formula::parse(&expression)?;
            let declared = members[1..].iter().cloned().collect::<BTreeSet<_>>();
            for used in formula::variables(&parsed) {
                if !declared.contains(&used) {
                    return Err(format!(
                        "formula expression references {used:?}, which is not one of this relation's declared members"
                    ));
                }
            }
            Ok(Some(expression))
        }
        _ if expression.is_some() => Err(format!(
            "relation op {op:?} cannot carry a formula expression"
        )),
        _ => Ok(None),
    }
}

/// Check that a relation's members carry a physically compatible set of
/// dimensions, using whatever `unit` each named variable currently carries
/// in `atlas`.
///
/// Called twice, for two different purposes: eagerly from [`create_relation`],
/// where a mismatch refuses the write outright; and forgivingly from
/// `describe.rs`'s read-back, where a mismatch that slipped in from a
/// document written before this check existed (or from a relation whose
/// member later gained an incompatible unit through some other write path)
/// is reported as a note instead of hidden. A member name that no longer
/// resolves to a living variable is not this function's problem to report
/// — that is a dangling reference, a different failure mode entirely — so
/// it is treated as nothing to check here rather than an error.
///
/// Each op's requirement follows directly from how `solve.rs`'s `System`
/// actually uses these members:
///
/// - `equal`: both members feed directly into one residual
///   (`values[left] - values[right]`, scaled — see `ResidualKind::Equal`),
///   so they must share a quantity; a scale difference (mm vs cm) is fine
///   and is converted for the solve, never refused.
/// - `distance`: `ax`/`ay`/`bx`/`by` are subtracted pairwise inside a
///   Euclidean norm and the result is compared to `target`
///   (`ResidualKind::Distance`), so all five must share one quantity — a
///   distance and its coordinates are the same kind of thing, a length
///   (or all five may be bare/pixel/whatever else, as long as they agree).
/// - `angle`: `ax`/`ay`/`px`/`py`/`bx`/`by` are the same kind of
///   subtraction-then-cross-product arithmetic as `distance`'s positions,
///   so they must share one quantity among themselves; but `target` is
///   compared directly to an `atan2` result in radians
///   (`ResidualKind::Angle`), a genuinely different physical quantity from
///   a position, so it is checked separately against "angle or no unit"
///   rather than against the positions' quantity.
/// - `linear`: `materialize()` computes `dependent = m * source + b` in
///   whatever raw units `source` and `b` (a bare coefficient, not a
///   variable — it never had a unit to check) happen to be in, with no
///   scale conversion at all; `m` is trusted to already carry whatever
///   ratio bridges `source`'s unit to `dependent`'s (see this file's
///   `Relation` doc comment). There is nothing to compare `dependent`
///   against, since `b` carries no separate unit of its own — its
///   dimension is, by construction, whatever `dependent`'s already is. The
///   one thing worth checking is that `dependent`'s own unit is not a
///   legacy string this module can no longer parse (see
///   `dimension::resolve`), since a linear relation would otherwise start
///   silently trusting a `derived` value whose unit nobody can any longer
///   verify.
/// - `formula`: see [`dimension::check_formula_arguments`]'s doc comment
///   for exactly what is checked (the arguments of `exp`/`ln`/`log10`/
///   `sin`/`cos`/`tan`) and, just as importantly, what is not: this module
///   makes no attempt at full dimensional inference through an arbitrary
///   expression, so `y = 3 * x` with `y` in `mm` and `x` in `px` is
///   accepted even though it is exactly as physically meaningless as
///   `y equals x` would be. Catching every possible formula-shaped
///   dimension error is more inference than this feature was asked to
///   build; the function-argument check catches the sharpest, most common
///   real mistake (`exp`/`sin`/`ln`/... of something that is not a bare
///   number) cheaply and soundly.
pub(super) fn dimension_check(
    atlas: &Atlas,
    name: &str,
    op: &str,
    members: &[String],
    expression: Option<&str>,
) -> Result<(), AtlasError> {
    let variable = |member: &str| atlas.variables.iter().find(|v| v.name == member);
    let context = || format!("relation {name:?}");
    match op {
        "equal" => {
            let (Some(left), Some(right)) = (variable(&members[0]), variable(&members[1])) else {
                return Ok(());
            };
            dimension::require_same_quantity(
                &context(),
                &left.name,
                left.unit.as_deref(),
                &right.name,
                right.unit.as_deref(),
            )
            .map(|_| ())
        }
        "linear" => {
            let Some(dependent) = variable(&members[0]) else {
                return Ok(());
            };
            dimension::resolve(&dependent.name, dependent.unit.as_deref()).map(|_| ())
        }
        "distance" => {
            let Some(vars) = members
                .iter()
                .map(|member| variable(member))
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(());
            };
            for pair in vars.windows(2) {
                dimension::require_same_quantity(
                    &context(),
                    &pair[0].name,
                    pair[0].unit.as_deref(),
                    &pair[1].name,
                    pair[1].unit.as_deref(),
                )?;
            }
            Ok(())
        }
        "angle" => {
            let Some(vars) = members
                .iter()
                .map(|member| variable(member))
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(());
            };
            for pair in vars[..6].windows(2) {
                dimension::require_same_quantity(
                    &context(),
                    &pair[0].name,
                    pair[0].unit.as_deref(),
                    &pair[1].name,
                    pair[1].unit.as_deref(),
                )?;
            }
            dimension::require_angle_like(&context(), &vars[6].name, vars[6].unit.as_deref())
                .map(|_| ())
        }
        "formula" => {
            let Some(expression) = expression else {
                return Ok(());
            };
            let expr = formula::parse(expression)?;
            let quantities = members[1..]
                .iter()
                .filter_map(|member| variable(member).map(|v| (member.as_str(), v.unit.as_deref())))
                .filter_map(|(member, unit)| dimension::quantity_of(unit).map(|q| (member, q)))
                .collect::<BTreeMap<_, _>>();
            dimension::check_formula_arguments(&expr, &quantities)
        }
        _ => Ok(()),
    }
}

/// Create a validated relation. Linear relations also make their first member
/// derived and materialize its substituted value.
#[allow(clippy::too_many_arguments)]
pub fn create_relation(
    scene: &mut Scene,
    name: &str,
    op: &str,
    members: &[&str],
    m: Option<f64>,
    b: Option<f64>,
    expression: Option<&str>,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    capacity(&atlas)?;

    let name = validated_identity(name, "relation name")?;
    if atlas.relations.iter().any(|relation| relation.name == name) {
        return Err(format!(
            "relation name {name:?} already exists; relation names must be unique"
        ));
    }
    let op = one_of(op, RELATION_OPS, "relation op")?;
    let members = members
        .iter()
        .map(|member| validated_identity(member, "relation member"))
        .collect::<Result<Vec<_>, _>>()?;
    validate_arity(&op, &members)?;
    if members.iter().collect::<BTreeSet<_>>().len() != members.len() {
        return Err("a relation cannot contain duplicate members".to_string());
    }
    for member in &members {
        if !atlas
            .variables
            .iter()
            .any(|variable| variable.name == *member)
        {
            return Err(format!(
                "relation member {member:?} does not resolve to an agent-ink variable"
            ));
        }
    }
    let (m, b) = validated_coefficients(&op, m, b)?;
    let expression = validated_expression(&op, expression, &members)?;
    dimension_check(&atlas, &name, &op, &members, expression.as_deref())?;

    let derived_update = if op == "linear" {
        let dependent = atlas
            .variables
            .iter()
            .find(|variable| variable.name == members[0])
            .ok_or_else(|| format!("linear dependent variable {:?} is missing", members[0]))?;
        if dependent.state != VarState::Free {
            return Err(format!(
                "linear dependent variable {:?} must be free before it becomes derived; got {}",
                dependent.name,
                dependent.state.as_str()
            ));
        }
        if atlas.relations.iter().any(|relation| {
            relation.op == "linear" && relation.members.first() == Some(&dependent.name)
        }) {
            return Err(format!(
                "variable {:?} already has a linear derivation",
                dependent.name
            ));
        }
        let source = atlas
            .variables
            .iter()
            .find(|variable| variable.name == members[1])
            .ok_or_else(|| format!("linear source variable {:?} is missing", members[1]))?;
        let (Some(m), Some(b)) = (m, b) else {
            return Err("linear relation is missing validated coefficients".to_string());
        };
        let value = m * source.value + b;
        if !value.is_finite() {
            return Err("linear substitution produced a non-finite value".to_string());
        }
        Some((
            variable_object_id(scene, &dependent.name)?,
            dependent.name.clone(),
            value,
        ))
    } else {
        None
    };

    let mut props = vec![
        (K_NAME, PropValue::Str(name)),
        (K_OP, PropValue::Str(op)),
        (K_MEMBERS, PropValue::Str(members.join(","))),
    ];
    if let (Some(m), Some(b)) = (m, b) {
        props.push((K_M, PropValue::Num(m)));
        props.push((K_B, PropValue::Num(b)));
    }
    if let Some(expression) = &expression {
        props.push((K_EXPRESSION, PropValue::Str(expression.clone())));
    }
    let id = scene
        .create_object_with_props(KIND_RELATION, author.clone(), &props)
        .map_err(|error| format!("could not create relation: {error}"))?;

    if let Some((dependent_id, dependent_name, value)) = derived_update {
        if let Err(error) = scene.set_props(
            &dependent_id,
            &[
                ("state", PropValue::Str("derived".to_string())),
                ("value", PropValue::Num(value)),
            ],
        ) {
            let cleanup = scene.delete_object(&id);
            return match cleanup {
                Ok(()) => Err(format!(
                    "could not make variable {dependent_name:?} derived: {error}"
                )),
                Err(cleanup_error) => Err(format!(
                    "could not make variable {dependent_name:?} derived: {error}; could not remove the incomplete relation: {cleanup_error}"
                )),
            };
        }
    }

    Ok(id.into_string())
}

pub(super) fn project(
    string: &impl Fn(&str) -> String,
    number: &impl Fn(&str) -> Option<f64>,
) -> Result<Relation, AtlasError> {
    let stored_name = string(K_NAME);
    let name = validated_identity(&stored_name, "relation name")?;
    if name != stored_name {
        return Err("stored relation name cannot have surrounding whitespace".to_string());
    }
    let op = one_of(&string(K_OP), RELATION_OPS, "relation op")?;
    let members = string(K_MEMBERS)
        .split(',')
        .map(|member| validated_identity(member, "relation member"))
        .collect::<Result<Vec<_>, _>>()?;
    validate_arity(&op, &members)?;
    if members.iter().collect::<BTreeSet<_>>().len() != members.len() {
        return Err("stored relation cannot contain duplicate members".to_string());
    }
    let (m, b) = validated_coefficients(&op, number(K_M), number(K_B))?;
    let stored_expression = string(K_EXPRESSION);
    let expression = (!stored_expression.is_empty()).then_some(stored_expression.as_str());
    let expression = validated_expression(&op, expression, &members)?;
    Ok(Relation {
        name,
        op,
        members,
        m,
        b,
        expression,
    })
}

pub(super) fn variable_object_ids(scene: &Scene) -> Result<BTreeMap<String, String>, AtlasError> {
    let doc = scene.doc();
    let objects = doc.get_or_insert_map("objects");
    let txn = doc
        .try_transact()
        .map_err(|error| format!("atlas is busy: {error}"))?;
    let mut ids = BTreeMap::new();
    for (id, value) in objects.iter(&txn) {
        let Out::YMap(map) = value else { continue };
        let stored_string = |key: &str| match map.get(&txn, key) {
            Some(Out::Any(Any::String(value))) => value.to_string(),
            _ => String::new(),
        };
        if stored_string("kind") != KIND_VARIABLE {
            continue;
        }
        let name = stored_string("name");
        if ids.insert(name.clone(), id.to_string()).is_some() {
            return Err(format!(
                "variable name {name:?} already exists; variable names must be unique"
            ));
        }
    }
    Ok(ids)
}

fn variable_object_id(scene: &Scene, name: &str) -> Result<ObjectId, AtlasError> {
    variable_object_ids(scene)?
        .remove(name)
        .map(ObjectId::from)
        .ok_or_else(|| format!("variable {name:?} has no persisted object"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_ink::create_variable;

    fn variable(scene: &mut Scene, name: &str, value: f64, state: &str) {
        create_variable(scene, name, value, state, None, None, &Author::Agent)
            .expect("create variable");
    }

    fn variable_with_unit(scene: &mut Scene, name: &str, value: f64, state: &str, unit: &str) {
        create_variable(scene, name, value, state, None, Some(unit), &Author::Agent)
            .expect("create variable with unit");
    }

    /// Writes a variable object directly, bypassing `create_variable`'s
    /// validation entirely, to stand in for a document written before this
    /// crate's unit-dimension check existed (or edited by something other
    /// than this crate's own API). `create_variable` itself can never
    /// produce a stored unit that fails `dimension::parse` — see
    /// `variable.rs`'s create-time check — so this is the only way to get
    /// one into a `Scene` at all.
    fn legacy_variable_with_unrecognized_unit(
        scene: &mut Scene,
        name: &str,
        value: f64,
        unit: &str,
    ) -> String {
        scene
            .create_object_with_props(
                KIND_VARIABLE,
                Author::Agent,
                &[
                    ("name", PropValue::Str(name.to_string())),
                    ("value", PropValue::Num(value)),
                    ("state", PropValue::Str("pinned".to_string())),
                    ("represents_object", PropValue::Str(String::new())),
                    ("represents_property", PropValue::Str(String::new())),
                    ("unit", PropValue::Str(unit.to_string())),
                ],
            )
            .expect("write legacy variable object directly")
            .into_string()
    }

    #[test]
    fn a_malformed_formula_expression_is_refused_at_create_time_naming_the_bad_token() {
        let mut scene = Scene::new();
        variable(&mut scene, "y", 0.0, "free");
        variable(&mut scene, "x", 1.0, "pinned");
        let error = create_relation(
            &mut scene,
            "y follows x",
            "formula",
            &["y", "x"],
            None,
            None,
            Some("2 + * x"),
            &Author::Agent,
        )
        .expect_err("a dangling `*` must be refused, not silently parsed as something");
        eprintln!("malformed_formula_error={error}");
        assert!(error.contains('*'), "{error}");
    }

    #[test]
    fn a_formula_referencing_an_unknown_variable_is_refused_at_create_time() {
        let mut scene = Scene::new();
        variable(&mut scene, "y", 0.0, "free");
        variable(&mut scene, "x", 1.0, "pinned");
        let error = create_relation(
            &mut scene,
            "y follows x",
            "formula",
            &["y", "x"],
            None,
            None,
            Some("exp(-2*z)"),
            &Author::Agent,
        )
        .expect_err("\"z\" is not a declared member and must be refused by name");
        eprintln!("unknown_variable_error={error}");
        assert!(error.contains('z'), "{error}");
    }

    /// Agent-ink variable names routinely contain spaces ("barrier
    /// width"), which a bare identifier token cannot spell; a formula
    /// wraps such a name in backticks instead (see `formula.rs`'s module
    /// doc comment for why backticks). A quoted name still has to name a
    /// real, declared member — quoting does not exempt it from the same
    /// check a bare name gets.
    #[test]
    fn a_quoted_but_unknown_variable_name_is_refused_at_create_time() {
        let mut scene = Scene::new();
        variable(&mut scene, "amplitude", 0.0, "free");
        variable(&mut scene, "barrier width", 1.0, "pinned");
        let error = create_relation(
            &mut scene,
            "amplitude decays exponentially with barrier width",
            "formula",
            &["amplitude", "barrier width"],
            None,
            None,
            Some("120 * exp(-0.03 * `unknown name`)"),
            &Author::Agent,
        )
        .expect_err("a quoted name still has to be a declared member");
        eprintln!("quoted_unknown_variable_error={error}");
        assert!(error.contains("unknown name"), "{error}");
    }

    /// The expression text — backticked spaced name and all — must survive
    /// a real CRDT encode/decode round trip byte-for-byte, not merely stay
    /// intact within one in-memory `Scene`.
    #[test]
    fn a_formula_expression_with_a_quoted_name_survives_a_serialized_document_reload() {
        let mut scene = Scene::new();
        variable(&mut scene, "amplitude", 0.0, "free");
        variable(&mut scene, "barrier width", 10.0, "pinned");
        create_relation(
            &mut scene,
            "amplitude decays exponentially with barrier width",
            "formula",
            &["amplitude", "barrier width"],
            None,
            None,
            Some("120 * exp(-0.03 * `barrier width`)"),
            &Author::Agent,
        )
        .expect("create formula relation with a spaced variable name");

        let serialized = scene.encode_full().expect("serialize document");
        let reloaded = Scene::from_state(&serialized).expect("reload serialized document");
        let atlas = read(&reloaded).expect("project reloaded document");
        let relation = atlas
            .relations
            .iter()
            .find(|relation| relation.name == "amplitude decays exponentially with barrier width")
            .expect("read relation back by name");

        assert_eq!(
            relation.expression.as_deref(),
            Some("120 * exp(-0.03 * `barrier width`)")
        );
    }

    #[test]
    fn a_formula_expression_is_refused_on_every_other_op() {
        let mut scene = Scene::new();
        variable(&mut scene, "a", 0.0, "pinned");
        variable(&mut scene, "b", 0.0, "pinned");
        let error = create_relation(
            &mut scene,
            "a equals b",
            "equal",
            &["a", "b"],
            None,
            None,
            Some("a"),
            &Author::Agent,
        )
        .expect_err("only formula relations may carry an expression");
        eprintln!("expression_on_equal_error={error}");
        assert!(error.contains("equal"), "{error}");
    }

    /// The whole point of this feature: "10 nm" and "20 px" are not the
    /// same kind of thing, and equating them must be refused before the
    /// solver ever gets to converge on a number that means nothing.
    #[test]
    fn equating_a_length_to_a_pixel_variable_is_refused_naming_both_units() {
        let mut scene = Scene::new();
        variable_with_unit(&mut scene, "molecule width", 10.0, "pinned", "nm");
        variable_with_unit(&mut scene, "icon width", 20.0, "free", "px");
        let error = create_relation(
            &mut scene,
            "molecule tracks icon",
            "equal",
            &["molecule width", "icon width"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect_err("nm and px must never be equated");
        eprintln!("length_equals_pixel_error={error}");
        assert!(error.contains("nm"), "{error}");
        assert!(error.contains("px"), "{error}");
        assert!(error.contains("molecule width"), "{error}");
        assert!(error.contains("icon width"), "{error}");
    }

    /// `e^(5 metres)` is not a number; the formula's declared-member check
    /// (above) refuses an *unknown* variable, but this variable is entirely
    /// legitimate — it is only the wrong dimension for what `exp` accepts.
    #[test]
    fn exp_of_a_dimensioned_argument_is_refused() {
        let mut scene = Scene::new();
        variable_with_unit(&mut scene, "displacement", 5.0, "pinned", "mm");
        variable(&mut scene, "amplitude", 0.0, "free");
        let error = create_relation(
            &mut scene,
            "amplitude follows displacement",
            "formula",
            &["amplitude", "displacement"],
            None,
            None,
            Some("exp(displacement)"),
            &Author::Agent,
        )
        .expect_err("exp of a length must be refused");
        eprintln!("exp_of_dimensioned_argument_error={error}");
        assert!(error.contains("exp"), "{error}");
        assert!(error.contains("length"), "{error}");
    }

    /// A distance relation's coordinates and target must all be the same
    /// kind of thing; mixing a pixel coordinate into an otherwise-bare
    /// distance is exactly as meaningless as mixing nm into px.
    #[test]
    fn a_distance_relation_mixing_quantities_is_refused() {
        let mut scene = Scene::new();
        variable(&mut scene, "ax", 0.0, "pinned");
        variable(&mut scene, "ay", 0.0, "pinned");
        variable_with_unit(&mut scene, "bx", 3.0, "free", "px");
        variable(&mut scene, "by", 4.0, "free");
        variable(&mut scene, "radius", 5.0, "pinned");
        let error = create_relation(
            &mut scene,
            "mixed distance",
            "distance",
            &["ax", "ay", "bx", "by", "radius"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect_err("a pixel coordinate mixed with bare coordinates must be refused");
        eprintln!("mixed_distance_error={error}");
        assert!(error.contains("px"), "{error}");
        assert!(error.contains("bx"), "{error}");
    }

    /// An angle relation's target is compared to radians directly (see
    /// `solve.rs`'s `ResidualKind::Angle`), so a target carrying a length
    /// unit is refused even though the positions it pivots around are
    /// perfectly consistent with each other.
    #[test]
    fn an_angle_relation_with_a_length_target_is_refused() {
        let mut scene = Scene::new();
        variable(&mut scene, "ax", 1.0, "pinned");
        variable(&mut scene, "ay", 0.0, "pinned");
        variable(&mut scene, "pivot-x", 0.0, "pinned");
        variable(&mut scene, "pivot-y", 0.0, "pinned");
        variable(&mut scene, "bx", 0.0, "pinned");
        variable(&mut scene, "by", 1.0, "pinned");
        variable_with_unit(&mut scene, "opening", 90.0, "pinned", "mm");
        let error = create_relation(
            &mut scene,
            "bad angle target",
            "angle",
            &["ax", "ay", "pivot-x", "pivot-y", "bx", "by", "opening"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect_err("a length cannot stand in for an angle");
        eprintln!("angle_length_target_error={error}");
        assert!(error.contains("opening"), "{error}");
        assert!(error.contains("mm"), "{error}");
    }

    /// A `linear` dependent may have any physical unit at all while its
    /// source stays bare — `m` carries the ratio between them, so this must
    /// keep working exactly as it did before this module existed (see
    /// `describe.rs`'s `real_step_advance_describes_motion_amount_and_held_values`,
    /// which relies on precisely this pairing).
    #[test]
    fn a_linear_relation_may_pair_a_dimensioned_dependent_with_a_bare_source() {
        let mut scene = Scene::new();
        variable_with_unit(&mut scene, "arm length", 0.0, "free", "mm");
        variable(&mut scene, "step", 0.0, "scrubbing");
        create_relation(
            &mut scene,
            "arm follows step",
            "linear",
            &["arm length", "step"],
            Some(20.0),
            Some(120.0),
            None,
            &Author::Agent,
        )
        .expect("a dimensioned dependent with a bare source must still be accepted");
    }

    /// A document written before this module existed can carry a unit
    /// string this module no longer parses. It must still load, and its
    /// variable must still read back with the exact stored value and unit
    /// — but a *new* relation that would need to check that variable's
    /// dimension is refused, naming the unrecognized unit, until it is
    /// corrected.
    #[test]
    fn a_legacy_unrecognized_unit_loads_but_blocks_a_new_dimension_checked_relation() {
        let mut scene = Scene::new();
        legacy_variable_with_unrecognized_unit(&mut scene, "wingspan", 12.0, "smoots");
        variable(&mut scene, "target", 12.0, "pinned");

        let atlas = read(&scene).expect("a legacy unrecognized unit must not block loading");
        let wingspan = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "wingspan")
            .expect("the legacy variable still reads back");
        assert_eq!(wingspan.value, 12.0);
        assert_eq!(wingspan.unit.as_deref(), Some("smoots"));

        let error = create_relation(
            &mut scene,
            "wingspan tracks target",
            "equal",
            &["wingspan", "target"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect_err("a new relation naming a legacy unrecognized unit must be refused");
        eprintln!("legacy_unrecognized_unit_error={error}");
        assert!(error.contains("smoots"), "{error}");
        assert!(error.contains("wingspan"), "{error}");
    }
}
