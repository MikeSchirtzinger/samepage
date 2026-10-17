//! The map capability driven through the real C2 kernel: plans resolve and
//! commit in the host, projections and read-back come back out, bylines are
//! the host's, refusals change nothing.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use samepage_capability::{Capability, Composition, Scalar, View};
use samepage_doc::host::{Commit, FrameRefusal, Host, InvokeError};
use samepage_doc::{Actor, Caller, sync};
use samepage_map::Map;
use serde_json::{Value as JsonValue, json};
use yrs::updates::decoder::Decode;
use yrs::{Map as _, MapPrelim, Transact};

fn host() -> Arc<Host> {
    let composition = Composition::new(vec![samepage_map::descriptor()]).unwrap();
    Arc::new(Host::in_memory(composition, vec![Arc::new(Map)]).unwrap())
}

fn agent() -> Actor {
    Actor::new("p-agent", Caller::Agent).labelled("Model")
}

fn alice() -> Actor {
    Actor::new("p-alice", Caller::Human).labelled("Alice")
}

fn project(host: &Host) -> JsonValue {
    Map.project(&host.view()).unwrap()
}

fn card<'a>(projection: &'a JsonValue, id: &str) -> &'a JsonValue {
    projection["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
}

fn place(host: &Host, args: JsonValue) -> String {
    host.invoke(&agent(), "card_place", args).unwrap().ids["card"].clone()
}

#[derive(Debug, PartialEq)]
struct Fingerprint(String, Vec<u8>, u64);

fn fingerprint(host: &Host) -> Fingerprint {
    Fingerprint(host.revision(), host.encode_full(), host.journal().seq)
}

fn commits(host: &Arc<Host>) -> Arc<Mutex<Vec<Commit>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    host.on_commit(move |commit| sink.lock().unwrap().push(commit.clone()));
    seen
}

#[test]
fn an_agent_places_a_card_through_the_legacy_alias_and_reads_it_back() {
    let host = host();
    let seen = commits(&host);
    let invoked = host
        .invoke(
            &agent(),
            "atlas_place",
            json!({ "label": "Queue", "note": "holds jobs", "tone": "crate" }),
        )
        .unwrap();
    assert_eq!(invoked.action, "card_place");
    let id = invoked.ids["card"].clone();
    assert_eq!(invoked.readback, vec!["Placed \"Queue\".".to_string()]);

    let projection = project(&host);
    let queue = card(&projection, &id);
    assert_eq!(queue["label"], "Queue");
    assert_eq!(queue["tone"], "crate");
    assert_eq!(queue["status"], "open");
    assert_eq!(
        (queue["x"].as_f64(), queue["y"].as_f64()),
        (Some(48.0), Some(64.0)),
        "first free grid slot"
    );
    assert_eq!(queue["width"], 232.0);
    assert_eq!(queue["height_source"], "estimated");
    assert_eq!(
        queue["created_by"], "Model",
        "the host's byline, not the plan's"
    );

    let readback = host.invoke(&agent(), "map_read", json!({})).unwrap();
    assert!(
        readback.readback[0].starts_with("MAP: 1 cards, 0 links."),
        "{:?}",
        readback.readback
    );
    assert!(readback.readback[0].contains("\"Queue\""));
    assert!(readback.commit.is_none());

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].action.as_deref(), Some("card_place"));
}

#[test]
fn move_resize_and_link_commit_with_their_actor() {
    let host = host();
    let a = place(&host, json!({ "label": "A", "x": 0, "y": 0 }));
    let b = place(&host, json!({ "label": "B", "x": 400, "y": 0 }));
    host.invoke(
        &alice(),
        "card_move",
        json!({ "id": a, "x": 12.5, "y": -40 }),
    )
    .unwrap();
    host.invoke(
        &alice(),
        "card_resize",
        json!({ "id": a, "h_pref": 96, "h_min": 64 }),
    )
    .unwrap();
    host.invoke(
        &agent(),
        "card_place",
        json!({ "id": b, "w": 400, "h_pref": 200, "w_min": 300, "h_min": 120 }),
    )
    .unwrap();
    let link = host
        .invoke(
            &agent(),
            "atlas_link",
            json!({ "from": a, "to": b, "label": "calls" }),
        )
        .unwrap();
    assert_eq!(link.action, "card_link");

    let projection = project(&host);
    let card_a = card(&projection, &a);
    assert_eq!(
        (card_a["x"].as_f64(), card_a["y"].as_f64()),
        (Some(12.5), Some(-40.0))
    );
    assert_eq!(card_a["height"], 96.0);
    assert_eq!(card_a["height_source"], "preferred");
    assert_eq!(card_a["legacy_h"], JsonValue::Null);
    assert_eq!(card_a["measured_known"], false);
    assert_eq!(card_a["touched_by"], "Alice");
    let card_b = card(&projection, &b);
    assert_eq!(
        (card_b["width"].as_f64(), card_b["height"].as_f64()),
        (Some(400.0), Some(200.0))
    );
    assert_eq!(card_b["height_source"], "preferred");
    let links = projection["links"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["label"], "calls");

    // Linking the same pair again relabels the existing link.
    host.invoke(
        &agent(),
        "card_link",
        json!({ "from": a, "to": b, "label": "depends on" }),
    )
    .unwrap();
    let projection = project(&host);
    assert_eq!(projection["links"].as_array().unwrap().len(), 1);
    assert_eq!(projection["links"][0]["label"], "depends on");
}

