//! The wire between an agent-ink variable's number and a shape or node's own
//! property in the drawing layer.
//!
//! A `represents` binding is validated once, at [`super::create_variable`]
//! time, against the atlas projected at that moment: the object must exist
//! and the property must be one of the closed vocabulary below. Neither
//! check runs again on every solve, because a binding that resolved cleanly
//! when it was authored can still go dangling later — a CRDT merge can
//! remove the object, or land a variable a peer created before the object it
//! names ever reached this replica. [`finalize_bindings`] is what tells the
//! read-back which is which, and every write in this file treats a dangling
//! binding as nothing to do rather than a reason to fail the whole solve.

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};

use super::solve::set_variable_value;
use crate::{read, Atlas, AtlasError};

/// The closed vocabulary of shape/node fields a variable may drive or read
/// back. Both kinds use the same key names for position and extent, so one
/// list covers both. `label`, `ink`, and the rest of the non-numeric fields
/// are deliberately absent: a numeric variable has nothing honest to say
/// about them, and letting one bind to `z` or `points` would hand the solver
/// paint order or a stroke's vertex list, neither of which is a scalar it
/// was ever built to own.
pub const REPRESENTED_PROPERTIES: &[&str] = &["x", "y", "w", "h"];

pub(crate) fn is_represented_property(property: &str) -> bool {
    REPRESENTED_PROPERTIES.contains(&property)
}

/// Whether `object` currently names a shape or a node in `atlas` — the only
/// two kinds of object an agent-ink variable may represent a property of.
pub(crate) fn object_exists(atlas: &Atlas, object: &str) -> bool {
    atlas.node(object).is_some() || atlas.shape(object).is_some()
}

