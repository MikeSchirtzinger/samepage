//! Give an agent-ink variable's `unit` string real meaning: a base physical
//! quantity plus a scale factor, checked whenever a relation ties two
//! variables together, instead of the free-text label it used to be.
//!
//! Before this module, `unit` was purely a display string (see
//! `variable.rs`): "10 nm" and "20 px" could be told `equal` and the solver
//! would converge on a number relating a molecule's width to a handful of
//! screen pixels, and a rendered diagram would look exactly as reasonable
//! either way. This module is what makes that a refused write instead of a
//! silently wrong one.
//!
//! # Why `px` is its own quantity, not a length
//!
//! `px` is a screen unit: a shape's width in `px` is a count of pixels in
//! the rendered canvas, not a physical distance, and nothing about zooming
//! or scrubbing the document ever converts it the way a change of physical
//! unit would convert a length. The whole point of this module is to catch
//! exactly the mistake of equating "10 nm" to "20 px". If `px` were folded
//! into [`Quantity::Length`] at some fixed ratio (as if it were just
//! another length unit, the way a browser might say "96 px per inch"),
//! that exact mistake would silently pass as a legal, if aggressive, unit
//! conversion instead of being refused. So `px` gets its own
//! [`Quantity::Pixel`]: a `px` variable can be related to another `px`
//! variable, never to a physical length, time, or angle, without an
//! explicit, deliberate bridge this module does not provide.
//!
//! This does mean a variable bound to a shape's `x`/`y`/`w`/`h` (see
//! `geometry.rs`) is not automatically typed `px` — `represents` and `unit`
//! are independent fields on [`super::Variable`], and every
//! `represents`-bound variable in this crate's own tests leaves `unit`
//! unset. Requiring every geometry-bound variable to suddenly carry an
//! explicit `px` unit would have made ordinary bound-geometry relations
//! (a shape's `x` tracking a plain, unitless target number) impossible to
//! create without a change nobody asked for; leaving `unit` an independent,
//! opt-in field keeps that existing, real work possible. Declaring `px`
//! (or any other unit) on a bound variable is something a caller chooses,
//! not something this module infers from the binding.
//!
//! # `Bare` is dimensionless, not "unknown"
//!
//! A variable created with no `unit` at all is genuinely dimensionless for
//! the purposes of this module — the same as one explicitly labeled
//! `"ratio"` — not a wildcard that matches everything. Two bare variables
//! (or a bare variable and a `"ratio"` one) are compatible with each other;
//! a bare variable and an explicitly `"mm"` one are not, the same as a
//! `"ratio"` variable and an `"mm"` one would not be. This is what keeps
//! every relation created before this module existed compiling and
//! solving identically: those relations only ever tied bare variables to
//! other bare variables (or, for `linear`, a bare source to a dependent
//! variable — see [`super::relation::dimension_check`]'s `"linear"` arm for
//! why that pairing needs no dimension check at all), and two bare
//! variables are, by this rule, a dimension match.

use std::collections::BTreeMap;

use super::formula::Expr;
use crate::AtlasError;

/// The base physical quantities this module tells apart. Two variables can
/// only be compared, added, or converted between when they carry the same
/// quantity; [`Dimension::scale`] only ever converts *within* one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Quantity {
    Length,
    /// Screen pixels — never a length. See the module doc comment.
    Pixel,
    Time,
    Angle,
    Dimensionless,
}

impl Quantity {
    fn name(self) -> &'static str {
        match self {
            Quantity::Length => "length",
            Quantity::Pixel => "pixel",
            Quantity::Time => "time",
            Quantity::Angle => "angle",
            Quantity::Dimensionless => "dimensionless",
        }
    }
}

