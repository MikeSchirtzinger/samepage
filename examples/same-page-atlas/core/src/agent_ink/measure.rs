//! Measured variables: a number derived from the drawing's own geometry
//! instead of one a caller declares.
//!
//! Every other agent-ink variable states a claim someone made — a caller
//! typed a value, or the solver computed one to satisfy a relation. A
//! measured variable states an observation: "these two boxes are 40px
//! apart" is true or false independent of anyone's intent, the moment the
//! boxes exist at the positions they are at. That is why `measured` is its
//! own entry in [`super::VAR_STATES`] rather than a spelling of `pinned` (a
//! human or agent may write a pinned value directly; nobody may write a
//! measured one, because there is nothing to write — see
//! [`super::solve::set_variable_value`]'s refusal) or of `derived` (a
//! derived variable follows a `linear` relation another write in this same
//! document declared; a measured variable follows the shapes and nodes
//! already on the canvas, which were never declared to be inputs to
//! anything).
//!
//! # Why a measured variable is solver-input, not solver-output
//!
//! [`super::solve::System::from_atlas`] only ever adds a variable to its
//! `free` set — the set the optimizer is allowed to move — when its state is
//! [`super::VarState::Free`]. A measured variable's state is deliberately
//! never `Free`, for the same reason a `distance` relation's own `target`
//! member is not automatically free: a measurement the solver was also free
//! to overwrite would stop being a measurement, and every relation naming
//! it would be solving toward a number that could quietly move out from
//! under the answer on the next read. A measured variable is exactly as
//! movable as a `pinned` one from the solver's point of view — which is to
//! say, not at all — except that nobody, human or agent, gets to move it
//! directly either; only the drawing does.
//!
//! # Re-measurement: recomputed on every read, never written back
//!
//! [`finalize_measurements`] runs once per [`crate::read`], right after
//! `geometry::finalize_bindings`, and overwrites every measured variable's
//! in-memory `value` with a fresh number computed from whatever the atlas's
//! shapes and nodes hold *right now*. It never writes that number back into
//! the CRDT. That combination is what makes a human drag on a measured
//! object work without any special capture path (contrast
//! `geometry::capture_represented_property`, which a *declared* binding
//! needs because nothing else would notice the drag): the drag already
//! landed in the dragged shape's own `x`/`y`/`w`/`h` through the ordinary
//! property-write path, so the very next `read()` — the one every tool in
//! this crate performs before it returns — recomputes the measurement from
//! that new position for free. There is nothing to refuse and nothing to
//! capture, because a measured variable never claimed to own the object it
//! measures; it only reports on it. Not persisting the recomputed number
//! also means a measurement is never "on every unrelated write" — it costs
//! one read of the objects it names, however many `read()` calls happen to
//! run, and touches the CRDT not at all.
//!
//! # Dangling measurements
//!
//! An object a measure names can be deleted the same way an object a
//! `represents` binding names can (see `geometry.rs`'s module doc comment).
//! [`finalize_measurements`] does not overwrite `value` when that happens —
//! see its doc comment for why leaving the last live number in place, next
//! to a `measured_live: false` flag, is the honest thing to show rather
//! than either panicking or reporting a fabricated zero.

use std::collections::BTreeSet;

use ag_ui_canvas::scene::{Author, Scene};
use serde::{Deserialize, Serialize};

use super::geometry::object_exists;
use super::variable::{create_measured_variable_object, required_text, validated_name};
use crate::{capacity, read, Atlas, AtlasError, MAX_PATH};

/// The closed vocabulary of geometric measures a variable may report.
pub const MEASURE_KINDS: &[&str] = &["gap", "center_distance", "width", "height", "angle"];