/// Set every variable's `represents_live` flag from the atlas it was just
/// projected into.
///
/// Called once, after [`crate::read`] has finished building the whole atlas,
/// so a variable is never judged against a partially-assembled projection
/// that has not reached its own bound object yet. A variable with no
/// binding is always `false`; the field only distinguishes a binding that is
/// driving geometry from one that is dangling.
pub(crate) fn finalize_bindings(atlas: &mut Atlas) {
    let object_ids = atlas
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .chain(atlas.shapes.iter().map(|shape| shape.id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    for variable in &mut atlas.variables {
        variable.represents_live = match &variable.represents {
            Some((object, property)) => {
                is_represented_property(property) && object_ids.contains(object.as_str())
            }
            None => false,
        };
    }
}

/// Write every live-bound variable's current value into the shape or node
/// property it represents.
///
/// Called after any operation that changes a variable's value — a solve, a
/// scrub, an advance, or a snapshot restore — so scrubbing the timeline
/// moves the actual shape, not just the panel's numbers. A dangling binding
/// is skipped rather than treated as a fault: [`super::describe`] is where a
/// dangling binding gets to say so, not a place that can fail an otherwise
/// successful solve.
pub(super) fn sync_represented_geometry(
    scene: &Scene,
    atlas: &Atlas,
    author: &Author,
) -> Result<(), AtlasError> {
    for variable in &atlas.variables {
        let Some((object, property)) = &variable.represents else {
            continue;
        };
        if !is_represented_property(property) || !object_exists(atlas, object) {
            continue;
        }
        if represented_value(atlas, object, property)
            .is_some_and(|current| current.to_bits() == variable.value.to_bits())
        {
            continue;
        }
        scene
            .set_props(
                &ObjectId::from(object.as_str()),
                &[
                    (property.as_str(), PropValue::Num(variable.value)),
                    ("touched_by", PropValue::Str(author.as_str().to_string())),
                ],
            )
            .map_err(|error| {
                format!(
                    "could not write variable {:?} into {object}.{property}: {error}",
                    variable.name
                )
            })?;
    }
    Ok(())
}

fn represented_value(atlas: &Atlas, object: &str, property: &str) -> Option<f64> {
    let value = |x: f64, y: f64, w: f64, h: f64| match property {
        "x" => Some(x),
        "y" => Some(y),
        "w" => Some(w),
        "h" => Some(h),
        _ => None,
    };
    atlas
        .node(object)
        .and_then(|node| value(node.x, node.y, node.w, node.h))
        .or_else(|| {
            atlas
                .shape(object)
                .and_then(|shape| value(shape.x, shape.y, shape.w, shape.h))
        })
}

/// The human-drag path: read `object`.`property`'s new value back into the
/// agent-ink variable that represents it.
///
/// `object` and `property` name the binding exactly the way
/// `represents_object`/`represents_property` do at [`super::create_variable`]
/// time; `value` is whatever the drag left the shape or node at. Returns the
/// name of the variable that was written, or `None` when no variable
/// represents that object/property pair — a drag on an unbound shape has
/// nothing to capture into, which is not an error.
///
/// The write itself goes through [`set_variable_value`], so a binding onto a
/// free or derived variable is refused with exactly the error a direct
/// `atlas_variable_set` would produce: solver-owned state is never silently
/// overwritten by a gesture, only ever refused by name. Call [`solve`] (or
/// [`solve_with_options`]) afterward to propagate the captured value to
/// whatever depends on it; this function only records the observation.
///
/// [`solve`]: super::solve::solve
/// [`solve_with_options`]: super::solve::solve_with_options
pub fn capture_represented_property(
    scene: &Scene,
    object: &str,
    property: &str,
    value: f64,
    author: &Author,
) -> Result<Option<String>, AtlasError> {
    if !value.is_finite() {
        return Err("captured value must be a finite number".to_string());
    }
    let atlas = read(scene)?;
    let Some(variable) = atlas.variables.iter().find(|variable| {
        variable
            .represents
            .as_ref()
            .is_some_and(|(bound_object, bound_property)| {
                bound_object == object && bound_property == property
            })
    }) else {
        return Ok(None);
    };
    let name = variable.name.clone();
    set_variable_value(scene, &name, value, author)?;
    Ok(Some(name))
}

#[cfg(test)]
mod tests {
    use ag_ui_canvas::scene::{Author, Scene};

    use super::*;
    use crate::agent_ink::{
        advance_step, create_relation, create_variable, record_current_step, scrub_step, solve,
    };
    use crate::{read, ShapePatch};

    fn make_shape(scene: &mut Scene, x: f64) -> String {
        crate::place_shape(
            scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(x),
                y: Some(0.0),
                w: Some(10.0),
                h: Some(10.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("create shape")
    }

    #[test]
    fn a_solve_moves_the_bound_shapes_x() {
        let mut scene = Scene::new();
        let shape_id = make_shape(&mut scene, 0.0);
        create_variable(
            &mut scene,
            "box x",
            0.0,
            "free",
            Some((&shape_id, "x")),
            None,
            &Author::Agent,
        )
        .expect("create bound variable");
        create_variable(
            &mut scene,
            "target x",
            42.0,
            "pinned",
            None,
            None,
            &Author::Agent,
        )
        .expect("create target");
        create_relation(
            &mut scene,
            "box tracks target",
            "equal",
            &["box x", "target x"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");

        solve(&mut scene, &Author::Agent).expect("solve");

        let atlas = read(&scene).expect("read after solve");
        let shape = atlas.shape(&shape_id).expect("shape still exists");
        assert!(
            (shape.x - 42.0).abs() < 1.0e-6,
            "solve must move the shape's stored x, got {}",
            shape.x
        );
    }

    /// Direction A, continued: scrubbing back to an earlier step must move
    /// the shape back to bit-identically what it held at that step, not just
    /// restore the panel's number.
    #[test]
    fn scrubbing_back_moves_the_shape_back_bit_identically() {
        let mut scene = Scene::new();
        let shape_id = make_shape(&mut scene, 0.0);
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
            "box x",
            0.0,
            "free",
            Some((&shape_id, "x")),
            None,
            &Author::Agent,
        )
        .expect("create bound variable");
        create_relation(
            &mut scene,
            "box tracks step",
            "linear",
            &["box x", "step"],
            Some(10.0),
            Some(0.0),
            None,
            &Author::Agent,
        )
        .expect("create relation");
        solve(&mut scene, &Author::Agent).expect("solve step zero");
        record_current_step(&mut scene, "step", &Author::Agent).expect("record step zero");
        let step_zero_x = read(&scene)
            .expect("read step zero")
            .shape(&shape_id)
            .expect("shape")
            .x;

        for _ in 1..=3 {
            advance_step(&mut scene, "step", &Author::Agent).expect("advance step");
        }
        let step_three_x = read(&scene)
            .expect("read step three")
            .shape(&shape_id)
            .expect("shape")
            .x;
        assert_ne!(step_zero_x.to_bits(), step_three_x.to_bits());

        scrub_step(&mut scene, "step", 0, &Author::Agent).expect("scrub back to step zero");
        let scrubbed_x = read(&scene)
            .expect("read after scrub")
            .shape(&shape_id)
            .expect("shape")
            .x;
        assert_eq!(
            scrubbed_x.to_bits(),
            step_zero_x.to_bits(),
            "scrubbing back must restore the shape's x bit for bit, got {scrubbed_x} vs {step_zero_x}"
        );
    }

    /// Direction B: a human drag is captured into the pinned variable it is
    /// bound to, and a solve propagates it to a dependent variable's shape.
    #[test]
    fn a_captured_drag_propagates_through_a_solve_to_a_dependent_shape() {
        let mut scene = Scene::new();
        let dragged_id = make_shape(&mut scene, 0.0);
        let dependent_id = make_shape(&mut scene, 0.0);
        create_variable(
            &mut scene,
            "dragged x",
            0.0,
            "pinned",
            Some((&dragged_id, "x")),
            None,
            &Author::Agent,
        )
        .expect("create pinned variable bound to the dragged shape");
        create_variable(
            &mut scene,
            "dependent x",
            0.0,
            "free",
            Some((&dependent_id, "x")),
            None,
            &Author::Agent,
        )
        .expect("create free variable bound to the dependent shape");
        create_relation(
            &mut scene,
            "dependent tracks dragged",
            "equal",
            &["dependent x", "dragged x"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");

        let captured = capture_represented_property(&scene, &dragged_id, "x", 77.0, &Author::Human)
            .expect("capture the drag")
            .expect("the dragged shape is bound to a variable");
        assert_eq!(captured, "dragged x");

        solve(&mut scene, &Author::Agent).expect("solve after the capture");

        let atlas = read(&scene).expect("read after solve");
        assert!(
            (atlas.shape(&dragged_id).expect("dragged shape").x - 77.0).abs() < 1.0e-6,
            "the captured drag must hold the value it was set to"
        );
        assert!(
            (atlas.shape(&dependent_id).expect("dependent shape").x - 77.0).abs() < 1.0e-6,
            "the dependent shape must move to follow the dragged value"
        );
    }

    /// Direction B's refusal: a drag targeting a free (solver-owned)
    /// variable's bound shape is refused by name, not silently dropped.
    #[test]
    fn a_captured_drag_onto_a_free_variable_is_refused_by_name() {
        let mut scene = Scene::new();
        let shape_id = make_shape(&mut scene, 0.0);
        create_variable(
            &mut scene,
            "target x",
            10.0,
            "pinned",
            None,
            None,
            &Author::Agent,
        )
        .expect("create target");
        create_variable(
            &mut scene,
            "solver-owned x",
            0.0,
            "free",
            Some((&shape_id, "x")),
            None,
            &Author::Agent,
        )
        .expect("create free variable bound to the shape");
        create_relation(
            &mut scene,
            "solver-owned tracks target",
            "equal",
            &["solver-owned x", "target x"],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create relation");

        let error = capture_represented_property(&scene, &shape_id, "x", 999.0, &Author::Human)
            .expect_err("a drag onto a free variable's bound shape must be refused");
        assert!(error.contains("solver-owned x"), "{error}");
        assert!(error.contains("solver-owned"), "{error}");
    }

    /// Creation-time validation: a binding to an object that does not exist
    /// on the atlas is refused immediately, before it can ever reach a solve.
    #[test]
    fn a_binding_to_a_nonexistent_object_is_refused_at_creation() {
        let mut scene = Scene::new();
        let error = create_variable(
            &mut scene,
            "ghost x",
            0.0,
            "pinned",
            Some(("no-such-shape", "x")),
            None,
            &Author::Agent,
        )
        .expect_err("a binding to a missing object must be refused at creation");
        assert!(error.contains("no-such-shape"), "{error}");
    }

    /// Creation-time validation: a binding to a property outside the closed
    /// geometry vocabulary is refused immediately.
    #[test]
    fn a_binding_to_an_unknown_property_is_refused_at_creation() {
        let mut scene = Scene::new();
        let shape_id = make_shape(&mut scene, 0.0);
        let error = create_variable(
            &mut scene,
            "label x",
            0.0,
            "pinned",
            Some((&shape_id, "label")),
            None,
            &Author::Agent,
        )
        .expect_err("a binding to an unrecognised property must be refused at creation");
        assert!(error.contains("label"), "{error}");
    }
}