/// A recognized unit: a quantity plus the factor that converts a value in
/// this unit into the quantity's canonical unit (meter for length, the
/// pixel itself for [`Quantity::Pixel`], second for time, radian for
/// angle, the bare ratio for dimensionless). Two variables of the same
/// quantity but different units — millimeters and centimeters, say — are
/// exactly the case a caller can relate honestly: convert both raw values
/// to the canonical unit with their own scale before comparing them (see
/// `solve.rs`'s use of [`quantity_scale`]), so the comparison means
/// something regardless of which of the two units either variable happened
/// to be authored in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Dimension {
    pub(super) quantity: Quantity,
    pub(super) scale: f64,
}

/// Every unit string this module recognizes, and the dimension it means.
/// `um` and `µm` are the same unit spelled two ways: the ASCII spelling
/// exists because a stray micro sign is easy to lose typing an expression
/// through a shell or a JSON string, not because the two units differ.
fn recognized(unit: &str) -> Option<Dimension> {
    let (quantity, scale) = match unit {
        "nm" => (Quantity::Length, 1.0e-9),
        "um" | "µm" => (Quantity::Length, 1.0e-6),
        "mm" => (Quantity::Length, 1.0e-3),
        "cm" => (Quantity::Length, 1.0e-2),
        "m" => (Quantity::Length, 1.0),
        "in" => (Quantity::Length, 0.0254),
        "px" => (Quantity::Pixel, 1.0),
        "ms" => (Quantity::Time, 1.0e-3),
        "s" => (Quantity::Time, 1.0),
        "min" => (Quantity::Time, 60.0),
        "deg" => (Quantity::Angle, std::f64::consts::PI / 180.0),
        "rad" => (Quantity::Angle, 1.0),
        "%" => (Quantity::Dimensionless, 0.01),
        "ratio" => (Quantity::Dimensionless, 1.0),
        _ => return None,
    };
    Some(Dimension { quantity, scale })
}

/// Parse a unit string a caller is about to attach to a variable. Called
/// only from [`super::create_variable`] — never while projecting a stored
/// document, see [`VariableDimension::of`] — so an unrecognized string here
/// is a real, write-refusing error, naming exactly the string that failed
/// to parse.
pub(super) fn parse(unit: &str) -> Result<Dimension, AtlasError> {
    recognized(unit).ok_or_else(|| {
        format!(
            "variable unit {unit:?} is not a recognized dimension; expected one of nm, um, µm, mm, cm, m, in, px, ms, s, min, deg, rad, %, ratio, or no unit at all"
        )
    })
}

/// A variable's dimension as read back from whatever `unit` string is
/// actually stored, which may predate this module and so may not parse.
#[derive(Debug, Clone, PartialEq)]
enum VariableDimension {
    /// No unit was ever set. Dimensionless, the same as `"ratio"` — see the
    /// module doc comment for why this is a real dimension and not a
    /// wildcard.
    Bare,
    /// A unit string that parses cleanly.
    Known(Dimension),
    /// A unit string that does not parse as any recognized dimension. This
    /// can only happen to a variable loaded from a document written before
    /// this module existed (or edited outside this crate's API):
    /// [`super::create_variable`] itself refuses an unrecognized unit
    /// outright, so no variable created through this crate's own API can
    /// ever carry one. The document still opens, and this variable's
    /// stored value and unit string still read back exactly as they did
    /// before this module existed (see `variable.rs`'s `project`); only a
    /// *new* relation naming this variable in a dimension-checked position
    /// is refused, by the unit string itself, until the unit is corrected.
    Unrecognized(String),
}

impl VariableDimension {
    fn of(unit: Option<&str>) -> Self {
        match unit {
            None => VariableDimension::Bare,
            Some(unit) => match recognized(unit) {
                Some(dimension) => VariableDimension::Known(dimension),
                None => VariableDimension::Unrecognized(unit.to_string()),
            },
        }
    }
}

