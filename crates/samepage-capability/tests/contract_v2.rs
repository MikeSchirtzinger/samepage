//! ABI v2: the property trigger, planner reach, and `plan_batch`.

mod common;

use samepage_capability::{
    ABI, ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CapError, Capability,
    ComposeError, Composition, Delta, Descriptor, Effect, Op, Plan, Trigger, Value, View,
};
use serde_json::json;

fn refuse(descriptors: Vec<Descriptor>) -> ComposeError {
    Composition::new(descriptors).expect_err("composition must be refused")
}

#[test]
fn the_abi_names_this_contract_revision() {
    assert_eq!(ABI, "samepage.capability.v2");
    assert_eq!(common::map().abi, ABI);
}

#[test]
fn triggers_keep_their_wire_shapes_and_add_a_property_trigger() {
    for (trigger, wire) in [
        (Trigger::Always, json!({ "on": "always" })),
        (
            Trigger::kind("node"),
            json!({ "on": "kind", "name": "node" }),
        ),
        (
            Trigger::tool("card_place"),
            json!({ "on": "tool", "name": "card_place" }),
        ),
        (
            Trigger::route("/diagram"),
            json!({ "on": "route", "name": "/diagram" }),
        ),
        (
            Trigger::property("edge", "event"),
            json!({ "on": "property", "kind": "edge", "key": "event" }),
        ),
    ] {
        assert_eq!(serde_json::to_value(&trigger).unwrap(), wire, "{trigger:?}");
        assert_eq!(serde_json::from_value::<Trigger>(wire).unwrap(), trigger);
    }
}

#[test]
fn a_trigger_names_data_the_capability_can_see_and_own() {
    // The case this exists for: a typed edge key, on a kind the capability
    // extends. A kind trigger would load it for every ordinary edge.
    let diagrams = Descriptor::new("diagrams", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "types the map's links")
        .extends("edge", &["event"], "typed transitions")
        .trigger(Trigger::property("edge", "event"));
    Composition::new(vec![common::map(), diagrams]).expect("property trigger on an extended key");

    let unseen = common::map().trigger(Trigger::kind("claim"));
    assert!(matches!(
        refuse(vec![unseen]),
        ComposeError::TriggerKindNotVisible { .. }
    ));
    let unseen_property = common::map().trigger(Trigger::property("claim", "verdict"));
    assert!(matches!(
        refuse(vec![unseen_property]),
        ComposeError::TriggerKindNotVisible { .. }
    ));
    let foreign_key = common::map().trigger(Trigger::property("node", "path"));
    assert!(matches!(
        refuse(vec![foreign_key]),
        ComposeError::TriggerKeyNotOwned { key, .. } if key == "path"
    ));
    // An owned kind and key are fine.
    Composition::new(vec![
        common::map()
            .trigger(Trigger::kind("node"))
            .trigger(Trigger::property("node", "label")),
    ])
    .expect("owned triggers");
}

#[test]
fn tool_and_route_triggers_keep_the_semantics_they_already_had() {
    // A tool trigger names the host's tool selection, which is not required to
    // be one of this capability's action names: a "diagram" tool may activate
    // a capability whose action is `diagram_draw`. A route is whatever the
    // host routes, including the root. This amendment adds the property
    // trigger; it introduces no tool or route policy.
    let diagrams = Descriptor::new("diagrams", "0.1.0", Artifact::Wasm)
        .depends_on("map", "0.1.0", "types the map's links")
        .extends("edge", &["event"], "typed transitions")
        .action(ActionDecl::new(
            "diagram_draw",
            Audience::Both,
            Effect::Mutate,
            "Draw a typed diagram.",
        ))
        .trigger(Trigger::tool("diagram"))
        .trigger(Trigger::route("/"));
    let composition = Composition::new(vec![common::map(), diagrams])
        .expect("a tool selection and the root route are both accepted");
    let triggers = &composition.descriptor("diagrams").unwrap().triggers;
    assert!(triggers.contains(&Trigger::tool("diagram")));
    assert!(triggers.contains(&Trigger::route("/")));
}