/// What kind of geometric fact a measured variable reports, and how many
/// objects it needs to report it — see [`validate_arity`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeasureKind {
    /// The clear distance between the two objects' nearest edges — zero
    /// when they touch or overlap, not the (generally larger) distance
    /// between their centres. "How far apart are these two boxes" is a
    /// question about the empty space between them, not about their
    /// origins, so this is the measure `gap` names — see [`gap`] for the
    /// exact formula and why it degrades correctly to a pure perpendicular
    /// separation when the boxes already overlap on one axis.
    Gap,
    /// The Euclidean distance between the two objects' centres, exactly the
    /// quantity a `distance` relation's residual already compares against a
    /// target — this measure exists for the reverse direction, reading that
    /// same quantity back out of the drawing instead of driving toward it.
    CenterDistance,
    /// The width (or, for more than one object, the width of the bounding
    /// box that encloses all of them) — see [`bounding_width`].
    Width,
    /// The height (or bounding-box height) — see [`bounding_height`].
    Height,
    /// The angle, in radians, either along a single line/arrow shape's own
    /// two endpoints, or of the line joining two objects' centres. See
    /// [`compute`]'s `Angle` arm for which of the two a given measure's
    /// object count selects.
    Angle,
}

impl MeasureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::CenterDistance => "center_distance",
            Self::Width => "width",
            Self::Height => "height",
            Self::Angle => "angle",
        }
    }

    fn parse(value: &str) -> Result<Self, AtlasError> {
        match value {
            "gap" => Ok(Self::Gap),
            "center_distance" => Ok(Self::CenterDistance),
            "width" => Ok(Self::Width),
            "height" => Ok(Self::Height),
            "angle" => Ok(Self::Angle),
            _ => Err(format!(
                "measure kind must be one of {}; got {value:?}",
                MEASURE_KINDS.join(", ")
            )),
        }
    }

    /// The unit a measure of this kind is expressed in, always — not
    /// something a caller chooses. A gap, a centre distance, and a width or
    /// height all come straight out of the atlas's own `x`/`y`/`w`/`h`
    /// coordinates, which are screen pixels the same way a `represents`
    /// binding's shape coordinates are (see `dimension.rs`'s module doc
    /// comment on why `px` is never a length); an angle comes out of
    /// `atan2`, which is radians by construction. Fixing the unit here
    /// rather than accepting one from the caller is what makes the O3
    /// dimension check catch "relate a measured pixel gap to a physical
    /// length" for free, with no measure-specific check of its own: a
    /// `px`-vs-`mm` mismatch is already refused by
    /// `dimension::require_same_quantity` the moment such a relation is
    /// created.
    fn unit(self) -> &'static str {
        match self {
            Self::Gap | Self::CenterDistance | Self::Width | Self::Height => "px",
            Self::Angle => "rad",
        }
    }
}

/// A measured variable's descriptor: what geometric fact it reports, and
/// which existing shapes or nodes it reports it about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Measure {
    pub kind: MeasureKind,
    pub objects: Vec<String>,
}

/// How many objects each measure kind needs, and in what roles.
///
/// `gap` and `center_distance` each compare exactly two objects — a third
/// would leave the measure not knowing which pair's distance to report.
/// `width` and `height` take the bounding box of however many objects are
/// named, one or more, because "the width of this one box" and "the width
/// spanned by these three boxes together" are the same computation
/// ([`bounding_width`]/[`bounding_height`] fold over however many rects
/// they are given). `angle` takes either one object — a line or arrow
/// shape, measured along its own two endpoints — or two, measured as the
/// bearing between their centres; see [`compute`]'s `Angle` arm.
fn validate_arity(kind: MeasureKind, objects: &[String]) -> Result<(), AtlasError> {
    let ok = match kind {
        MeasureKind::Gap | MeasureKind::CenterDistance => objects.len() == 2,
        MeasureKind::Width | MeasureKind::Height => !objects.is_empty(),
        MeasureKind::Angle => objects.len() == 1 || objects.len() == 2,
    };
    if ok {
        return Ok(());
    }
    let expected = match kind {
        MeasureKind::Gap | MeasureKind::CenterDistance => "exactly 2 objects".to_string(),
        MeasureKind::Width | MeasureKind::Height => "at least 1 object".to_string(),
        MeasureKind::Angle => "1 or 2 objects".to_string(),
    };
    Err(format!(
        "measure kind {:?} needs {expected}; got {}",
        kind.as_str(),
        objects.len()
    ))
}