/// Resolve `unit` into a dimension usable in a dimension-checked relation,
/// naming `variable` in the error if the stored unit does not parse. This
/// is the only place a legacy unrecognized unit actually stops anything —
/// see [`VariableDimension::Unrecognized`]'s doc comment.
pub(super) fn resolve(variable: &str, unit: Option<&str>) -> Result<Dimension, AtlasError> {
    match VariableDimension::of(unit) {
        VariableDimension::Bare => Ok(Dimension {
            quantity: Quantity::Dimensionless,
            scale: 1.0,
        }),
        VariableDimension::Known(dimension) => Ok(dimension),
        VariableDimension::Unrecognized(stored) => Err(format!(
            "variable {variable:?} has unit {stored:?}, which is not a recognized dimension; it cannot be used in a dimension-checked relation until its unit is corrected"
        )),
    }
}

/// The quantity `unit` names, or `None` for a unit this module cannot
/// place at all (an [`VariableDimension::Unrecognized`] legacy string).
/// Used only where a caller is willing to treat "unknown" as "not provably
/// anything in particular" rather than as a hard error — see
/// `check_formula_arguments`'s inference, which is exactly that kind of
/// conservative caller.
pub(super) fn quantity_of(unit: Option<&str>) -> Option<Quantity> {
    match VariableDimension::of(unit) {
        VariableDimension::Bare => Some(Quantity::Dimensionless),
        VariableDimension::Known(dimension) => Some(dimension.quantity),
        VariableDimension::Unrecognized(_) => None,
    }
}

/// The factor that converts `unit`'s raw stored value into its quantity's
/// canonical unit, for use in the solver's residual arithmetic
/// (`solve.rs`). An [`VariableDimension::Unrecognized`] legacy unit falls
/// back to `1.0` — the same as a bare variable — rather than failing the
/// solve outright: a relation naming this variable could only have been
/// created before this module existed (a *new* one is refused at
/// `create_relation` time, see [`resolve`]), and a solve that already runs
/// today should keep running exactly as it did, in the same raw units it
/// always used, rather than starting to fail because a display string it
/// never checked before turned out not to parse.
pub(super) fn quantity_scale(unit: Option<&str>) -> f64 {
    match VariableDimension::of(unit) {
        VariableDimension::Bare => 1.0,
        VariableDimension::Known(dimension) => dimension.scale,
        VariableDimension::Unrecognized(_) => 1.0,
    }
}

fn unit_label(unit: Option<&str>) -> &str {
    unit.unwrap_or("(no unit)")
}

/// Require that two variables' resolved dimensions describe the same
/// physical quantity, returning each one's scale-to-canonical factor so a
/// caller can convert both raw values before comparing them — millimeters
/// and centimeters are equal at the right ratio, never at face value.
pub(super) fn require_same_quantity(
    context: &str,
    left_name: &str,
    left_unit: Option<&str>,
    right_name: &str,
    right_unit: Option<&str>,
) -> Result<(f64, f64), AtlasError> {
    let left = resolve(left_name, left_unit)?;
    let right = resolve(right_name, right_unit)?;
    if left.quantity != right.quantity {
        return Err(format!(
            "{context} cannot relate {left_name:?} ({}, {}) to {right_name:?} ({}, {}): {} is not the same dimension as {}",
            unit_label(left_unit),
            left.quantity.name(),
            unit_label(right_unit),
            right.quantity.name(),
            left.quantity.name(),
            right.quantity.name(),
        ));
    }
    Ok((left.scale, right.scale))
}

/// Require that `name` can stand in for an angle: either an explicit
/// `Quantity::Angle` unit (`deg` or `rad`) or no unit at all (the untyped
/// legacy assumption that an angle target is already a plain radian
/// value — see `solve.rs`'s `ResidualKind::Angle`, which compares it
/// directly to an `atan2` result). Anything else — a length, a pixel
/// count, a time — is refused: those are real, different physical
/// quantities that an angle relation's target was never meant to hold.
pub(super) fn require_angle_like(
    context: &str,
    name: &str,
    unit: Option<&str>,
) -> Result<f64, AtlasError> {
    let dimension = resolve(name, unit)?;
    match dimension.quantity {
        Quantity::Angle | Quantity::Dimensionless => Ok(dimension.scale),
        other => Err(format!(
            "{context} target {name:?} has unit {} ({}), which cannot be used as an angle; expected deg, rad, or no unit",
            unit_label(unit),
            other.name()
        )),
    }
}

