use samepage_capability::{Batch, BatchEntry, Call, Capability, Composition, Op, Plan, Target};
use samepage_doc::{Actor, Caller, Replica, host::Host};
use samepage_panes::{Panes, model};
use serde_json::{Value, json};
use std::sync::Arc;
fn agent() -> Actor {
    Actor::new("agent", Caller::Agent).labelled("Panes worker")
}
fn human() -> Actor {
    Actor::new("person-1", Caller::Human).labelled("Sam")
}
fn host() -> Host {
    Host::in_memory(
        Composition::new(vec![samepage_panes::descriptor()]).unwrap(),
        vec![Arc::new(Panes)],
    )
    .unwrap()
}
fn call(h: &Host, who: &Actor, name: &str, mut args: Value) -> samepage_doc::host::Invoked {
    args["expected_revision"] = json!(h.revision());
    h.invoke(who, name, args).unwrap()
}
fn put(h: &Host) {
    call(
        h,
        &agent(),
        "pane_put",
        json!({"id":"topic","title":"Topic","view":{"kind":"text","text":"Draft"}}),
    );
}
fn projection(h: &Host) -> Value {
    Panes.project(&h.view()).unwrap()
}
fn p(h: &Host) -> Value {
    projection(h)["panes"][0].clone()
}
fn old() -> Value {
    json!({"schema_version":1,"revision":3,"intent":"an older room","columns":2,"theme":model::Theme::default(),"log":[{"revision":3,"by":"agent","by_name":"Historical agent","by_id":"old-agent","said":"put up the old pane"}],"panes":[{"id":"old","title":"Written earlier","author":"agent","by_name":"Historical agent","by_id":"old-agent","view":{"kind":"text","text":"hello"},"span":1,"height":"auto","revision":3},{"id":"wide","title":"A wide one","author":"you","view":{"kind":"text","text":"hello"},"span":2,"height":"tall","revision":3,"mark":"important","note":"theirs","note_by":"old-person","note_by_name":"Sam"}]})
}
#[test]
fn migrating_the_same_document_twice_changes_nothing() {
    let mut raw = old();
    model::migrate(&mut raw).unwrap();
    let first = raw.clone();
    model::migrate(&mut raw).unwrap();
    assert_eq!(first, raw);
    let h = host();
    call(&h, &agent(), "panes_import", json!({"document":old()}));
    let first = (h.revision(), h.encode_full(), h.journal());
    let result = call(&h, &agent(), "panes_import", json!({"document":old()}));
    assert!(result.commit.is_none());
    assert_eq!(first, (h.revision(), h.encode_full(), h.journal()));
    assert_eq!(
        samepage_panes::encode_json(&projection(&h)["original"]),
        samepage_panes::encode_json(&old())
    );
}
#[test]
fn a_room_written_before_bylines_existed_still_opens() {
    let mut raw = old();
    raw["panes"][0].as_object_mut().unwrap().remove("by_name");
    raw["panes"][0].as_object_mut().unwrap().remove("by_id");
    model::migrate(&mut raw).unwrap();
    let doc: model::RoomDoc = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(doc.panes[0].by_name, None);
    assert!(doc.panes[1].spot.w > doc.panes[0].spot.w);
    assert!(doc.panes.iter().all(|p| p.pointed_path.is_empty()));
    let h = host();
    call(&h, &agent(), "panes_import", json!({"document":raw}));
    assert!(p(&h)["by_id"].is_null());
    assert_eq!(projection(&h)["panes"][1]["author"], "human");
}
#[test]
fn a_rewrite_keeps_the_mark_and_note_the_person_left() {
    let h = host();
    put(&h);
    call(
        &h,
        &human(),
        "pane_annotate",
        json!({"id":"topic","mark":"question","note":"which retry?"}),
    );
    let before = p(&h);
    call(
        &h,
        &agent(),
        "pane_put",
        json!({"id":"topic","title":"Answer","view":{"kind":"text","text":"Revised answer"},"size":"small"}),
    );
    let after = p(&h);
    assert_eq!(after["mark"], "question");
    assert_eq!(after["note"], "which retry?");
    assert_eq!(after["mark_by"], before["mark_by"]);
    assert_eq!(after["note_by"], before["note_by"]);
    assert_eq!(projection(&h)["panes"].as_array().unwrap().len(), 1);
    assert_eq!(after["by_id"], "agent");
}
#[test]
fn a_stale_revision_is_refused() {
    let h = host();
    let old = h.revision();
    put(&h);
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    let error=h.invoke(&agent(),"pane_put",json!({"expected_revision":old,"id":"two","title":"Two","view":{"kind":"text","text":"b"}})).unwrap_err().to_string();
    assert!(error.contains("revision conflict"));
    assert_eq!(before, (h.revision(), h.encode_full(), h.journal().seq));
}
#[test]
fn a_rewrite_leaves_a_pane_where_the_person_dragged_it() {
    let h = host();
    put(&h);
    call(
        &h,
        &human(),
        "pane_put",
        json!({"id":"topic","spot":{"x":173,"y":117,"w":420,"h":280}}),
    );
    let before = p(&h)["spot"].clone();
    call(
        &h,
        &agent(),
        "pane_put",
        json!({"id":"topic","title":"Answer","view":{"kind":"text","text":"answer"}}),
    );
    assert_eq!(before, p(&h)["spot"]);
    assert_eq!(p(&h)["layout_by_id"], "person-1");
}
#[test]
fn a_rewrite_keeps_the_size_the_person_dragged() {
    let h = host();
    put(&h);
    call(
        &h,
        &human(),
        "pane_put",
        json!({"id":"topic","spot":{"x":100,"y":100,"w":760,"h":380}}),
    );
    let before = p(&h)["spot"].clone();
    call(
        &h,
        &agent(),
        "pane_put",
        json!({"id":"topic","title":"Answer","view":{"kind":"text","text":"answer"},"size":"small"}),
    );
    assert_eq!(before, p(&h)["spot"]);
}
#[test]
fn the_agent_cannot_send_a_rectangle() {
    let h = host();
    put(&h);
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    let args = json!({"expected_revision":h.revision(),"id":"topic","spot":{"x":500,"y":50,"w":400,"h":300}});
    assert!(
        h.invoke(&agent(), "pane_put", args.clone())
            .unwrap_err()
            .to_string()
            .contains("rectangle")
    );
    let b = Panes
        .plan_batch(
            &Call {
                action: "pane_put".into(),
                args,
            },
            &h.view(),
        )
        .unwrap();
    assert!(h.commit_batch(&agent(), &b).is_err());
    let mut replica = Replica::new();
    replica
        .set_composition(vec![samepage_panes::descriptor()])
        .unwrap();
    for frame in h.hello() {
        replica.receive(&frame).unwrap();
    }
    replica.apply_batch(&b).unwrap();
    for frame in replica.take_outbound() {
        assert!(h.receive_frame(&agent(), &frame).is_err());
    }
    assert_eq!(before, (h.revision(), h.encode_full(), h.journal().seq));
}
#[test]
fn a_resize_does_not_read_back_as_a_move() {
    let h = host();
    put(&h);
    call(
        &h,
        &human(),
        "pane_put",
        json!({"id":"topic","spot":{"x":100,"y":100,"w":400,"h":300}}),
    );
    let before = projection(&h);
    let result = call(
        &h,
        &human(),
        "pane_put",
        json!({"id":"topic","spot":{"x":100,"y":100,"w":560,"h":300}}),
    );
    assert!(result.readback.join(" ").contains("resized"));
    let changes = Panes
        .changes(&before, &projection(&h), "agent")
        .unwrap()
        .join(" ");
    assert!(
        changes.contains("resized") && !changes.contains("moved"),
        "{changes}"
    );
    let before = projection(&h);
    call(
        &h,
        &human(),
        "pane_put",
        json!({"id":"topic","spot":{"x":220,"y":180,"w":560,"h":300}}),
    );
    let changes = Panes
        .changes(&before, &projection(&h), "agent")
        .unwrap()
        .join(" ");
    assert!(changes.contains("moved") && !changes.contains("resized"));
}
#[test]
fn two_people_who_chose_the_same_name_are_still_two_people() {
    let h = host();
    put(&h);
    call(
        &h,
        &human(),
        "pane_annotate",
        json!({"id":"topic","mark":"question"}),
    );
    let second = Actor::new("person-2", Caller::Human).labelled("Sam");
    call(
        &h,
        &second,
        "pane_annotate",
        json!({"id":"topic","note":"another person"}),
    );
    let pane = p(&h);
    assert_eq!(pane["mark_by_name"], pane["note_by_name"]);
    assert_eq!(pane["mark_by"], "person-1");
    assert_eq!(pane["note_by"], "person-2");
}
#[test]
fn a_source_pane_that_stopped_resolving_says_so_instead_of_vanishing() {
    let h = host();
    call(
        &h,
        &agent(),
        "pane_put",
        json!({"id":"source","title":"Source","view":{"kind":"source","path":"Cargo.toml","from":1,"to":2}}),
    );
    let mut facts = std::collections::BTreeMap::new();
    facts.insert(
        samepage_panes::view::source_key("Cargo.toml", Some(1), Some(2)),
        json!({"error":"the file is gone"}),
    );
    let value = samepage_panes::projection(&h.view(), &facts).unwrap();
    assert_eq!(value["panes"].as_array().unwrap().len(), 1);
    assert_eq!(value["panes"][0]["view"]["error"], "the file is gone");
    assert!(
        samepage_panes::describe_projection(&value)
            .unwrap()
            .contains("the file is gone")
    );
}
#[test]
fn the_document_survives_a_reopen() {
    let path = std::env::temp_dir().join(format!("panes-reopen-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let composition = || Composition::new(vec![samepage_panes::descriptor()]).unwrap();
    let h = Host::open(&path, composition(), vec![Arc::new(Panes)]).unwrap();
    call(&h, &agent(), "panes_import", json!({"document":old()}));
    call(
        &h,
        &human(),
        "pane_annotate",
        json!({"id":"old","note":"persist this"}),
    );
    let before = h.view();
    let journal = h.journal();
    h.save().unwrap();
    drop(h);
    let h = Host::open(&path, composition(), vec![Arc::new(Panes)]).unwrap();
    assert_eq!(before, h.view());
    assert_eq!(journal.seq, h.journal().seq);
    assert_eq!(journal.recent, h.journal().recent);
    assert_eq!(journal.latest, h.journal().latest);
    assert!(journal.reserved.is_subset(&h.journal().reserved));
    drop(h);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn annotations_and_legacy_bylines_are_protected_on_every_route() {
    let h = host();
    put(&h);
    assert!(
        h.invoke(
            &agent(),
            "pane_annotate",
            json!({"expected_revision":h.revision(),"id":"topic","mark":"agree"})
        )
        .is_err()
    );
    let b = Panes
        .plan_batch(
            &Call {
                action: "pane_annotate".into(),
                args: json!({"expected_revision":h.revision(),"id":"topic","mark":"agree"}),
            },
            &h.view(),
        )
        .unwrap();
    let before = h.revision();
    assert!(h.commit_batch(&agent(), &b).is_err());
    let id = p(&h)["record_id"].as_str().unwrap().to_string();
    let forged = Batch {
        base_revision: h.revision(),
        entries: vec![BatchEntry {
            capability: "panes".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(id),
                    props: [(
                        "historical".into(),
                        json!({"by_name":"Forged"}).to_string().into(),
                    )]
                    .into(),
                }],
                readback: "".into(),
            },
        }],
    };
    assert!(h.commit_batch(&human(), &forged).is_err());
    assert_eq!(before, h.revision());
}
#[test]
fn aliases_resolve_and_journal_the_preferred_names() {
    let h = host();
    let composition = Composition::new(vec![samepage_panes::descriptor()]).unwrap();
    for a in samepage_panes::descriptor().actions {
        for alias in a.aliases {
            assert_eq!(
                composition.resolve_action(&alias).unwrap().preferred,
                a.name
            );
        }
    }
    call(
        &h,
        &agent(),
        "room_put_pane",
        json!({"id":"topic","title":"Topic","view":{"kind":"text","text":"hello"}}),
    );
    assert_eq!(
        h.journal().recent.back().unwrap().action.as_deref(),
        Some("pane_put")
    );
}
#[test]
fn pane_cascade_removes_only_its_own_aspects() {
    let h = host();
    put(&h);
    call(
        &h,
        &human(),
        "pane_annotate",
        json!({"id":"topic","mark":"question"}),
    );
    let seq = h.journal().seq;
    call(&h, &agent(), "pane_remove", json!({"id":"topic"}));
    assert_eq!(seq + 1, h.journal().seq);
    assert!(h.view().is_empty());
}
#[test]
fn untouched_unknown_records_and_extension_props_survive() {
    use base64::Engine as _;
    use yrs::updates::decoder::Decode;
    use yrs::{Map, ReadTxn, Transact};
    let original = host();
    put(&original);
    let id = original
        .view()
        .records()
        .find(|r| r.kind == "pane")
        .unwrap()
        .id
        .clone();
    let doc = yrs::Doc::new();
    let objects = doc.get_or_insert_map("objects");
    {
        let mut txn = doc.transact_mut();
        txn.apply_update(yrs::Update::decode_v1(&original.encode_full()).unwrap())
            .unwrap();
        let yrs::Out::YMap(pane) = objects.get(&txn, &id).unwrap() else {
            panic!()
        };
        pane.insert(&mut txn, "future_property", "untouched");
        let opaque = objects.insert(&mut txn, "opaque", yrs::MapPrelim::default());
        opaque.insert(&mut txn, "kind", "future");
        opaque.insert(&mut txn, "data", "history");
    }
    let root = std::env::temp_dir().join(format!("panes-unknown-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let legacy = root.join("legacy.state");
    std::fs::write(
        &legacy,
        base64::engine::general_purpose::STANDARD.encode(
            doc.transact()
                .encode_state_as_update_v1(&yrs::StateVector::default()),
        ),
    )
    .unwrap();
    let path = root.join("document.json");
    let composition = || Composition::new(vec![samepage_panes::descriptor()]).unwrap();
    let h = Host::import_legacy(&legacy, &path, composition(), vec![Arc::new(Panes)]).unwrap();
    call(
        &h,
        &agent(),
        "pane_put",
        json!({"id":"topic","title":"Revision","view":{"kind":"text","text":"changed"}}),
    );
    assert_eq!(
        h.view().get("opaque").unwrap().props["data"],
        samepage_capability::Scalar::from("history")
    );
    assert_eq!(
        h.view().get(&id).unwrap().props["future_property"],
        samepage_capability::Scalar::from("untouched")
    );
    h.save().unwrap();
    let reopened = Host::open(&path, composition(), vec![Arc::new(Panes)]).unwrap();
    assert_eq!(h.view(), reopened.view());
    drop(h);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_document_fields_and_log_are_projected_without_impersonation() {
    let h = host();
    call(&h, &agent(), "panes_import", json!({"document":old()}));
    let value = projection(&h);
    assert_eq!(value["schema_version"], 3);
    assert_eq!(value["legacy_revision"], 3);
    assert_eq!(value["revision"], h.revision());
    assert_eq!(value["log"], old()["log"]);
    assert_eq!(value["panes"][0]["by_id"], "old-agent");
    let record = h
        .view()
        .records()
        .find(|r| {
            r.kind == "pane" && r.props["pane_id"] == samepage_capability::Scalar::from("old")
        })
        .unwrap()
        .clone();
    assert_eq!(
        record.props["touched_by_participant"],
        samepage_capability::Scalar::from("agent")
    );
}
#[test]
fn a_new_pane_can_be_pinned_and_its_removal_is_atomic() {
    let h = host();
    call(
        &h,
        &agent(),
        "pane_put",
        json!({"id":"pinned","title":"Pinned","view":{"kind":"text","text":"Keep it"},"pinned":true}),
    );
    assert_eq!(p(&h)["pinned"], true);
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    assert!(
        h.invoke(
            &agent(),
            "pane_remove",
            json!({"expected_revision":h.revision(),"id":"pinned"})
        )
        .is_err()
    );
    assert_eq!(before, (h.revision(), h.encode_full(), h.journal().seq));
}

#[test]
fn importing_a_matching_human_id_retains_history_and_never_mints_a_live_byline() {
    let h = host();
    let mut document = old();
    document["panes"][1]["by_id"] = json!(human().participant);
    document["panes"][1]["by_name"] = json!("Sam");
    document["panes"][1]["note_by"] = json!(human().participant);
    call(&h, &agent(), "panes_import", json!({"document":document}));
    let value = projection(&h);
    let imported = &value["panes"][1];
    assert_eq!(imported["by_id"], human().participant);
    assert_eq!(imported["historical"], true);
    assert_eq!(imported["note_historical"], true);
    assert_eq!(imported["imported_by"], "Panes worker");
    assert!(
        samepage_panes::describe_projection(&value)
            .unwrap()
            .contains("Sam (historical)")
    );
    call(
        &h,
        &human(),
        "pane_annotate",
        json!({"id":"wide","note":"A new note"}),
    );
    let current = &projection(&h)["panes"][1];
    assert_eq!(current["historical"], true);
    assert_eq!(current["note_historical"], false);
    assert_eq!(current["note_by"], human().participant);
}
