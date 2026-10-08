use samepage_capability::{Batch, BatchEntry, Call, Capability, Composition, Plan, Record, View};
use samepage_doc::{host::Host, Actor, Caller};
use samepage_lanes::{Evidence, Facts, Lane, Lanes, Registry, Removal, Scan};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/lanes.rs"]
mod native;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/sources.rs"]
mod sources;
fn lane(line: u32) -> Lane {
    Lane {
        lane_id: "lane".into(),
        kind: "listener".into(),
        label: "test".into(),
        package: None,
        detail: "a listener".into(),
        evidence: Evidence {
            path: "src/main.rs".into(),
            line,
            snippet: "TcpListener::bind(...)".into(),
            sha256: "a".repeat(64),
        },
    }
}
#[test]
fn a_binding_within_five_lines_matches_and_six_does_not() {
    let l = lane(12);
    for n in [10, 17] {
        assert!(samepage_lanes::binding_matches(
            "./src/main.rs",
            &n.to_string(),
            &l
        ));
    }
    assert!(!samepage_lanes::binding_matches("src/main.rs", "18", &l));
    assert!(!samepage_lanes::binding_matches("other.rs", "12", &l));
}
#[test]
fn a_removal_whose_lane_vanished_from_the_scan_still_renders_and_can_read_gate_met() {
    let mut f = Facts {
        scan: Scan {
            lanes: vec![lane(4)],
            ..Default::default()
        },
        ..Default::default()
    };
    f.registry.removals.insert(
        "gone-lane-id".into(),
        Removal {
            obligation_id: "ATLAS-REMOVE-bbbb2222".into(),
            kind: "spawn".into(),
            label: "test".into(),
            path: "src/main.rs".into(),
            line: 12,
            snippet: "Command::new(\"node\")".into(),
        },
    );
    let v = View::new("r", vec![]).unwrap();
    let unmet = samepage_lanes::reconcile(&v, &f);
    assert_eq!(unmet["lanes"].as_array().unwrap().len(), 2);
    assert_eq!(unmet["lanes"][1]["removal"]["chip"], "REMOVE unmet");
    f.clean.insert("ATLAS-REMOVE-bbbb2222".into());
    assert_eq!(
        samepage_lanes::reconcile(&v, &f)["lanes"][1]["removal"]["chip"],
        "REMOVE met"
    );
}
#[test]
fn groups_are_bounded_and_unresolved_count_includes_hidden_findings() {
    let mut f = Facts::default();
    f.scan.lanes = (1..=44)
        .map(|n| {
            let mut l = lane(n);
            l.lane_id = n.to_string();
            l
        })
        .collect();
    let out = samepage_lanes::reconcile(&View::new("r", vec![]).unwrap(), &f);
    assert_eq!(out["unresolved_count"], 44);
    assert_eq!(out["groups"][0]["shown"].as_array().unwrap().len(), 20);
    assert_eq!(out["groups"][0]["more"], 24);
}
#[test]
fn stale_scan_cannot_pin_a_baseline() {
    let l = lane(1);
    let mut f = Facts::default();
    f.excerpt_hashes.insert("lane".into(), "b".repeat(64));
    assert!(samepage_lanes::pin_check(&l, &f).is_err());
    f.file_hashes
        .insert("lane".into(), l.evidence.sha256.clone());
    assert_eq!(samepage_lanes::pin_check(&l, &f).unwrap(), "b".repeat(64));
}
static SEQ: AtomicU64 = AtomicU64::new(0);
fn root() -> PathBuf {
    let r = std::env::temp_dir().join(format!(
        "lanes-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::create_dir_all(r.join(".local")).unwrap();
    std::fs::write(
        r.join("Cargo.toml"),
        "[package]\nname='lanes-proof'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(
        r.join("src/main.rs"),
        "fn main() {\n let _listener=std::net::TcpListener::bind(\"127.0.0.1:0\");\n}\n",
    )
    .unwrap();
    std::fs::write(r.join("other.rs"), "pub fn independent() {}\n").unwrap();
    r
}
#[test]
fn real_scan_claim_adds_a_pinned_binding_and_refuses_stale_or_absent_lanes_atomically() {
    let r = root();
    let state = r.join(".local/document.json");
    let sh = sources::SourcesHost::open(&r, &state).unwrap();
    let lh = Arc::new(native::LanesHost::open(&r, &state).unwrap());
    let h = Host::open(
        &state,
        Composition::new(vec![
            samepage_map::descriptor(),
            samepage_sources::descriptor(),
            samepage_lanes::descriptor(),
        ])
        .unwrap(),
        vec![
            Arc::new(samepage_map::Map),
            Arc::new(samepage_sources::Sources),
            Arc::new(Lanes),
        ],
    )
    .unwrap()
    .with_verifier("sources", Arc::new(sources::Repository::open(&r).unwrap()))
    .unwrap()
    .with_verifier("lanes", lh.clone())
    .unwrap();
    let a = Actor::new("lane-agent", Caller::Agent).labelled("Lane proof");
    let b = samepage_sources::place_batch(
        &Call {
            action: "card_place".into(),
            args: json!({"label":"Original","path":"other.rs","lines":"1"}),
        },
        &h.view(),
    )
    .unwrap();
    let node = h.invoke_composed(&a, "card_place", &b).unwrap().ids["card"].clone();
    let f = lh.facts(&h.view(), sh.facts(&h.view()).unwrap()).unwrap();
    let l = f.scan.lanes.iter().find(|l| l.kind == "listener").unwrap();
    let args = json!({"node_id":node,"lane_id":l.lane_id,"scan_sha256":l.evidence.sha256});
    let b = lh
        .prepare(args.clone(), &h.view(), sh.facts(&h.view()).unwrap())
        .unwrap();
    let legacy = lh
        .prepare(
            json!({"card_id":node,"lane_id":l.lane_id,"scan_sha256":l.evidence.sha256}),
            &h.view(),
            sh.facts(&h.view()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(&b).unwrap(),
        serde_json::to_value(&legacy).unwrap()
    );
    assert!(lh
        .prepare(
            json!({"card_id":"other","node_id":node,"lane_id":l.lane_id}),
            &h.view(),
            sh.facts(&h.view()).unwrap()
        )
        .is_err());
    let claimed = h.invoke_composed(&a, "atlas_claim_lane", &legacy).unwrap();
    assert_eq!(claimed.action, "lane_claim");
    assert_eq!(h.journal().seq, 2);
    assert_eq!(
        h.view().get(&node).unwrap().props["path"].as_str(),
        Some("other.rs")
    );
    let fact = sh.facts(&h.view()).unwrap();
    assert_eq!(fact.extras[&node].len(), 1);
    assert_eq!(
        samepage_sources::project(&h.view(), &fact).unwrap()["cards"][&node]["bindings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let binding = &claimed.ids["binding"];
    assert_eq!(
        h.view().get(binding).unwrap().props["created_by_participant"].as_str(),
        Some("lane-agent")
    );
    h.save().unwrap();
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    std::fs::write(
        r.join("src/main.rs"),
        "fn main() {\n let _listener=std::net::TcpListener::bind(\"127.0.0.1:1\");\n}\n",
    )
    .unwrap();
    assert!(lh
        .prepare(args, &h.view(), sh.facts(&h.view()).unwrap())
        .is_err());
    assert!(h.invoke_composed(&a, "lane_claim", &b).is_err());
    assert_eq!((h.revision(), h.encode_full(), h.journal().seq), before);
    std::fs::remove_file(r.join("src/main.rs")).unwrap();
    let f = lh.facts(&h.view(), sh.facts(&h.view()).unwrap()).unwrap();
    assert!(samepage_lanes::reconcile(&h.view(), &f)["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["removal"]["chip"] == "SOURCE VANISHED"));
    drop(lh);
    let reopened = native::LanesHost::open(&r, &state).unwrap();
    assert!(!reopened.registry.lock().unwrap().removals.is_empty());
    std::fs::remove_dir_all(r).unwrap();
}
#[test]
fn deletion_cascades_extra_bindings_without_changing_other_cards() {
    let v = View::new(
        "r",
        vec![
            Record::new("card", "node"),
            Record::new("other", "node"),
            Record::new("binding", samepage_lanes::KIND).with("node", "card"),
        ],
    )
    .unwrap();
    let p = Lanes.cascade(&["card".into()], &v).unwrap();
    assert_eq!(p.ops.len(), 1);
    let _: Batch = Batch {
        base_revision: "r".into(),
        entries: vec![BatchEntry {
            capability: "lanes".into(),
            plan: Plan {
                ops: p.ops,
                readback: String::new(),
            },
        }],
    };
    let _ = Registry::default();
}
