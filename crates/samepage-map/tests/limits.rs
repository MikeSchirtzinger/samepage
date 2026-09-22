//! Placement and growth limits, and transition links, through the real kernel.

use std::sync::Arc;

use base64::Engine as _;
use samepage_capability::{Capability, Composition, Scalar};
use samepage_doc::host::{FrameRefusal, Host, InvokeError};
use samepage_doc::{sync, Actor, Caller};
use samepage_map::rules::{
    grid_rows, grid_slot, GRID_COLUMNS, GRID_ORIGIN, GRID_STEP, MAX_MAP_OBJECTS, WORLD_LIMIT,
};
use samepage_map::Map;
use serde_json::json;
use yrs::updates::decoder::Decode;
use yrs::{Map as _, MapPrelim, Transact};

fn agent() -> Actor {
    Actor::new("p-agent", Caller::Agent).labelled("Model")
}

fn alice() -> Actor {
    Actor::new("p-alice", Caller::Human).labelled("Alice")
}

fn composition() -> Composition {
    Composition::new(vec![samepage_map::descriptor()]).unwrap()
}

fn fingerprint(host: &Host) -> (String, Vec<u8>, u64) {
    (host.revision(), host.encode_full(), host.journal().seq)
}

/// A legacy document with `cards` cards (and no links), imported read-only.
fn imported(cards: usize, name: &str) -> Host {
    let doc = yrs::Doc::new();
    let objects = doc.get_or_insert_map("objects");
    {
        let mut txn = doc.transact_mut();
        for index in 0..cards {
            let node = objects.insert(&mut txn, format!("n{index}").as_str(), MapPrelim::default());
            node.insert(&mut txn, "kind", "node");
            node.insert(&mut txn, "label", format!("Card {index}"));
            node.insert(&mut txn, "x", (index % 100) as f64 * 10.0);
            node.insert(&mut txn, "y", (index / 100) as f64 * 10.0);
        }
    }
    let state =
        yrs::ReadTxn::encode_state_as_update_v1(&doc.transact(), &yrs::StateVector::default());
    let dir = std::env::temp_dir().join(format!("samepage-map-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let legacy = dir.join("atlas.json");
    std::fs::write(
        &legacy,
        base64::engine::general_purpose::STANDARD.encode(state),
    )
    .unwrap();
    Host::import_legacy(
        &legacy,
        dir.join("page.samepage"),
        composition(),
        vec![Arc::new(Map)],
    )
    .unwrap()
}

#[test]
fn every_automatic_slot_up_to_the_limit_is_distinct_and_inside_the_world() {
    let mut seen = std::collections::BTreeSet::new();
    for index in 0..MAX_MAP_OBJECTS {
        let (x, y) = grid_slot(index);
        assert!(
            x.abs() <= WORLD_LIMIT && y.abs() <= WORLD_LIMIT,
            "slot {index} at ({x}, {y})"
        );
        assert!(seen.insert((x as i64, y as i64)), "slot {index} repeats");
    }
    // The first band is the original four-column layout.
    for index in 0..GRID_COLUMNS * grid_rows() {
        let expected = (
            GRID_ORIGIN.0 + (index % GRID_COLUMNS) as f64 * GRID_STEP.0,
            GRID_ORIGIN.1 + (index / GRID_COLUMNS) as f64 * GRID_STEP.1,
        );
        assert_eq!(grid_slot(index), expected);
    }
}

#[test]
fn five_hundred_and_more_default_placements_all_commit_inside_the_world() {
    let host = Host::in_memory(composition(), vec![Arc::new(Map)]).unwrap();
    for index in 0..520 {
        host.invoke(
            &agent(),
            "card_place",
            json!({ "label": format!("Card {index}") }),
        )
        .unwrap_or_else(|error| panic!("card {index}: {error}"));
    }
    let view = host.view();
    assert_eq!(view.len(), 520);
    for record in view.records() {
        for key in ["x", "y"] {
            let value = record.props[key].as_f64().unwrap();
            assert!(value.abs() <= WORLD_LIMIT, "{} {key} = {value}", record.id);
        }
    }
}

#[test]
fn growth_past_the_limit_is_refused_natively_and_from_a_replica() {
    let host = imported(MAX_MAP_OBJECTS, "full");
    let before = fingerprint(&host);
    for (action, args) in [
        ("card_place", json!({ "label": "One more", "x": 0, "y": 0 })),
        ("card_link", json!({ "from": "n0", "to": "n1" })),
    ] {
        match host.invoke(&agent(), action, args) {
            Err(InvokeError::Capability { error, .. }) => {
                assert_eq!(error.code, "full", "{action}")
            }
            other => panic!("{action}: expected full, got {other:?}"),
        }
        assert_eq!(fingerprint(&host), before);
    }

    // A replica that writes a new card directly is refused by validation.
    let peer = yrs::Doc::new();
    let objects = peer.get_or_insert_map("objects");
    peer.transact_mut()
        .apply_update(yrs::Update::decode_v1(&host.encode_full()).unwrap())
        .unwrap();
    let mark = yrs::ReadTxn::state_vector(&peer.transact());
    {
        let mut txn = peer.transact_mut();
        let id = format!("{:016x}-{:08x}", peer.client_id().get(), 0);
        let node = objects.insert(&mut txn, id.as_str(), MapPrelim::default());
        node.insert(&mut txn, "kind", "node");
        node.insert(&mut txn, "label", "Sneaked in");
        node.insert(&mut txn, "x", 1.0);
        node.insert(&mut txn, "y", 1.0);
    }
    let frame = sync::update(yrs::ReadTxn::encode_state_as_update_v1(
        &peer.transact(),
        &mark,
    ));
    let refusal = host.receive_frame(&alice(), &frame).unwrap_err();
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref error, .. } if error.code == "full"),
        "{refusal}"
    );
    assert_eq!(fingerprint(&host), before);

    // At the limit, edits and deletions still work; after a deletion there is
    // room for one more.
    host.invoke(&alice(), "card_move", json!({ "id": "n5", "x": 5, "y": 5 }))
        .unwrap();
    host.invoke(&alice(), "object_remove", json!({ "id": "n6" }))
        .unwrap();
    host.invoke(
        &agent(),
        "card_place",
        json!({ "label": "Fits now", "x": 0, "y": 0 }),
    )
    .unwrap();
}