#[test]
fn a_planner_may_target_itself_and_its_declared_dependencies_only() {
    let a = Descriptor::new("a", "0.1.0", Artifact::Wasm).owns("a-record", &["x"], "a");
    let b = Descriptor::new("b", "0.1.0", Artifact::Wasm)
        .depends_on("a", "0.1.0", "b writes a's records")
        .owns("b-record", &["y"], "b");
    let c = Descriptor::new("c", "0.1.0", Artifact::Wasm)
        .depends_on("b", "0.1.0", "c writes b's records")
        .owns("c-record", &["z"], "c");
    let composition = Composition::new(vec![a, b, c]).unwrap();

    assert!(composition.may_target("c", "c"));
    assert!(composition.may_target("c", "b"), "declared dependency");
    assert!(
        !composition.may_target("c", "a"),
        "a dependency of a dependency is not declared by c and may not be targeted"
    );
    assert!(
        !composition.may_target("a", "b"),
        "dependencies do not run backwards"
    );
}

/// Plans one card and types it, in that order.
struct Typed;

impl Typed {
    fn descriptor() -> Descriptor {
        Descriptor::new("typed", "0.1.0", Artifact::Wasm)
            .depends_on("map", "0.1.0", "types the map's records")
            .extends("node", &["diagram_kind"], "typed cards")
    }
}

impl Capability for Typed {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Err(CapError::new(
            "use_plan_batch",
            "typed cards are planned as a batch",
        ))
    }
    fn plan_batch(&self, _call: &Call, view: &View) -> Result<Batch, CapError> {
        Ok(Batch {
            base_revision: view.revision().to_string(),
            entries: vec![
                BatchEntry {
                    capability: "map".into(),
                    plan: Plan {
                        ops: vec![Op::Create {
                            reference: "typed-1".into(),
                            kind: "node".into(),
                            props: [("label".to_string(), Value::from("State"))].into(),
                        }],
                        readback: "Placed one card.".into(),
                    },
                },
                BatchEntry {
                    capability: "typed".into(),
                    plan: Plan {
                        ops: vec![Op::Set {
                            target: samepage_capability::Target::Ref("typed-1".into()),
                            props: [("diagram_kind".to_string(), Value::from("state"))].into(),
                        }],
                        readback: "Typed it.".into(),
                    },
                },
            ],
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

/// Uses the default `plan_batch`.
struct Plain;

impl Capability for Plain {
    fn descriptor(&self) -> Descriptor {
        common::map()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan {
            ops: vec![Op::Create {
                reference: "card".into(),
                kind: "node".into(),
                props: [("label".to_string(), Value::from("A"))].into(),
            }],
            readback: "Placed A.".into(),
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
fn the_default_plan_batch_wraps_plan_in_one_self_owned_entry() {
    let view = common::view();
    let call = Call {
        action: "card_place".into(),
        args: json!({}),
    };
    let batch = Plain.plan_batch(&call, &view).unwrap();
    assert_eq!(batch.base_revision, view.revision());
    assert_eq!(batch.entries.len(), 1);
    assert_eq!(batch.entries[0].capability, "map");
    assert_eq!(batch.entries[0].plan, Plain.plan(&call, &view).unwrap());
}

#[test]
fn an_override_returns_ordered_entries_that_resolve_against_each_other() {
    let composition = Composition::new(vec![common::map(), Typed::descriptor()]).unwrap();
    let view = common::view();
    let call = Call {
        action: "diagram_draw".into(),
        args: json!({}),
    };
    let batch = Typed.plan_batch(&call, &view).unwrap();
    assert_eq!(
        batch
            .entries
            .iter()
            .map(|e| e.capability.as_str())
            .collect::<Vec<_>>(),
        ["map", "typed"],
        "creation before typing"
    );
    let resolved = batch
        .resolve(&composition, &view, &mut common::counter())
        .expect("the second entry resolves the first entry's reference");
    let id = resolved.ids()["typed-1"].clone();
    let record = resolved.projected().get(&id).unwrap();
    assert_eq!(record.kind, "node");
    assert_eq!(
        record.props["label"],
        samepage_capability::Scalar::from("State")
    );
    assert_eq!(
        record.props["diagram_kind"],
        samepage_capability::Scalar::from("state")
    );
    assert_eq!(
        resolved.readback(),
        ["Placed one card.".to_string(), "Typed it.".to_string()]
    );
}
