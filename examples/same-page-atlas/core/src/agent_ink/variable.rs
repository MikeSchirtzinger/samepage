use ag_ui_canvas::scene::{Author, PropValue, Scene};
use serde::{Deserialize, Serialize};

use super::geometry::{is_represented_property, object_exists, REPRESENTED_PROPERTIES};
use super::measure::Measure;
use crate::{capacity, read, text, AtlasError, KIND_VARIABLE, MAX_LABEL, MAX_PATH};

const K_NAME: &str = "name";
const K_VALUE: &str = "value";
const K_STATE: &str = "state";
const K_REPRESENTS_OBJECT: &str = "represents_object";
const K_REPRESENTS_PROPERTY: &str = "represents_property";
const K_UNIT: &str = "unit";
const K_MEASURE_KIND: &str = "measure_kind";
const K_MEASURE_OBJECTS: &str = "measure_objects";
const K_TOUCHED_BY: &str = "touched_by";

/// Closed ownership vocabulary for numeric variables. `measured` sits
/// alongside `derived`: neither is ever free for the solver to move, but
/// where `derived` follows a linear relation another write declared,
/// `measured` follows the drawing itself — see `measure.rs`'s module doc
/// comment for why that makes it a fifth kind of ownership rather than a
/// spelling of `pinned` or `derived`.
pub const VAR_STATES: &[&str] = &["pinned", "scrubbing", "free", "derived", "measured"];

/// Who may write a variable's current value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VarState {
    Pinned,
    Scrubbing,
    Free,
    Derived,
    Measured,
}

impl VarState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Scrubbing => "scrubbing",
            Self::Free => "free",
            Self::Derived => "derived",
            Self::Measured => "measured",
        }
    }

    fn parse(value: &str) -> Result<Self, AtlasError> {
        match value {
            "pinned" => Ok(Self::Pinned),
            "scrubbing" => Ok(Self::Scrubbing),
            "free" => Ok(Self::Free),
            "derived" => Ok(Self::Derived),
            "measured" => Ok(Self::Measured),
            _ => Err(format!(
                "variable state must be one of {}; got {value:?}",
                VAR_STATES.join(", ")
            )),
        }
    }
}

/// A named numeric quantity projected from the shared CRDT document.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Variable {
    pub name: String,
    pub value: f64,
    pub state: VarState,
    /// Who created this variable object.
    pub created_by: String,
    /// Who last authored its value. Measurement refreshes deliberately leave
    /// this unchanged because a measurement is a derived observation, not an
    /// edit by the browser that happened to render it.
    pub touched_by: String,
    pub represents: Option<(String, String)>,
    pub unit: Option<String>,
    /// Whether `represents` currently names a real shape or node property.
    ///
    /// Always `false` when `represents` is `None`. Set once, by
    /// `geometry::finalize_bindings`, after [`crate::read`] has finished
    /// projecting the whole atlas — a binding that resolved cleanly when it
    /// was created can still go dangling later if the object it names is
    /// removed on another replica, so this is read fresh on every read
    /// rather than persisted.
    pub represents_live: bool,
    /// What this variable measures about the drawing, if it is a measured
    /// variable at all. `Some` if and only if `state` is
    /// [`VarState::Measured`] — see `measure.rs`.
    pub measure: Option<Measure>,
    /// Whether `measure` currently resolves against real geometry.
    ///
    /// Always `false` when `measure` is `None`. Set on every
    /// [`crate::read`] by `measure::finalize_measurements`, the same way
    /// [`Self::represents_live`] is set by `geometry::finalize_bindings`:
    /// `value` is only trustworthy when this is `true`, because a measured
    /// variable's `value` is never re-derived once the object it measures
    /// has gone missing (see that function's doc comment for why it is
    /// left at its last live number instead of zeroed).
    pub measured_live: bool,
}

pub(super) fn validated_name(value: &str) -> Result<String, AtlasError> {
    let name = text(value, MAX_LABEL, "variable name")?;
    if name.is_empty() {
        return Err("variable name cannot be empty".to_string());
    }
    Ok(name)
}

