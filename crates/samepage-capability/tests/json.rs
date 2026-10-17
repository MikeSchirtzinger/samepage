mod common;

use samepage_capability::envelope;
use samepage_capability::{
    ABI, Call, CapError, Capability, Delta, Descriptor, Op, Plan, Record, Scalar, Target, Value,
    View,
};
use serde_json::json;

#[test]
fn descriptors_round_trip_and_refuse_unknown_fields() {
    for descriptor in [common::map(), common::sources(), common::sketch()] {
        let text = serde_json::to_string(&descriptor).unwrap();
        let back: Descriptor = serde_json::from_str(&text).unwrap();
        assert_eq!(back, descriptor);
    }
    let mut value = serde_json::to_value(common::map()).unwrap();
    value["wildcard"] = json!(true);
    assert!(serde_json::from_value::<Descriptor>(value).is_err());

    let artifact = serde_json::to_value(common::map().artifact).unwrap();
    assert_eq!(artifact, json!({ "type": "wasm" }));
}

#[test]
fn ops_have_one_readable_json_form() {
    let op: Op = serde_json::from_value(json!({
        "op": "create",
        "ref": "card",
        "kind": "node",
        "props": { "label": "API", "x": 3, "pinned": true, "parent": { "$ref": "frame" } }
    }))
    .unwrap();
    let Op::Create {
        reference, props, ..
    } = &op
    else {
        panic!("create")
    };
    assert_eq!(reference, "card");
    assert_eq!(props["label"], Value::Scalar(Scalar::from("API")));
    assert_eq!(props["x"], Value::Scalar(Scalar::Num(3.0)));
    assert_eq!(props["pinned"], Value::Scalar(Scalar::Bool(true)));
    assert_eq!(props["parent"], Value::reference("frame"));

    let set: Op = serde_json::from_value(json!({
        "op": "set", "target": { "ref": "card" }, "props": { "w": 320 }
    }))
    .unwrap();
    assert!(matches!(set, Op::Set { target: Target::Ref(ref r), .. } if r == "card"));
    let delete: Op =
        serde_json::from_value(json!({ "op": "delete", "target": { "id": "n1" } })).unwrap();
    assert!(matches!(delete, Op::Delete { target: Target::Id(ref id) } if id == "n1"));

    let round = serde_json::to_value(&op).unwrap();
    assert_eq!(serde_json::from_value::<Op>(round).unwrap(), op);
}

#[test]
fn views_refuse_duplicates_kind_in_props_and_empty_ids() {
    let duplicate = json!({ "revision": "r", "records": [
        { "id": "a", "kind": "node" }, { "id": "a", "kind": "edge" }
    ]});
    assert!(serde_json::from_value::<View>(duplicate).is_err());
    let kind_prop = json!({ "revision": "r", "records": [
        { "id": "a", "kind": "node", "props": { "kind": "edge" } }
    ]});
    assert!(serde_json::from_value::<View>(kind_prop).is_err());
    let empty = json!({ "revision": "r", "records": [{ "id": "", "kind": "node" }]});
    assert!(serde_json::from_value::<View>(empty).is_err());
    assert!(View::new("r", vec![Record::new("a", "node").with("x", f64::INFINITY)]).is_err());

    let view = common::view();
    let back: View = serde_json::from_str(&serde_json::to_string(&view).unwrap()).unwrap();
    assert_eq!(back, view);
    assert_eq!(view.of_kinds(&["edge"]).len(), 1);
}

/// A capability used only to exercise the calling convention.
struct Notes;

impl Capability for Notes {
    fn descriptor(&self) -> Descriptor {
        common::map()
    }

    fn project(&self, view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({ "cards": view.of_kinds(&["node"]).len() }))
    }

    fn plan(&self, call: &Call, _view: &View) -> Result<Plan, CapError> {
        match call.action.as_str() {
            "card_place" => {
                let label = call.args["label"]
                    .as_str()
                    .ok_or_else(|| CapError::new("invalid_args", "label is required"))?;
                Ok(Plan {
                    ops: vec![Op::Create {
                        reference: "card".into(),
                        kind: "node".into(),
                        props: [("label".to_string(), Value::from(label))].into(),
                    }],
                    readback: format!("Placed {label}."),
                })
            }
            other => Err(CapError::new("unknown_action", other)),
        }
    }

    fn validate(&self, delta: &Delta, _view: &View) -> Result<(), CapError> {
        if delta.deleted.contains("n1") {
            return Err(CapError::new("protected", "n1 is protected"));
        }
        Ok(())
    }

    fn digest(&self, view: &View) -> Result<serde_json::Value, CapError> {
        self.project(view)
    }
}

fn view_json() -> String {
    serde_json::to_string(&common::view()).unwrap()
}