/// Parse a stored `measure_kind`/`measure_objects` pair back into a
/// [`Measure`], or `None` when neither field was ever written. Called only
/// from [`super::variable::project`], which is also what enforces that a
/// `measured` variable always has one of these and every other variable
/// never does — see that function's consistency check just after this call.
pub(super) fn project_measure(kind: &str, objects: &str) -> Result<Option<Measure>, AtlasError> {
    match (kind.is_empty(), objects.is_empty()) {
        (true, true) => Ok(None),
        (false, false) => {
            let kind = MeasureKind::parse(kind)?;
            let objects = objects
                .split(',')
                .map(|object| required_text(object, MAX_PATH, "measured object"))
                .collect::<Result<Vec<_>, _>>()?;
            validate_arity(kind, &objects)?;
            Ok(Some(Measure { kind, objects }))
        }
        _ => Err(
            "stored variable measure fields must contain both a kind and at least one object, or neither"
                .to_string(),
        ),
    }
}

/// A shape or node's position and extent — the only geometry a measure ever
/// reads, the same closed set `geometry.rs`'s `represents` binding reads
/// and writes (`x`, `y`, `w`, `h`), just taken from both kinds of object at
/// once instead of one named property of one.
fn rect(atlas: &Atlas, id: &str) -> Option<(f64, f64, f64, f64)> {
    if let Some(node) = atlas.node(id) {
        return Some((node.x, node.y, node.w, node.h));
    }
    atlas
        .shape(id)
        .map(|shape| (shape.x, shape.y, shape.w, shape.h))
}

fn center(rect: (f64, f64, f64, f64)) -> (f64, f64) {
    (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0)
}

/// The clear distance between two axis-aligned rectangles' nearest edges.
///
/// `dx`/`dy` are each the separation along that one axis, clamped to zero
/// when the rectangles already overlap on it — the standard formula for the
/// distance between two axis-aligned boxes. When the boxes overlap on
/// *both* axes (they intersect or touch), both clamp to zero and the gap is
/// correctly zero. When they overlap on exactly one axis (one is directly
/// above or beside the other), the other axis's separation alone is the
/// gap — not a diagonal corner-to-corner distance, which would overstate
/// how far apart two boxes are when one is directly below the other. When
/// they overlap on neither axis (diagonal placement), the gap is the
/// straight-line distance between the two nearest corners, `hypot(dx, dy)`.
/// This is deliberately not centre-to-centre distance — see
/// [`MeasureKind::Gap`]'s doc comment for why "how far apart are these"
/// means the empty space between them, not their origins.
fn gap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let dx = (ax - (bx + bw)).max(bx - (ax + aw)).max(0.0);
    let dy = (ay - (by + bh)).max(by - (ay + ah)).max(0.0);
    dx.hypot(dy)
}

fn center_distance(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let (ax, ay) = center(a);
    let (bx, by) = center(b);
    (bx - ax).hypot(by - ay)
}

/// The bearing, in radians, from `a`'s centre to `b`'s centre — the two
/// object case of [`MeasureKind::Angle`].
fn bearing(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let (ax, ay) = center(a);
    let (bx, by) = center(b);
    (by - ay).atan2(bx - ax)
}

/// The width of the bounding box enclosing every rect: the span from the
/// leftmost left edge to the rightmost right edge. For a single rect this
/// is just its own `w`.
fn bounding_width(rects: &[(f64, f64, f64, f64)]) -> f64 {
    let min_x = rects
        .iter()
        .map(|rect| rect.0)
        .fold(f64::INFINITY, f64::min);
    let max_x = rects
        .iter()
        .map(|rect| rect.0 + rect.2)
        .fold(f64::NEG_INFINITY, f64::max);
    max_x - min_x
}

