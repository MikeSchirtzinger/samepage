//! Deterministic diagnostics shared by the host and browser WASM.

use crate::{Atlas, AtlasError, Problem, layout};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Validation {
    pub schema_version: u32,
    /// Content identity includes deletions and measurements. A Yrs state
    /// vector alone does not identify deletions and cannot certify a render.
    pub revision: String,
    pub errors: Vec<Problem>,
    pub warnings: Vec<Problem>,
    pub unmeasured: usize,
}

pub fn validate(atlas: &Atlas) -> Result<Validation, AtlasError> {
    let bytes = serde_json::to_vec(atlas).map_err(|error| error.to_string())?;
    let revision = format!("{:x}", Sha256::digest(bytes));
    let report = layout(atlas);
    let (warnings, mut errors): (Vec<_>, Vec<_>) =
        report.problems.into_iter().partition(|problem| {
            // Distance from peers and estimated text height are advisory. They
            // cannot prove an occlusion or an invalid relationship.
            matches!(problem.code, "stranded_node" | "label_overflow")
        });
    for edge in &atlas.edges {
        if atlas.node(&edge.from).is_none() || atlas.node(&edge.to).is_none() {
            errors.push(Problem {
                code: "missing_endpoint",
                ids: vec![edge.id.clone(), edge.from.clone(), edge.to.clone()],
                detail: "A relationship refers to a component that is missing.".into(),
            });
        }
    }
    Ok(Validation {
        schema_version: 1,
        revision,
        errors,
        warnings,
        unmeasured: report.unmeasured + atlas.shapes.iter().filter(|shape| !shape.measured).count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodePatch, measure_node, place_node, read};
    use ag_ui_canvas::scene::{Author, Scene};

    #[test]
    fn measured_overlap_reports_ids_and_clears_only_when_separated() {
        let mut scene = Scene::new();
        let ids: Vec<_> = [0.0, 100.0]
            .into_iter()
            .map(|x| {
                place_node(
                    &mut scene,
                    &NodePatch {
                        label: Some("component".into()),
                        x: Some(x),
                        y: Some(0.0),
                        w: Some(280.0),
                        ..Default::default()
                    },
                    &Author::Agent,
                )
                .unwrap()
            })
            .collect();
        let unmeasured = validate(&read(&scene).unwrap()).unwrap();
        assert_eq!(unmeasured.unmeasured, 2);
        for id in &ids {
            measure_node(&scene, id, 160.0).unwrap();
        }
        let overlapping = validate(&read(&scene).unwrap()).unwrap();
        assert_eq!(overlapping.unmeasured, 0);
        assert!(overlapping.errors.iter().any(
            |error| error.code == "card_overlap" && ids.iter().all(|id| error.ids.contains(id))
        ));
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(ids[1].clone()),
                x: Some(400.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        let repaired = validate(&read(&scene).unwrap()).unwrap();
        assert!(repaired.errors.is_empty());
        assert_ne!(overlapping.revision, repaired.revision);
    }

    #[test]
    fn identity_changes_on_delete_even_when_the_state_vector_does_not() {
        let mut scene = Scene::new();
        let id = place_node(
            &mut scene,
            &NodePatch {
                label: Some("temporary".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        let before = validate(&read(&scene).unwrap()).unwrap();
        let vector = scene.state_vector_v1().unwrap();
        crate::remove(&scene, &id).unwrap();
        let after = validate(&read(&scene).unwrap()).unwrap();
        assert_eq!(vector, scene.state_vector_v1().unwrap());
        assert_ne!(before.revision, after.revision);
    }

    #[test]
    fn text_shapes_require_browser_measurement_too() {
        let mut scene = Scene::new();
        let id = crate::place_shape(
            &mut scene,
            &crate::ShapePatch {
                form: Some("text".into()),
                label: Some("A paragraph on the canvas".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        assert_eq!(validate(&read(&scene).unwrap()).unwrap().unmeasured, 1);
        crate::measure_shape(&scene, &id, 80.0).unwrap();
        assert_eq!(validate(&read(&scene).unwrap()).unwrap().unmeasured, 0);
    }
}