pub(super) fn required_text(value: &str, limit: usize, field: &str) -> Result<String, AtlasError> {
    let value = text(value, limit, field)?;
    if value.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    Ok(value)
}

/// Create a variable only after every field and the document-wide name rule
/// have been validated.
pub fn create_variable(
    scene: &mut Scene,
    name: &str,
    value: f64,
    state: &str,
    represents: Option<(&str, &str)>,
    unit: Option<&str>,
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
    if !value.is_finite() {
        return Err("variable value must be a finite number".to_string());
    }
    let state = VarState::parse(state)?;
    // A `measured` variable's value comes from the drawing, not from a
    // caller — it has nothing to write here, and no `represents`/`unit` of
    // its own to validate, because both are computed from `measure`
    // instead. `create_measured_variable` is the only path that can ever
    // write a `measure_kind`/`measure_objects` pair, so refusing `measured`
    // here keeps that invariant total: every variable this function writes
    // has an empty measure, and every variable `create_measured_variable`
    // writes has state `measured` — see `project`'s consistency check below
    // for the other half of that guarantee.
    if state == VarState::Measured {
        return Err("variable state \"measured\" cannot be set here; call \
             atlas_variable_measure_create instead, which derives the value \
             from the drawing itself"
            .to_string());
    }
    let represents = represents
        .map(|(object, property)| {
            Ok::<_, AtlasError>((
                required_text(object, MAX_PATH, "represented object")?,
                required_text(property, MAX_LABEL, "represented property")?,
            ))
        })
        .transpose()?;
    // A binding is checked against the atlas exactly once, here, while the
    // check is still cheap and the failure can still stop the write. Once
    // this variable exists, the object it names can still disappear under a
    // concurrent edit or a merge; that later state is a dangling binding for
    // `describe()` to report, not a reason to make every future read of this
    // document fail.
    if let Some((object, property)) = &represents {
        if !is_represented_property(property) {
            return Err(format!(
                "variable cannot represent property {property:?}; the represented property must be one of {}",
                REPRESENTED_PROPERTIES.join(", ")
            ));
        }
        if !object_exists(&atlas, object) {
            return Err(format!(
                "variable cannot represent {object:?}: no shape or node with that id exists on the atlas"
            ));
        }
    }
    let unit = unit
        .map(|unit| text(unit, MAX_LABEL, "variable unit"))
        .transpose()?
        .filter(|unit| !unit.is_empty());
    // A unit string is meaningless if the solver cannot tell what physical
    // quantity it names, so it is checked here, at the one point a document
    // can never route around: every unit ever attached to a variable
    // through this crate's own API passed through `dimension::parse` first.
    // A stored unit that fails to parse can therefore only ever be a
    // document written before this check existed (or edited outside this
    // crate) — see `dimension.rs`'s `VariableDimension::Unrecognized` doc
    // comment for how that case is handled on load instead of here.
    if let Some(unit) = &unit {
        super::dimension::parse(unit)?;
    }

    let (represented_object, represented_property) = represents.unwrap_or_default();
    let id = scene
        .create_object_with_props(
            KIND_VARIABLE,
            author.clone(),
            &[
                (K_NAME, PropValue::Str(name)),
                (K_VALUE, PropValue::Num(value)),
                (K_STATE, PropValue::Str(state.as_str().to_string())),
                (K_REPRESENTS_OBJECT, PropValue::Str(represented_object)),
                (K_REPRESENTS_PROPERTY, PropValue::Str(represented_property)),
                (K_UNIT, PropValue::Str(unit.unwrap_or_default())),
                (K_MEASURE_KIND, PropValue::Str(String::new())),
                (K_MEASURE_OBJECTS, PropValue::Str(String::new())),
                (K_TOUCHED_BY, PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not create variable: {error}"))?;
    Ok(id.into_string())
}

/// Create a `measured` variable: state `measured`, a `measure` descriptor
/// instead of a caller-supplied value, and no `represents`/`unit` of its
/// own. Kept in `measure.rs` next to the validation and geometry it depends
/// on; this function only owns the parts every variable creation path
/// shares (capacity, name uniqueness) plus the object write itself.
pub(super) fn create_measured_variable_object(
    scene: &mut Scene,
    name: String,
    value: f64,
    unit: &'static str,
    measure_kind: &'static str,
    measure_objects: String,
    author: &Author,
) -> Result<String, AtlasError> {
    let id = scene
        .create_object_with_props(
            KIND_VARIABLE,
            author.clone(),
            &[
                (K_NAME, PropValue::Str(name)),
                (K_VALUE, PropValue::Num(value)),
                (
                    K_STATE,
                    PropValue::Str(VarState::Measured.as_str().to_string()),
                ),
                (K_REPRESENTS_OBJECT, PropValue::Str(String::new())),
                (K_REPRESENTS_PROPERTY, PropValue::Str(String::new())),
                (K_UNIT, PropValue::Str(unit.to_string())),
                (K_MEASURE_KIND, PropValue::Str(measure_kind.to_string())),
                (K_MEASURE_OBJECTS, PropValue::Str(measure_objects)),
                (K_TOUCHED_BY, PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not create measured variable: {error}"))?;
    Ok(id.into_string())
}

pub(super) fn project(
    string: &impl Fn(&str) -> String,
    number: &impl Fn(&str) -> Option<f64>,
) -> Result<Variable, AtlasError> {
    let stored_name = string(K_NAME);
    let name = validated_name(&stored_name)?;
    if name != stored_name {
        return Err(format!(
            "stored variable name {stored_name:?} is not canonical; names cannot have surrounding whitespace"
        ));
    }

    let state = VarState::parse(&string(K_STATE))?;
    let created_by = string("created_by");
    let touched_by = match string(K_TOUCHED_BY) {
        value if value.is_empty() => created_by.clone(),
        value => value,
    };
    let value = number(K_VALUE)
        .filter(|value| value.is_finite())
        .ok_or_else(|| "variable value is missing or not finite".to_string())?;
    let represented_object = string(K_REPRESENTS_OBJECT);
    let represented_property = string(K_REPRESENTS_PROPERTY);
    let represents = match (
        represented_object.is_empty(),
        represented_property.is_empty(),
    ) {
        (true, true) => None,
        (false, false) => {
            let object = required_text(&represented_object, MAX_PATH, "represented object")?;
            let property = required_text(&represented_property, MAX_LABEL, "represented property")?;
            if object != represented_object || property != represented_property {
                return Err(
                    "stored variable represents fields cannot have surrounding whitespace"
                        .to_string(),
                );
            }
            Some((object, property))
        }
        _ => {
            return Err(
                "variable represents fields must contain both object and property, or neither"
                    .to_string(),
            )
        }
    };
    let unit = match string(K_UNIT) {
        unit if unit.is_empty() => None,
        unit => {
            let canonical = text(&unit, MAX_LABEL, "variable unit")?;
            if canonical != unit {
                return Err("stored variable unit cannot have surrounding whitespace".to_string());
            }
            Some(canonical)
        }
    };

    let measure =
        super::measure::project_measure(&string(K_MEASURE_KIND), &string(K_MEASURE_OBJECTS))?;
    // Every variable this crate has ever written keeps `state == measured`
    // and "has a measure descriptor" in lockstep — `create_variable` refuses
    // `measured` outright, and `create_measured_variable_object` never
    // writes any other state — so either half of this disagreeing can only
    // mean a document edited outside this crate's own API, and it is
    // refused here rather than read back as a variable that is measured in
    // name only, or one silently missing the descriptor it claims to have.
    match (state, measure.is_some()) {
        (VarState::Measured, false) => {
            return Err(format!(
                "stored variable {name:?} has state \"measured\" but no measure descriptor"
            ))
        }
        (VarState::Measured, true) | (_, false) => {}
        (_, true) => {
            let error = format!(
                "stored variable {name:?} has a measure descriptor but state {:?}, not \"measured\"",
                state.as_str()
            );
            return Err(error);
        }
    }

    Ok(Variable {
        name,
        value,
        state,
        created_by,
        touched_by,
        represents,
        unit,
        // Finalized once the whole atlas has been assembled — see
        // `geometry::finalize_bindings`, called from `crate::read`.
        represents_live: false,
        measure,
        // Finalized the same way, by `measure::finalize_measurements` —
        // see that function's doc comment.
        measured_live: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_state_vocabulary_is_closed_and_matches_the_projection() {
        assert_eq!(
            [
                VarState::Pinned.as_str(),
                VarState::Scrubbing.as_str(),
                VarState::Free.as_str(),
                VarState::Derived.as_str(),
                VarState::Measured.as_str(),
            ],
            VAR_STATES
        );
    }

    #[test]
    fn named_variable_survives_a_serialized_document_reload() {
        let mut scene = Scene::new();
        let swing_arm = crate::place_shape(
            &mut scene,
            &crate::ShapePatch {
                form: Some("rect".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(10.0),
                h: Some(10.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("create a shape for the represents binding");
        create_variable(
            &mut scene,
            "swing arm length",
            120.0,
            "pinned",
            Some((&swing_arm, "w")),
            Some("mm"),
            &Author::Human,
        )
        .expect("create named variable");

        let serialized = scene.encode_full().expect("serialize document");
        let reloaded = Scene::from_state(&serialized).expect("reload serialized document");
        let atlas = read(&reloaded).expect("project reloaded document");
        let variable = atlas
            .variables
            .iter()
            .find(|variable| variable.name == "swing arm length")
            .expect("read variable back by name");

        assert_eq!(variable.value, 120.0);
        assert_eq!(variable.state, VarState::Pinned);
        assert_eq!(
            variable.represents,
            Some((swing_arm.clone(), "w".to_string()))
        );
        assert!(
            variable.represents_live,
            "the binding must be live: the shape it names still exists"
        );
        assert_eq!(variable.unit.as_deref(), Some("mm"));
    }

    #[test]
    fn an_unrecognized_unit_is_refused_at_create_time_naming_the_string() {
        let mut scene = Scene::new();
        let error = create_variable(
            &mut scene,
            "distance",
            10.0,
            "free",
            None,
            Some("smoots"),
            &Author::Agent,
        )
        .expect_err("an unrecognized unit must be refused before the write happens");
        assert!(error.contains("smoots"), "{error}");
        assert!(
            read(&scene).expect("read").variables.is_empty(),
            "the refused variable must never have been written"
        );
    }

    #[test]
    fn duplicate_variable_names_are_rejected_before_the_second_write() {
        let mut scene = Scene::new();
        create_variable(
            &mut scene,
            "radius",
            10.0,
            "free",
            None,
            None,
            &Author::Agent,
        )
        .expect("first radius");
        let error = create_variable(
            &mut scene,
            " radius ",
            20.0,
            "free",
            None,
            None,
            &Author::Agent,
        )
        .expect_err("canonical duplicate must be rejected");

        assert!(error.contains("must be unique"), "{error}");
        assert_eq!(read(&scene).expect("read").variables.len(), 1);
    }

    #[test]
    fn independently_created_duplicate_names_fail_closed_after_merge() {
        let mut left = Scene::new();
        let mut right = Scene::new();
        for scene in [&mut left, &mut right] {
            create_variable(scene, "radius", 10.0, "free", None, None, &Author::Agent)
                .expect("create concurrent radius");
        }

        let right_state = right.encode_full().expect("serialize right");
        left.apply_update(&right_state)
            .expect("merge right into left");
        let error = read(&left).expect_err("merged duplicate names must fail closed");
        assert!(error.contains("must be unique"), "{error}");
    }
}