#[test]
fn the_envelope_carries_results_and_errors_without_panicking() {
    let descriptor: Descriptor = envelope::decode(&envelope::descriptor(&Notes)).unwrap();
    assert_eq!(descriptor.id, "map");

    let projected: serde_json::Value =
        envelope::decode(&envelope::project(&Notes, &view_json())).unwrap();
    assert_eq!(projected, json!({ "cards": 2 }));

    let call = json!({ "action": "card_place", "args": { "label": "Queue" } }).to_string();
    // Under ABI v2 the planning export answers with a Batch.
    let batch: samepage_capability::Batch =
        envelope::decode(&envelope::plan(&Notes, &call, &view_json())).unwrap();
    assert_eq!(batch.entries.len(), 1);
    assert_eq!(batch.entries[0].capability, "map");
    assert_eq!(batch.entries[0].plan.readback, "Placed Queue.");

    // A batch from the envelope resolves against the composition like any other.
    let resolved = samepage_capability::Batch {
        base_revision: "rev-1".into(),
        entries: batch.entries,
    }
    .resolve(
        &common::composition(),
        &common::view(),
        &mut common::counter(),
    )
    .unwrap();
    assert_eq!(resolved.ids()["card"], "id-0");

    let refused = json!({ "action": "card_place", "args": {} }).to_string();
    let error = envelope::decode::<samepage_capability::Batch>(&envelope::plan(
        &Notes,
        &refused,
        &view_json(),
    ))
    .unwrap_err();
    assert_eq!(error.code, "invalid_args");

    let garbage = envelope::plan(&Notes, "{not json", &view_json());
    let error = envelope::decode::<samepage_capability::Batch>(&garbage).unwrap_err();
    assert_eq!(error.code, "bad_input");
    assert!(error.message.starts_with("call:"), "{}", error.message);

    let delta = serde_json::to_string(&Delta {
        deleted: ["n1".to_string()].into(),
        ..Delta::default()
    })
    .unwrap();
    let error =
        envelope::decode::<()>(&envelope::validate(&Notes, &delta, &view_json())).unwrap_err();
    assert_eq!(error.code, "protected");

    let changes: Vec<String> =
        envelope::decode(&envelope::changes(&Notes, "{}", "{}", "agent")).unwrap();
    assert!(changes.is_empty());
    let cascade: Plan =
        envelope::decode(&envelope::cascade(&Notes, "[\"n1\"]", &view_json())).unwrap();
    assert!(cascade.ops.is_empty());
}

#[test]
fn an_envelope_from_another_abi_is_refused() {
    let foreign = json!({ "abi": "samepage.capability.v0", "ok": 1 }).to_string();
    assert_eq!(
        envelope::decode::<i32>(&foreign).unwrap_err().code,
        "abi_mismatch"
    );
    let current = json!({ "abi": ABI, "ok": 1 }).to_string();
    assert_eq!(envelope::decode::<i32>(&current).unwrap(), 1);
}

/// Computes a number from its arguments and puts it in a plan, the way a
/// real capability derives geometry. `divide_by: 0` yields a non-finite value.
struct Ratio;

impl Ratio {
    fn computed(args: &serde_json::Value) -> f64 {
        args["x"].as_f64().unwrap_or(0.0) / args["divide_by"].as_f64().unwrap_or(1.0)
    }
}

impl Capability for Ratio {
    fn descriptor(&self) -> Descriptor {
        common::map()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan {
            ops: vec![Op::Set {
                target: Target::Id("n1".into()),
                props: [("x".to_string(), Value::from(Self::computed(&call.args)))].into(),
            }],
            readback: String::new(),
        })
    }
    fn cascade(&self, deleted: &[String], _view: &View) -> Result<Plan, CapError> {
        let divide_by: f64 = deleted[0].parse().unwrap_or(1.0);
        let x: f64 = deleted.get(1).and_then(|v| v.parse().ok()).unwrap_or(1.0);
        Ok(Plan {
            ops: vec![Op::Set {
                target: Target::Id("n1".into()),
                props: [("x".to_string(), Value::from(x / divide_by))].into(),
            }],
            readback: String::new(),
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

#[test]
fn a_non_finite_number_in_a_plan_is_an_error_envelope_never_ok() {
    for (x, divide_by, what) in [(0.0, 0.0, "NaN"), (1.0, 0.0, "+inf"), (-1.0, 0.0, "-inf")] {
        let call = json!({ "action": "card_place", "args": { "x": x, "divide_by": divide_by } })
            .to_string();
        let text = envelope::plan(&Ratio, &call, &view_json());
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            parsed.get("ok").is_none(),
            "{what}: plan returned ok: {text}"
        );
        assert_eq!(
            parsed["error"]["code"], "non_finite_output",
            "{what}: {text}"
        );
        assert!(
            !text.contains("null"),
            "{what}: no value may be silently nulled: {text}"
        );

        let deleted = serde_json::to_string(&[divide_by.to_string(), x.to_string()]).unwrap();
        let text = envelope::cascade(&Ratio, &deleted, &view_json());
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            parsed.get("ok").is_none(),
            "{what}: cascade returned ok: {text}"
        );
        assert_eq!(
            parsed["error"]["code"], "non_finite_output",
            "{what}: {text}"
        );
    }
    // A finite computed value still goes through.
    let call =
        json!({ "action": "card_place", "args": { "x": 9.0, "divide_by": 3.0 } }).to_string();
    let batch: samepage_capability::Batch =
        envelope::decode(&envelope::plan(&Ratio, &call, &view_json())).unwrap();
    assert!(
        matches!(&batch.entries[0].plan.ops[0], Op::Set { props, .. } if props["x"] == Value::from(3.0))
    );
}

#[test]
fn a_non_finite_scalar_refuses_to_serialize() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(serde_json::to_string(&Scalar::Num(value)).is_err());
        assert!(serde_json::to_value(Value::from(value)).is_err());
        assert!(serde_json::to_string(&Record::new("a", "node").with("x", value)).is_err());
    }
    assert_eq!(serde_json::to_string(&Scalar::Num(1.5)).unwrap(), "1.5");
}