/// The height of the bounding box enclosing every rect — [`bounding_width`],
/// along the other axis.
fn bounding_height(rects: &[(f64, f64, f64, f64)]) -> f64 {
    let min_y = rects
        .iter()
        .map(|rect| rect.1)
        .fold(f64::INFINITY, f64::min);
    let max_y = rects
        .iter()
        .map(|rect| rect.1 + rect.3)
        .fold(f64::NEG_INFINITY, f64::max);
    max_y - min_y
}

/// Compute what `measure` currently says about `atlas`, or the reason it
/// cannot: an object it names no longer exists, or (the one-object `angle`
/// case only) the named shape does not have the two points an angle needs.
/// Called both from [`finalize_measurements`], which only keeps the `Ok`
/// half, and from `describe.rs`'s read-back, which surfaces the `Err` half
/// as the DANGLING reason — the same two-caller split `relation.rs`'s
/// `dimension_check` uses, for the same reason: one caller enforces, the
/// other explains.
pub(super) fn compute(atlas: &Atlas, measure: &Measure) -> Result<f64, String> {
    let rects = measure
        .objects
        .iter()
        .map(|id| {
            rect(atlas, id)
                .ok_or_else(|| format!("no shape or node named {id:?} exists on the atlas"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    match measure.kind {
        MeasureKind::Gap => Ok(gap(rects[0], rects[1])),
        MeasureKind::CenterDistance => Ok(center_distance(rects[0], rects[1])),
        MeasureKind::Width => Ok(bounding_width(&rects)),
        MeasureKind::Height => Ok(bounding_height(&rects)),
        MeasureKind::Angle if rects.len() == 2 => Ok(bearing(rects[0], rects[1])),
        MeasureKind::Angle => {
            // The one-object case: a line or arrow's own two endpoints.
            // `object_exists` above already proved this id names *some*
            // shape or node; here it must specifically be a shape with at
            // least two points, or there is no line to take an angle along.
            let id = &measure.objects[0];
            let shape = atlas.shape(id).ok_or_else(|| {
                format!(
                    "{id:?} is a node, not a shape; a one-object angle measure needs a line or \
                     arrow shape with at least two points"
                )
            })?;
            if shape.points.len() < 2 {
                return Err(format!(
                    "shape {id:?} has fewer than two points; a one-object angle measure needs a \
                     line or arrow with at least two points"
                ));
            }
            let (x0, y0) = shape.points[0];
            let (x1, y1) = *shape.points.last().expect("checked len() >= 2 above");
            Ok((y1 - y0).atan2(x1 - x0))
        }
    }
}

/// Refresh every measured variable's `value` from the atlas that was just
/// finished projecting, the same way `geometry::finalize_bindings` refreshes
/// `represents_live`, and for the same reason: a measure that resolved
/// cleanly when it was created can point at an object that has since been
/// deleted, moved, or (for a one-object angle) had its points edited down
/// below two, and only the whole assembled atlas — not whatever partial
/// state existed while this variable's own object was being projected —
/// can say which. Called from [`crate::read`] right after
/// `finalize_bindings`.
///
/// A variable whose measure fails to resolve keeps whatever `value` it last
/// held — the value it was created with, since this never writes back to
/// the CRDT (see the module doc comment) — with `measured_live` set to
/// `false`. That is a deliberate choice to leave a stale-but-honest number
/// in place next to a flag saying not to trust it, matching
/// `represents_live`'s convention, rather than replacing it with a zero or
/// a `NaN` that would look like a real measurement of an empty drawing.
pub(crate) fn finalize_measurements(atlas: &mut Atlas) {
    let snapshot: &Atlas = atlas;
    let updates = snapshot
        .variables
        .iter()
        .enumerate()
        .filter_map(|(index, variable)| {
            variable
                .measure
                .as_ref()
                .map(|measure| (index, compute(snapshot, measure)))
        })
        .collect::<Vec<_>>();
    for (index, outcome) in updates {
        match outcome {
            Ok(value) => {
                atlas.variables[index].value = value;
                atlas.variables[index].measured_live = true;
            }
            Err(_) => {
                atlas.variables[index].measured_live = false;
            }
        }
    }
}

/// Create a measured variable: `kind` and `objects` name the geometric fact
/// to report and the existing shapes or nodes to report it about, exactly
/// the closed vocabulary [`validate_arity`] checks. There is no `value` or
/// `unit` parameter — see [`MeasureKind::unit`] for why the unit is never a
/// caller's choice — and no `represents` binding, because a measured
/// variable already names the objects it is about, in `objects`, not in a
/// separate single-property binding the way a declared variable does.
pub fn create_measured_variable(
    scene: &mut Scene,
    name: &str,
    kind: &str,
    objects: &[&str],
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    capacity(&atlas)?;

    let name = validated_name(name)?;
    if atlas.variables.iter().any(|variable| variable.name == name) {
        return Err(format!(
            "variable name {name:?} already exists; variable names must be unique"
        ));
    }
    let kind = MeasureKind::parse(kind)?;
    let objects = objects
        .iter()
        .map(|object| required_text(object, MAX_PATH, "measured object"))
        .collect::<Result<Vec<_>, AtlasError>>()?;
    if objects.iter().any(|object| object.contains(',')) {
        return Err("a measured object id cannot contain a comma".to_string());
    }
    validate_arity(kind, &objects)?;
    if objects.iter().collect::<BTreeSet<_>>().len() != objects.len() {
        return Err("a measured variable cannot name the same object twice".to_string());
    }
    for object in &objects {
        if !object_exists(&atlas, object) {
            return Err(format!(
                "variable cannot measure {object:?}: no shape or node with that id exists on the atlas"
            ));
        }
    }

    let measure = Measure {
        kind,
        objects: objects.clone(),
    };
    // Computed once, eagerly, so creating a measured variable never leaves
    // the document holding a placeholder number before the first read
    // comes along to fix it up — see `compute`'s doc comment for why this
    // can still fail (a one-object angle needs a shape with two points,
    // which `validate_arity` cannot check on its own).
    let value = compute(&atlas, &measure)
        .map_err(|reason| format!("cannot create measured variable {name:?}: {reason}"))?;

    create_measured_variable_object(
        scene,
        name,
        value,
        kind.unit(),
        kind.as_str(),
        objects.join(","),
        author,
    )
}

#[cfg(test)]
mod tests {
    use ag_ui_canvas::ids::ObjectId;
    use ag_ui_canvas::scene::{Author, PropValue, Scene};

    use super::*;
    use crate::agent_ink::{create_relation, create_variable, solve, VarState};
    use crate::{read, ShapePatch};

    fn make_rect(scene: &mut Scene, x: f64, y: f64, w: f64, h: f64) -> String {
        crate::place_shape(
            scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(x),
                y: Some(y),
                w: Some(w),
                h: Some(h),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("create shape")
    }

    /// Requirement 1: a gap between two rects with known geometry, checked
    /// against an independently computed number. `a` spans x in [0, 10]; `b`
    /// spans x in [40, 60], directly to its right with matching y, so the
    /// two overlap in y (dy clamps to 0) and the gap is purely horizontal:
    /// 40 - 10 = 30.
    #[test]
    fn measured_gap_matches_an_independently_computed_distance() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 20.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");

        let atlas = read(&scene).expect("read");
        let variable = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "gap ab")
            .expect("find measured variable");
        assert!(
            (variable.value - 30.0).abs() < 1.0e-9,
            "expected 30, got {}",
            variable.value
        );
        assert!(variable.measured_live);
        assert_eq!(variable.unit.as_deref(), Some("px"));
        assert_eq!(variable.state, VarState::Measured);
    }

    /// A diagonal placement exercises the two-axis branch of the gap
    /// formula: `a` is a 10x10 box at the origin (edges at x=[0,10],
    /// y=[0,10]); `b` is a 10x10 box at (20, 20) (edges at x=[20,30],
    /// y=[20,30]), overlapping `a` on neither axis, so the gap is the
    /// corner-to-corner distance between `a`'s bottom-right corner (10, 10)
    /// and `b`'s top-left corner (20, 20): `hypot(10, 10)`.
    #[test]
    fn measured_gap_on_a_diagonal_placement_is_the_corner_to_corner_distance() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 20.0, 20.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap diag", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");

        // a's right/bottom edge is at (10, 10); b's near corner is at
        // (20, 20); the nearest-corner distance is hypot(10, 10).
        let expected = (10.0_f64).hypot(10.0);
        let atlas = read(&scene).expect("read");
        let value = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "gap diag")
            .expect("find variable")
            .value;
        assert!(
            (value - expected).abs() < 1.0e-9,
            "expected {expected}, got {value}"
        );
    }

    #[test]
    fn measured_center_distance_matches_an_independently_computed_distance() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 30.0, 0.0, 10.0, 10.0);
        create_measured_variable(
            &mut scene,
            "center distance ab",
            "center_distance",
            &[&a, &b],
            &Author::Agent,
        )
        .expect("create measured center distance");

        // Centres are (5, 5) and (35, 5): distance is exactly 30.
        let atlas = read(&scene).expect("read");
        let value = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "center distance ab")
            .expect("find variable")
            .value;
        assert!((value - 30.0).abs() < 1.0e-9, "expected 30, got {value}");
    }

    #[test]
    fn measured_width_of_a_bounding_box_spans_every_named_object() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "span", "width", &[&a, &b], &Author::Agent)
            .expect("create measured width");

        // Leftmost left edge is 0, rightmost right edge is 50: span is 50.
        let atlas = read(&scene).expect("read");
        let value = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "span")
            .expect("find variable")
            .value;
        assert!((value - 50.0).abs() < 1.0e-9, "expected 50, got {value}");
    }

    /// Requirement 2: a test relating a measured value to a declared one
    /// through the solver and getting the right answer. The measured gap
    /// between two rects is related `equal` to a free target variable; the
    /// solve must move the free variable to the measured value, not the
    /// other way around, because a measured variable is never in the
    /// solver's free set.
    #[test]
    fn a_measured_value_drives_a_free_variable_through_a_solve() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");
        create_variable(
            &mut scene,
            "target gap",
            0.0,
            "free",
            None,
            Some("px"),
            &Author::Agent,
        )
        .expect("create free target");
        create_relation(
            &mut scene,
            "target tracks measured gap",
            "equal",
            &["target gap", "gap ab"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("relate measured to free");

        solve(&mut scene, &Author::Agent).expect("solve");

        let atlas = read(&scene).expect("read after solve");
        let target = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "target gap")
            .expect("find target");
        assert!(
            (target.value - 30.0).abs() < 1.0e-6,
            "the free variable must converge to the measured gap, got {}",
            target.value
        );
        let measured = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "gap ab")
            .expect("find measured");
        assert!(
            (measured.value - 30.0).abs() < 1.0e-9,
            "the measured variable itself must still report the drawing's real gap, got {}",
            measured.value
        );
    }

    /// Requirement 3: moving one rect changes the measured value on the
    /// next read, with no solve or capture call in between.
    #[test]
    fn moving_a_measured_object_changes_the_value_on_the_next_read() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");
        assert!((read(&scene).expect("read").variables[0].value - 30.0).abs() < 1.0e-9);

        scene
            .set_prop(&ObjectId::from(b.as_str()), "x", PropValue::Num(100.0))
            .expect("move b further away");

        let after = read(&scene).expect("read after the move");
        let value = after
            .variables
            .iter()
            .find(|variable| variable.name == "gap ab")
            .expect("find variable")
            .value;
        assert!(
            (value - 90.0).abs() < 1.0e-9,
            "the gap must be recomputed from b's new position, got {value}"
        );
    }

    /// A human drag needs no capture call at all: the same recompute
    /// mechanism that requirement 3 exercises through a direct patch also
    /// covers a drag, because both paths write the same `x`/`y`/`w`/`h`
    /// properties this measure reads.
    #[test]
    fn a_measured_gap_reflects_a_drag_with_no_capture_call() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");

        scene
            .set_prop(&ObjectId::from(b.as_str()), "x", PropValue::Num(20.0))
            .expect("drag b closer");

        let value = read(&scene)
            .expect("read")
            .variables
            .iter()
            .find(|variable| variable.name == "gap ab")
            .expect("find variable")
            .value;
        assert!((value - 10.0).abs() < 1.0e-9, "expected 10, got {value}");
    }

    /// Requirement 4: a measured px value related to a physical length is
    /// refused by the O3 dimension check, exactly as a declared px variable
    /// already would be — see `dimension.rs`'s module doc comment on why
    /// `px` is its own quantity.
    #[test]
    fn relating_a_measured_pixel_gap_to_a_physical_length_is_refused() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");
        create_variable(
            &mut scene,
            "physical gap",
            30.0,
            "pinned",
            None,
            Some("mm"),
            &Author::Agent,
        )
        .expect("create physical-length variable");

        let error = create_relation(
            &mut scene,
            "gap equals physical",
            "equal",
            &["gap ab", "physical gap"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect_err("relating px to mm must be refused");
        assert!(error.contains("pixel"), "{error}");
        assert!(error.contains("length"), "{error}");
    }

    /// Requirement 5: a deleted object makes the measure read back as
    /// dangling, keeping its last live number rather than reporting a
    /// fabricated one, matching `represents_live`'s convention.
    #[test]
    fn a_measured_variable_whose_object_is_deleted_reads_back_as_dangling() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");

        let live = read(&scene).expect("read before delete");
        assert!(live.variables[0].measured_live);
        let last_value = live.variables[0].value;

        crate::remove(&scene, &b).expect("delete b");

        let dangling = read(&scene).expect("read after delete");
        let variable = dangling
            .variables
            .iter()
            .find(|variable| variable.name == "gap ab")
            .expect("find variable");
        assert!(
            !variable.measured_live,
            "the measure must go dangling once its object is gone"
        );
        assert_eq!(
            variable.value, last_value,
            "a dangling measure must keep its last live number, not fabricate a new one"
        );

        let text = dangling.describe();
        assert!(text.contains("DANGLING"), "{text}");
    }

    /// State-vocabulary interaction: a plain `atlas_variable_create` cannot
    /// mint a `measured` variable — it has no measure to attach — so this
    /// is refused by name at the boundary, not left to fail confusingly
    /// somewhere inside the solver later.
    #[test]
    fn a_plain_create_variable_call_refuses_the_measured_state() {
        let mut scene = Scene::new();
        let error = create_variable(
            &mut scene,
            "ghost",
            0.0,
            "measured",
            None,
            None,
            &Author::Agent,
        )
        .expect_err("measured must be refused outside create_measured_variable");
        assert!(error.contains("atlas_variable_measure_create"), "{error}");
    }

    /// A measured variable cannot be written directly either by a human
    /// drag's capture path or by `atlas_variable_set` — see
    /// `solve::set_variable_value`'s refusal, exercised here end to end.
    #[test]
    fn a_measured_variable_cannot_be_set_directly() {
        let mut scene = Scene::new();
        let a = make_rect(&mut scene, 0.0, 0.0, 10.0, 10.0);
        let b = make_rect(&mut scene, 40.0, 0.0, 10.0, 10.0);
        create_measured_variable(&mut scene, "gap ab", "gap", &[&a, &b], &Author::Agent)
            .expect("create measured gap");

        let error = super::super::set_variable_value(&scene, "gap ab", 999.0, &Author::Agent)
            .expect_err("a measured variable cannot be written directly");
        assert!(error.contains("gap ab"), "{error}");
    }
}
