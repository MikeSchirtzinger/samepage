use samepage_capability::{Capability, Composition};
use samepage_doc::{host::Host, Actor, Caller};
use samepage_map::{validation, Map};
use serde_json::json;
use std::sync::Arc;
fn host() -> Host {
    Host::in_memory(
        Composition::new(vec![samepage_map::descriptor()]).unwrap(),
        vec![Arc::new(Map)],
    )
    .unwrap()
}
fn actor() -> Actor {
    Actor::new("writer", Caller::Human)
}
fn node(h: &Host, label: &str, x: f64) -> String {
    h.invoke(&actor(), "card_place", json!({"label":label,"x":x}))
        .unwrap()
        .ids["card"]
        .clone()
}
#[test]
fn a_measurement_does_not_look_like_a_human_edit() {
    let h = host();
    let a = node(&h, "grows locally", 0.0);
    let b = node(&h, "below", 0.0);
    h.invoke(&actor(), "card_move", json!({"id":b,"x":0,"y":240}))
        .unwrap();
    let before = (
        h.revision(),
        h.encode_full(),
        Map.digest(&h.view()).unwrap(),
    );
    assert_eq!(
        validation::report(&h.view(), &Default::default()).unwrap()["status"],
        "passed"
    );
    let measured = [(a, [0., 0., 232., 320.])].into();
    assert_eq!(
        validation::report(&h.view(), &measured).unwrap()["errors"][0]["code"],
        "card_overlap"
    );
    assert_eq!(
        before,
        (
            h.revision(),
            h.encode_full(),
            Map.digest(&h.view()).unwrap()
        )
    );
}
#[test]
fn constraint_authoring_refusals_store_nothing() {
    let h = host();
    let a = node(&h, "A", 0.0);
    let b = node(&h, "B", 400.0);
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    for args in [
        json!({"op":"align","members":[a,b]}),
        json!({"op":"sequence","members":[a]}),
        json!({"op":"group","members":[a,a]}),
        json!({"op":"group","members":[a,"missing"]}),
        json!({"op":"attaches","members":[a,b,a]}),
    ] {
        assert!(h.invoke(&actor(), "constraint_add", args).is_err());
    }
    assert_eq!(before, (h.revision(), h.encode_full(), h.journal().seq));
}
#[test]
fn two_constraint_cycle_is_the_minimal_unsat_core() {
    let h = host();
    let a = node(&h, "A", 0.);
    let b = node(&h, "B", 400.);
    let forward = h
        .invoke(
            &actor(),
            "constraint_add",
            json!({"op":"sequence","members":[a,b],"axis":"x"}),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let reverse = h
        .invoke(
            &actor(),
            "constraint_add",
            json!({"op":"sequence","members":[b,a],"axis":"x"}),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let mut expected = vec![forward, reverse];
    expected.sort();
    let p = Map.project(&h.view()).unwrap();
    assert_eq!(
        p["constraint_state"],
        json!({"status":"unsat","core":expected})
    );
    assert!(p["constraints"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["status"] == "sat"));
    assert!(h
        .invoke(&actor(), "map_read", json!({}))
        .unwrap()
        .readback
        .join("\n")
        .contains("UNSAT sequence cycle"));
}
#[test]
fn sequence_cycles_do_not_mix_axes() {
    let h = host();
    let a = node(&h, "A", 0.);
    let b = node(&h, "B", 400.);
    for (members, axis) in [(vec![a.clone(), b.clone()], "x"), (vec![b, a], "y")] {
        h.invoke(
            &actor(),
            "constraint_add",
            json!({"op":"sequence","members":members,"axis":axis}),
        )
        .unwrap();
    }
    assert_eq!(
        Map.project(&h.view()).unwrap()["constraint_state"]["status"],
        "sat"
    );
}