#[test]
fn removing_a_card_takes_its_links_and_frees_its_children_in_one_commit() {
    let host = host();
    let frame = place(&host, json!({ "label": "Frame" }));
    let child = place(&host, json!({ "label": "Child", "parent": frame }));
    let other = place(&host, json!({ "label": "Other" }));
    let link_in = host
        .invoke(&agent(), "card_link", json!({ "from": other, "to": frame }))
        .unwrap()
        .ids["link"]
        .clone();
    let link_out = host
        .invoke(&agent(), "card_link", json!({ "from": frame, "to": other }))
        .unwrap()
        .ids["link"]
        .clone();
    let keep = host
        .invoke(&agent(), "card_link", json!({ "from": other, "to": child }))
        .unwrap()
        .ids["link"]
        .clone();

    let seq = host.journal().seq;
    let commit = host
        .invoke(&alice(), "atlas_remove", json!({ "id": frame }))
        .unwrap()
        .commit
        .unwrap();
    assert_eq!(host.journal().seq, seq + 1, "one commit");
    let mut deleted = commit.deleted.clone();
    deleted.sort();
    let mut expected = vec![frame.clone(), link_in, link_out];
    expected.sort();
    assert_eq!(deleted, expected);
    assert_eq!(commit.modified, vec![child.clone()]);
    let projection = project(&host);
    assert_eq!(card(&projection, &child)["parent"], "");
    assert_eq!(projection["links"].as_array().unwrap().len(), 1);
    assert_eq!(projection["links"][0]["id"], keep);
    assert!(projection["problems"].as_array().unwrap().is_empty());
}

#[test]
fn invalid_input_is_refused_and_changes_nothing() {
    let host = host();
    let a = place(&host, json!({ "label": "A" }));
    let b = place(&host, json!({ "label": "B", "parent": a }));
    let before = fingerprint(&host);
    let cases: Vec<(&str, JsonValue, &str)> = vec![
        ("card_place", json!({}), "invalid_args"),
        ("card_place", json!({ "label": "  " }), "invalid"),
        ("card_place", json!({ "label": "x".repeat(141) }), "invalid"),
        ("card_place", json!({ "label": "bad\u{7}" }), "invalid"),
        (
            "card_place",
            json!({ "label": "T", "tone": "purple" }),
            "invalid",
        ),
        ("card_place", json!({ "label": "T", "x": 1e9 }), "invalid"),
        (
            "card_place",
            json!({ "label": "T", "x": "12" }),
            "invalid_args",
        ),
        ("card_place", json!({ "label": "T", "w": 5000 }), "invalid"),
        (
            "card_place",
            json!({ "label": "T", "h_pref": 10 }),
            "invalid",
        ),
        (
            "card_place",
            json!({ "label": "T", "wobble": 1 }),
            "invalid_args",
        ),
        (
            "card_place",
            json!({ "label": "T", "path": "src/lib.rs" }),
            "needs_capability",
        ),
        (
            "card_place",
            json!({ "id": a, "lines": "1-3" }),
            "needs_capability",
        ),
        ("card_place", json!({ "id": a, "parent": b }), "invalid"),
        ("card_place", json!({ "id": a, "parent": a }), "invalid"),
        (
            "card_place",
            json!({ "id": "ghost", "label": "x" }),
            "not_found",
        ),
        ("card_move", json!({ "id": a, "x": 1 }), "invalid_args"),
        (
            "card_move",
            json!({ "id": a, "x": 1, "y": -30000 }),
            "invalid",
        ),
        ("card_link", json!({ "from": a, "to": a }), "invalid"),
        (
            "card_link",
            json!({ "from": a, "to": "ghost" }),
            "not_found",
        ),
        ("object_remove", json!({ "id": "ghost" }), "not_found"),
    ];
    for (action, args, code) in cases {
        match host.invoke(&agent(), action, args.clone()) {
            Err(InvokeError::Capability { error, .. }) => {
                assert_eq!(error.code, code, "{action} {args}: {error}")
            }
            other => panic!("{action} {args}: expected {code}, got {other:?}"),
        }
        assert_eq!(
            fingerprint(&host),
            before,
            "{action} {args} changed the host"
        );
    }
}