#[test]
fn a_document_imported_over_the_limit_stays_editable_but_cannot_grow() {
    let host = imported(MAX_MAP_OBJECTS + 3, "over");
    host.invoke(&alice(), "card_move", json!({ "id": "n1", "x": 7, "y": 7 }))
        .unwrap();
    host.invoke(
        &alice(),
        "card_place",
        json!({ "id": "n2", "label": "Renamed" }),
    )
    .unwrap();
    host.invoke(&alice(), "object_remove", json!({ "id": "n3" }))
        .unwrap();
    let before = fingerprint(&host);
    match host.invoke(
        &agent(),
        "card_place",
        json!({ "label": "No", "x": 0, "y": 0 }),
    ) {
        Err(InvokeError::Capability { error, .. }) => assert_eq!(error.code, "full"),
        other => panic!("expected full, got {other:?}"),
    }
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_link_carrying_any_transition_key_is_never_relabelled_by_card_link() {
    let doc = yrs::Doc::new();
    let objects = doc.get_or_insert_map("objects");
    {
        let mut txn = doc.transact_mut();
        for id in ["a", "b", "c"] {
            let node = objects.insert(&mut txn, id, MapPrelim::default());
            node.insert(&mut txn, "kind", "node");
            node.insert(&mut txn, "label", id.to_uppercase());
        }
        let empty_event = objects.insert(&mut txn, "t1", MapPrelim::default());
        empty_event.insert(&mut txn, "kind", "edge");
        empty_event.insert(&mut txn, "from", "a");
        empty_event.insert(&mut txn, "to", "b");
        empty_event.insert(&mut txn, "label", "transition");
        empty_event.insert(&mut txn, "event", "");
        empty_event.insert(&mut txn, "guard", "ready");
        let order_only = objects.insert(&mut txn, "t2", MapPrelim::default());
        order_only.insert(&mut txn, "kind", "edge");
        order_only.insert(&mut txn, "from", "b");
        order_only.insert(&mut txn, "to", "c");
        order_only.insert(&mut txn, "label", "ordered");
        order_only.insert(&mut txn, "transition_order", 2.0);
    }
    let state =
        yrs::ReadTxn::encode_state_as_update_v1(&doc.transact(), &yrs::StateVector::default());
    let dir = std::env::temp_dir().join(format!("samepage-map-transitions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let legacy = dir.join("atlas.json");
    std::fs::write(
        &legacy,
        base64::engine::general_purpose::STANDARD.encode(state),
    )
    .unwrap();
    let host = Host::import_legacy(
        &legacy,
        dir.join("page.samepage"),
        composition(),
        vec![Arc::new(Map)],
    )
    .unwrap();

    let projection = Map.project(&host.view()).unwrap();
    for link in projection["links"].as_array().unwrap() {
        assert_eq!(link["transition"], true, "{link}");
    }
    host.invoke(
        &agent(),
        "card_link",
        json!({ "from": "a", "to": "b", "label": "plain" }),
    )
    .unwrap();
    host.invoke(
        &agent(),
        "card_link",
        json!({ "from": "b", "to": "c", "label": "plain" }),
    )
    .unwrap();
    let view = host.view();
    assert_eq!(
        view.get("t1").unwrap().props["label"],
        Scalar::from("transition")
    );
    assert_eq!(
        view.get("t1").unwrap().props["guard"],
        Scalar::from("ready")
    );
    assert_eq!(
        view.get("t2").unwrap().props["label"],
        Scalar::from("ordered")
    );
    assert_eq!(
        view.records().filter(|r| r.kind == "edge").count(),
        4,
        "two new plain links"
    );
}

#[test]
fn map_read_accepts_no_arguments_like_its_schema_says() {
    let host = Host::in_memory(composition(), vec![Arc::new(Map)]).unwrap();
    match host.invoke(&agent(), "map_read", json!({ "verbose": true })) {
        Err(InvokeError::Capability { error, .. }) => assert_eq!(error.code, "invalid_args"),
        other => panic!("expected invalid_args, got {other:?}"),
    }
    assert!(host.invoke(&agent(), "map_read", json!({})).is_ok());
}
