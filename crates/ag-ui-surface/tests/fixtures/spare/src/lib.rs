//! Fixture capability: one kind, one action, exported to WebAssembly.

use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CapError, Capability, Delta, Descriptor, Effect,
    LegendEntry, Op, Plan, Target, Value, View, Trigger,
};
use serde_json::json;

pub struct Spare;

impl Capability for Spare {
    fn descriptor(&self) -> Descriptor {
        Descriptor::new("spare", "0.0.0", Artifact::Wasm)
            .depends_on("adapter", "1", "host adapter fixture")
            .trigger(Trigger::Route("/spare".into()))
            .owns("spare", &["label", "x", "y"], "fixture records")
            .action(
                ActionDecl::new("spare_add", Audience::Both, Effect::Mutate, "Add a spare.")
                    .alias("fixture_spare"),
            )
            .action(ActionDecl::new(
                "spare_move",
                Audience::Both,
                Effect::Mutate,
                "Move a spare.",
            ))
    }

    fn project(&self, view: &View) -> Result<serde_json::Value, CapError> {
        let spare: Vec<_> = view
            .records()
            .filter(|record| record.kind == "spare")
            .map(|record| json!({ "id": record.id, "label": record.props.get("label") }))
            .collect();
        Ok(json!({ "spare": spare }))
    }

    fn plan(&self, call: &Call, view: &View) -> Result<Plan, CapError> {
        match call.action.as_str() {
            "spare_add" => {
                let label = call.args["label"]
                    .as_str()
                    .ok_or_else(|| CapError::new("invalid_args", "label is required"))?;
                Ok(Plan {
                    ops: vec![Op::Create {
                        reference: "spare".into(),
                        kind: "spare".into(),
                        props: [
                            ("label".to_string(), Value::from(label)),
                            ("x".to_string(), Value::from(0.0)),
                        ]
                        .into(),
                    }],
                    readback: format!("Added spare {label}."),
                })
            }
            "spare_move" => {
                let id = call.args["id"]
                    .as_str()
                    .ok_or_else(|| CapError::new("invalid_args", "id is required"))?;
                if view.get(id).is_none() {
                    return Err(CapError::new("not_found", format!("no spare {id}")));
                }
                let x = call.args["x"]
                    .as_f64()
                    .ok_or_else(|| CapError::new("invalid_args", "x is required"))?;
                // Computed geometry, so a zero divisor yields NaN or an infinity
                // inside the module, the case the envelope must refuse.
                let x = x / call.args["divide_by"].as_f64().unwrap_or(1.0);
                Ok(Plan {
                    ops: vec![Op::Set {
                        target: Target::Id(id.to_string()),
                        props: [("x".to_string(), Value::from(x))].into(),
                    }],
                    readback: format!("Moved {id}."),
                })
            }
            other => Err(CapError::new("unknown_action", other)),
        }
    }

    fn validate(&self, delta: &Delta, view: &View) -> Result<(), CapError> {
        for id in delta.created.iter().chain(delta.modified.keys()) {
            if let Some(record) = view.get(id) {
                if record.kind == "spare" && !record.props.contains_key("label") {
                    return Err(CapError::new("invalid_spare", format!("{id} has no label")));
                }
            }
        }
        Ok(())
    }

    /// Deleting `ratio:<x>:<divisor>` asks to move `p1` to x / divisor, so the
    /// check can drive a computed non-finite number through `cascade`.
    fn cascade(&self, deleted: &[String], _view: &View) -> Result<Plan, CapError> {
        let mut ops = Vec::new();
        for id in deleted {
            let mut parts = id.split(':');
            if parts.next() != Some("ratio") {
                continue;
            }
            let x: f64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let divisor: f64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1.0);
            ops.push(Op::Set {
                target: Target::Id("p1".into()),
                props: [("x".to_string(), Value::from(x / divisor))].into(),
            });
        }
        Ok(Plan {
            ops,
            readback: String::new(),
        })
    }

    fn digest(&self, view: &View) -> Result<serde_json::Value, CapError> {
        self.project(view)
    }

    fn legend(&self, _view: &View) -> Result<Vec<LegendEntry>, CapError> {
        Ok(vec![LegendEntry {
            capability: "spare".into(),
            attribute: "kind".into(),
            value: "spare".into(),
            label: "A spare".into(),
            class: Some("spare".into()),
            symbol: Some("•".into()),
        }])
    }
}

samepage_capability::export_capability!(Spare, Spare);