/// The function names whose argument must be dimensionless: raising `e` to
/// a power that carries units, or taking the sine of a length, has no
/// physical meaning — unlike `sqrt` (whose argument's dimension survives,
/// halved in a sense this module has no vocabulary to name) or `abs`
/// (whose argument's dimension passes straight through), neither of which
/// is checked here.
const REQUIRES_DIMENSIONLESS_ARGUMENT: &[&str] = &["exp", "ln", "log10", "sin", "cos", "tan"];

/// A conservative, structural guess at an expression's quantity, used only
/// to check the arguments of [`REQUIRES_DIMENSIONLESS_ARGUMENT`]'s
/// functions. This is *not* full dimensional analysis — it combines
/// quantities only in the handful of shapes where the result is
/// unambiguous:
///
/// - `+`/`-`: both sides must already be the same quantity; the sum is
///   that quantity.
/// - `*`/`/`: one side must be dimensionless, in which case the result is
///   the other side's quantity — or, for `/` only, both sides may be the
///   *same* quantity, which cancels to dimensionless.
/// - `^`: both the base and the exponent must be dimensionless.
/// - a named function call: `exp`/`ln`/`log10`/`sin`/`cos`/`tan` always
///   return a dimensionless result (their argument is separately required
///   to be dimensionless by `check_formula_arguments`, which is why this
///   arm does not need to re-derive it); every other function's result
///   quantity passes its argument's quantity straight through, which is
///   exactly right for `abs` and only accidentally not wrong for `sqrt`
///   (a genuinely dimensioned argument to `sqrt` infers to `None` here
///   only if the argument itself was not already resolvable, since this
///   module has no "square root of a quantity" vocabulary entry to give
///   it — see the module doc comment's `Quantity` list).
///
/// Every other shape — multiplying two lengths, raising a length to a
/// power, anything nested under a shape this function does not recognize
/// — infers to `None`, "cannot prove this is dimensionless," which is
/// exactly the argument [`check_formula_arguments`] refuses. This is
/// deliberately biased toward false alarms over false confidence: some
/// expressions that are genuinely dimensionless by construction (a length
/// divided by a length, buried inside an addition this function cannot see
/// through) will be refused as a false positive, but nothing dimensioned
/// is ever silently accepted as safe.
fn infer(expr: &Expr, quantities: &BTreeMap<&str, Quantity>) -> Option<Quantity> {
    match expr {
        Expr::Num(_) => Some(Quantity::Dimensionless),
        Expr::Var(name) => quantities.get(name.as_str()).copied(),
        Expr::Neg(inner) => infer(inner, quantities),
        Expr::Add(left, right) | Expr::Sub(left, right) => {
            match (infer(left, quantities), infer(right, quantities)) {
                (Some(left), Some(right)) if left == right => Some(left),
                _ => None,
            }
        }
        Expr::Mul(left, right) => match (infer(left, quantities), infer(right, quantities)) {
            (Some(Quantity::Dimensionless), Some(other)) => Some(other),
            (Some(other), Some(Quantity::Dimensionless)) => Some(other),
            _ => None,
        },
        Expr::Div(left, right) => match (infer(left, quantities), infer(right, quantities)) {
            (Some(left), Some(right)) if left == right => Some(Quantity::Dimensionless),
            (Some(left), Some(Quantity::Dimensionless)) => Some(left),
            _ => None,
        },
        Expr::Pow(base, exponent) => match (infer(base, quantities), infer(exponent, quantities)) {
            (Some(Quantity::Dimensionless), Some(Quantity::Dimensionless)) => {
                Some(Quantity::Dimensionless)
            }
            _ => None,
        },
        Expr::Call(function, inner) => {
            let argument = infer(inner, quantities);
            if REQUIRES_DIMENSIONLESS_ARGUMENT.contains(function) {
                Some(Quantity::Dimensionless)
            } else {
                argument
            }
        }
    }
}