#[test]
fn a_card_created_with_bad_geometry_by_a_replica_is_refused() {
    let host = host();
    place(&host, json!({ "label": "A" }));
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
        node.insert(&mut txn, "label", "Far");
        node.insert(&mut txn, "x", 1.0e9);
    }
    let frame = sync::update(yrs::ReadTxn::encode_state_as_update_v1(
        &peer.transact(),
        &mark,
    ));
    let before = fingerprint(&host);
    let refusal = host.receive_frame(&alice(), &frame).unwrap_err();
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref capability, .. } if capability == "map"),
        "{refusal}"
    );
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_replica_that_deletes_a_card_without_its_links_is_refused_and_one_with_the_cascade_commits() {
    let host = host();
    let a = place(&host, json!({ "label": "A" }));
    let b = place(&host, json!({ "label": "B" }));
    host.invoke(&agent(), "card_link", json!({ "from": a, "to": b }))
        .unwrap();
    let composition = Composition::new(vec![samepage_map::descriptor()]).unwrap();

    let mut replica = samepage_doc::Replica::new();
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    replica
        .set_composition(composition.descriptors().to_vec())
        .unwrap();

    // Without the cascade: the link would dangle.
    let bare = samepage_capability::Plan {
        ops: vec![samepage_capability::Op::Delete {
            target: samepage_capability::Target::Id(a.clone()),
        }],
        readback: String::new(),
    };
    replica.apply("map", bare).unwrap();
    let frame = replica.take_outbound().remove(0);
    let before = fingerprint(&host);
    let refusal = host.receive_frame(&alice(), &frame).unwrap_err();
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref error, .. } if error.code == "dangling"),
        "{refusal}"
    );
    assert_eq!(fingerprint(&host), before);

    // A fresh replica that plans the removal through the capability commits.
    replica.mark_refused("dangling");
    drop(replica);
    let mut replica = samepage_doc::Replica::new();
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    replica
        .set_composition(composition.descriptors().to_vec())
        .unwrap();
    let call = samepage_capability::Call {
        action: "object_remove".into(),
        args: json!({ "id": a }),
    };
    let plan = Map.plan(&call, &replica.view()).unwrap();
    replica.apply("map", plan).unwrap();
    let frame = replica.take_outbound().remove(0);
    let reply = host.receive_frame(&alice(), &frame).unwrap();
    assert!(matches!(
        reply.outcome,
        samepage_doc::host::FrameOutcome::Committed { .. }
    ));
    let projection = project(&host);
    assert_eq!(projection["cards"].as_array().unwrap().len(), 1);
    assert!(projection["links"].as_array().unwrap().is_empty());
}

#[test]
fn a_human_drag_from_the_replica_commits_with_the_host_byline() {
    let host = host();
    let seen = commits(&host);
    let a = place(&host, json!({ "label": "A" }));
    let mut replica = samepage_doc::Replica::new();
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    replica
        .set_composition(vec![samepage_map::descriptor()])
        .unwrap();
    let call = samepage_capability::Call {
        action: "card_move".into(),
        args: json!({ "id": a, "x": 300, "y": 20 }),
    };
    let plan = Map.plan(&call, &replica.view()).unwrap();
    replica.apply("map", plan).unwrap();
    for frame in replica.take_outbound() {
        host.receive_frame(&alice(), &frame).unwrap();
    }
    let projection = project(&host);
    assert_eq!(card(&projection, &a)["x"], 300.0);
    assert_eq!(card(&projection, &a)["touched_by"], "Alice");
    // Every peer converges on the broadcast.
    for commit in seen.lock().unwrap().iter().skip(1) {
        replica.receive(&commit.update).unwrap();
    }
    assert_eq!(replica.view(), host.view());
}

/// A document the Atlas wrote, imported read-only: keys other capabilities
/// own, legacy keys, and a legacy measured `h`.
fn legacy_host(tag: &str) -> Arc<Host> {
    let doc = yrs::Doc::new();
    let objects = doc.get_or_insert_map("objects");
    {
        let mut txn = doc.transact_mut();
        let node = objects.insert(&mut txn, "n1", MapPrelim::default());
        for (key, value) in [
            ("kind", yrs::Any::from("node")),
            ("label", "Legacy".into()),
            ("x", 10.0.into()),
            ("y", 20.0.into()),
            ("w", 240.0.into()),
            ("h", 88.0.into()),
            ("tone", "crate".into()),
            ("status", "agreed".into()),
            ("path", "src/main.rs".into()),
            ("lines", "1-20".into()),
            ("diagram_kind", "".into()),
            ("owner", "agent".into()),
            ("created_by", "agent".into()),
            ("touched_by", "agent".into()),
        ] {
            node.insert(&mut txn, key, value);
        }
        let n2 = objects.insert(&mut txn, "n2", MapPrelim::default());
        n2.insert(&mut txn, "kind", "node");
        n2.insert(&mut txn, "label", "Other");
        let edge = objects.insert(&mut txn, "e1", MapPrelim::default());
        edge.insert(&mut txn, "kind", "edge");
        edge.insert(&mut txn, "from", "n1");
        edge.insert(&mut txn, "to", "n2");
        edge.insert(&mut txn, "label", "");
        edge.insert(&mut txn, "event", "Submit");
    }
    let state =
        yrs::ReadTxn::encode_state_as_update_v1(&doc.transact(), &yrs::StateVector::default());
    let dir =
        std::env::temp_dir().join(format!("samepage-map-legacy-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let legacy = dir.join("atlas.json");
    std::fs::write(
        &legacy,
        base64::engine::general_purpose::STANDARD.encode(state),
    )
    .unwrap();
    let composition = Composition::new(vec![samepage_map::descriptor()]).unwrap();
    Arc::new(
        Host::import_legacy(
            &legacy,
            dir.join("page.samepage"),
            composition,
            vec![Arc::new(Map)],
        )
        .unwrap(),
    )
}

/// A card the Atlas wrote, with keys other capabilities own and legacy keys,
/// projects; a move keeps every foreign key untouched.
#[test]
fn legacy_cards_project_and_keep_foreign_keys_through_an_edit() {
    let host = legacy_host("project");

    let projection = project(&host);
    let legacy_card = card(&projection, "n1");
    assert_eq!(legacy_card["label"], "Legacy");
    // The legacy measured height survives as history and is never this
    // render's height.
    assert_eq!(legacy_card["legacy_h"], 88.0);
    assert_eq!(legacy_card["height_source"], "estimated");
    assert_ne!(legacy_card["height"], 88.0);
    assert_eq!(legacy_card["measured_known"], false);
    let mut other: Vec<String> = legacy_card["other_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    other.sort();
    assert_eq!(other, ["diagram_kind", "lines", "owner", "path"]);
    assert_eq!(projection["links"][0]["transition"], true);

    let before: View = host.view();
    host.invoke(
        &alice(),
        "card_move",
        json!({ "id": "n1", "x": 99, "y": 20 }),
    )
    .unwrap();
    let after = host.view();
    let old = before.get("n1").unwrap();
    let new = after.get("n1").unwrap();
    for key in [
        "path",
        "lines",
        "diagram_kind",
        "owner",
        "created_by",
        "label",
        "tone",
        "status",
        "w",
        "h",
    ] {
        assert_eq!(
            new.props.get(key),
            old.props.get(key),
            "{key} must be untouched"
        );
    }
    assert_eq!(new.props["x"], Scalar::Num(99.0));
    assert_eq!(new.props["touched_by"], Scalar::from("Alice"));
    // A transition link is not relabelled by card_link: a new plain link is made.
    host.invoke(
        &agent(),
        "card_link",
        json!({ "from": "n1", "to": "n2", "label": "plain" }),
    )
    .unwrap();
    assert_eq!(project(&host)["links"].as_array().unwrap().len(), 2);
}

// ── geometry: authored is shared, measured is not ────────────────────────

/// A peer holding the host's state, able to write raw records the way a
/// browser replica does.
struct Peer {
    doc: yrs::Doc,
    mark: yrs::StateVector,
}

impl Peer {
    fn of(host: &Host) -> Self {
        let doc = yrs::Doc::new();
        doc.get_or_insert_map("objects");
        doc.transact_mut()
            .apply_update(yrs::Update::decode_v1(&host.encode_full()).unwrap())
            .unwrap();
        let mark = yrs::ReadTxn::state_vector(&doc.transact());
        Self { doc, mark }
    }

    fn write(&mut self, id: &str, key: &str, value: f64) {
        let objects = self.doc.get_or_insert_map("objects");
        let mut txn = self.doc.transact_mut();
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, id) else {
            panic!("no record {id}");
        };
        record.insert(&mut txn, key, value);
    }

    fn frame(&self) -> Vec<u8> {
        sync::update(yrs::ReadTxn::encode_state_as_update_v1(
            &self.doc.transact(),
            &self.mark,
        ))
    }
}

#[test]
fn no_route_may_write_the_legacy_measured_height() {
    let host = host();
    let a = place(&host, json!({ "label": "A" }));

    // 1. A host action: there is no action that writes `h` at all.
    assert!(matches!(
        host.invoke(&alice(), "card_measure", json!({ "id": a, "h": 96 })),
        Err(InvokeError::UnknownAction(_))
    ));
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(&alice(), "card_resize", json!({ "id": a, "h": 96 })),
        Err(InvokeError::Capability { .. })
    ));
    assert_eq!(fingerprint(&host), before);

    // 2. An untrusted batch.
    let batch = samepage_capability::Batch {
        base_revision: host.revision(),
        entries: vec![samepage_capability::BatchEntry {
            capability: "map".into(),
            plan: samepage_capability::Plan {
                ops: vec![samepage_capability::Op::Set {
                    target: samepage_capability::Target::Id(a.clone()),
                    props: [("h".to_string(), samepage_capability::Value::from(96.0))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    let before = fingerprint(&host);
    let refused = host.commit_batch(&alice(), &batch).unwrap_err();
    assert!(format!("{refused}").contains("read-only"), "{refused}");
    assert_eq!(fingerprint(&host), before);

    // 3. A replica frame.
    let mut peer = Peer::of(&host);
    peer.write(&a, "h", 96.0);
    let before = fingerprint(&host);
    let refusal = host.receive_frame(&alice(), &peer.frame()).unwrap_err();
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref capability, .. } if capability == "map"),
        "{refusal}"
    );
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn resize_writes_the_authored_size_and_holds_the_pair_rules_against_the_card() {
    let host = host();
    let a = place(&host, json!({ "label": "A", "w": 400, "h_pref": 200 }));

    // One-sided resizes are judged against what the card will have.
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(&alice(), "card_resize", json!({ "id": a, "w_min": 500 })),
        Err(InvokeError::Capability { .. })
    ));
    assert!(matches!(
        host.invoke(&alice(), "card_resize", json!({ "id": a, "h_min": 300 })),
        Err(InvokeError::Capability { .. })
    ));
    assert!(
        matches!(
            host.invoke(&alice(), "card_resize", json!({ "id": a })),
            Err(InvokeError::Capability { .. })
        ),
        "a resize that authors nothing is refused"
    );
    assert_eq!(fingerprint(&host), before);

    let invoked = host
        .invoke(
            &alice(),
            "card_resize",
            json!({ "id": a, "w": 360, "w_min": 300, "h_pref": 180, "h_min": 120 }),
        )
        .unwrap();
    assert!(invoked.readback[0].starts_with("Resized"));
    let resized = card(&project(&host), &a).clone();
    assert_eq!(
        (
            resized["w"].as_f64(),
            resized["w_min"].as_f64(),
            resized["h_pref"].as_f64(),
            resized["h_min"].as_f64()
        ),
        (Some(360.0), Some(300.0), Some(180.0), Some(120.0))
    );
    assert_eq!(resized["height"], 180.0);
    assert_eq!(resized["height_source"], "preferred");
    assert_eq!(resized["touched_by"], "Alice");

    // Deliberate resize converges: a second peer reading the document sees the
    // same authored numbers, because they are in the document.
    let peer = Peer::of(&host);
    let objects = peer.doc.get_or_insert_map("objects");
    let txn = peer.doc.transact();
    let Some(yrs::Out::YMap(record)) = objects.get(&txn, a.as_str()) else {
        panic!()
    };
    assert_eq!(
        record.get(&txn, "h_pref").unwrap().to_string(&txn),
        "180".to_string()
    );
}

#[test]
fn a_card_with_no_authored_height_reports_an_estimate_and_never_claims_measurement() {
    let host = host();
    let a = place(
        &host,
        json!({ "label": "A card with a longer label", "note": "two\nlines" }),
    );
    let unsized_card = card(&project(&host), &a).clone();
    assert_eq!(unsized_card["height_source"], "estimated");
    assert_eq!(unsized_card["measured_known"], false);
    assert_eq!(unsized_card["legacy_h"], JsonValue::Null);
    assert!(unsized_card["height"].as_f64().unwrap() > 0.0);

    // An authored floor above what the content needs governs, and is still not
    // a measurement.
    host.invoke(&alice(), "card_resize", json!({ "id": a, "h_min": 400 }))
        .unwrap();
    let floored = card(&project(&host), &a).clone();
    assert_eq!(floored["height"], 400.0);
    assert_eq!(floored["height_source"], "preferred");
    assert_eq!(floored["measured_known"], false);
}

#[test]
fn a_same_value_write_to_a_legacy_height_is_still_a_write_and_is_refused() {
    // An imported card already carries `h: 88`. A change that writes 88 again
    // leaves no delta, so only `Change.attempted` sees it. Without that check
    // the write would commit, stamp a fresh byline on a record nobody edited,
    // and teach the peer that the key is writable.
    let host = legacy_host("same-value");
    let before = fingerprint(&host);
    let byline = card(&project(&host), "n1")["touched_by"].clone();
    assert_eq!(byline, "agent");

    let batch = |value: f64| samepage_capability::Batch {
        base_revision: host.revision(),
        entries: vec![samepage_capability::BatchEntry {
            capability: "map".into(),
            plan: samepage_capability::Plan {
                ops: vec![samepage_capability::Op::Set {
                    target: samepage_capability::Target::Id("n1".into()),
                    props: [("h".to_string(), samepage_capability::Value::from(value))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    // 88.0 is the same value; 96.0 changes it. Both are attempts to write a
    // read-only key, and both are refused.
    for value in [88.0, 96.0] {
        let refused = host.commit_batch(&alice(), &batch(value)).unwrap_err();
        assert!(
            format!("{refused}").contains("read-only"),
            "{value}: {refused}"
        );
    }

    // The same bytes as a replica frame: a same-value write produces no
    // visible change, so this is the case the delta alone would miss.
    let mut peer = Peer::of(&host);
    peer.write("n1", "h", 88.0);
    let refusal = host.receive_frame(&alice(), &peer.frame()).unwrap_err();
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref capability, .. } if capability == "map"),
        "{refusal}"
    );

    assert_eq!(fingerprint(&host), before, "no revision, no journal entry");
    let after = card(&project(&host), "n1").clone();
    assert_eq!(
        after["touched_by"], byline,
        "no new byline on an untouched record"
    );
    assert_eq!(after["legacy_h"], 88.0, "the legacy value survives");
    assert_eq!(after["height_source"], "estimated");
}

#[test]
fn a_short_preference_never_clips_the_content_and_a_long_one_governs() {
    let host = host();
    // A card whose content needs more than the preference it was given: the
    // content wins and the projection says so, rather than drawing a card too
    // short to read.
    let short = place(
        &host,
        json!({ "label": "A", "note": "a note long enough to need more than one line of room on this card" }),
    );
    let estimated = card(&project(&host), &short)["height"].as_f64().unwrap();
    host.invoke(
        &alice(),
        "card_resize",
        json!({ "id": short, "h_pref": 48 }),
    )
    .unwrap();
    let card_short = card(&project(&host), &short).clone();
    assert!(estimated > 48.0, "the content needs more than 48");
    assert_eq!(card_short["height"].as_f64(), Some(estimated));
    assert_eq!(card_short["height_source"], "estimated");
    assert_eq!(
        card_short["h_pref"], 48.0,
        "the preference is still authored"
    );

    // A preference above what the content needs governs.
    let long = place(&host, json!({ "label": "B" }));
    host.invoke(
        &alice(),
        "card_resize",
        json!({ "id": long, "h_pref": 400 }),
    )
    .unwrap();
    let card_long = card(&project(&host), &long).clone();
    assert_eq!(card_long["height"], 400.0);
    assert_eq!(card_long["height_source"], "preferred");
    assert_eq!(card_long["measured_known"], false);
}