/// Refuse a `formula` expression if any argument to `exp`, `ln`, `log10`,
/// `sin`, `cos`, or `tan` cannot be shown to be dimensionless by [`infer`].
/// This is the one dimensional check this module makes of a `formula`
/// relation's expression — see the module-level comment on
/// `super::relation::dimension_check`'s `"formula"` arm for what is
/// deliberately left unchecked and why.
pub(super) fn check_formula_arguments(
    expr: &Expr,
    quantities: &BTreeMap<&str, Quantity>,
) -> Result<(), AtlasError> {
    match expr {
        Expr::Call(function, inner) => {
            if REQUIRES_DIMENSIONLESS_ARGUMENT.contains(function) {
                match infer(inner, quantities) {
                    Some(Quantity::Dimensionless) => {}
                    Some(other) => {
                        return Err(format!(
                            "formula expression calls {function}(...) with an argument that is {}, not dimensionless; {function} only accepts a dimensionless argument",
                            other.name()
                        ))
                    }
                    None => {
                        return Err(format!(
                            "formula expression calls {function}(...) with an argument whose dimension could not be shown to be dimensionless; {function} only accepts a dimensionless argument"
                        ))
                    }
                }
            }
            check_formula_arguments(inner, quantities)
        }
        Expr::Neg(inner) => check_formula_arguments(inner, quantities),
        Expr::Add(left, right)
        | Expr::Sub(left, right)
        | Expr::Mul(left, right)
        | Expr::Div(left, right)
        | Expr::Pow(left, right) => {
            check_formula_arguments(left, quantities)?;
            check_formula_arguments(right, quantities)
        }
        Expr::Num(_) | Expr::Var(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_length_unit_shares_one_quantity_and_converts_through_meters() {
        for (unit, scale) in [
            ("nm", 1.0e-9),
            ("um", 1.0e-6),
            ("µm", 1.0e-6),
            ("mm", 1.0e-3),
            ("cm", 1.0e-2),
            ("m", 1.0),
            ("in", 0.0254),
        ] {
            let dimension = parse(unit).unwrap_or_else(|error| panic!("{unit}: {error}"));
            assert_eq!(dimension.quantity, Quantity::Length, "{unit}");
            assert_eq!(dimension.scale, scale, "{unit}");
        }
    }

    #[test]
    fn px_is_its_own_quantity_not_a_length() {
        let px = parse("px").expect("px is recognized");
        assert_eq!(px.quantity, Quantity::Pixel);
        assert_ne!(px.quantity, Quantity::Length);
    }

    #[test]
    fn an_unrecognized_unit_is_refused_naming_it() {
        let error = parse("smoots").unwrap_err();
        assert!(error.contains("smoots"), "{error}");
    }

    #[test]
    fn a_bare_and_a_ratio_variable_are_the_same_dimension() {
        let (bare_scale, ratio_scale) =
            require_same_quantity("test", "a", None, "b", Some("ratio")).expect("bare == ratio");
        assert_eq!(bare_scale, 1.0);
        assert_eq!(ratio_scale, 1.0);
    }

    #[test]
    fn a_length_and_a_pixel_variable_are_refused_naming_both_units() {
        let error = require_same_quantity("test", "a", Some("nm"), "b", Some("px")).unwrap_err();
        assert!(error.contains("nm"), "{error}");
        assert!(error.contains("px"), "{error}");
        assert!(error.contains('a'), "{error}");
        assert!(error.contains('b'), "{error}");
    }
}
