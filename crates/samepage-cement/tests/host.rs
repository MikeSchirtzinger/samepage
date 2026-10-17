use samepage_capability::{Call, Composition, Record, View};
use samepage_cement::{Cement, decision::DraftInput};
use samepage_doc::{Actor, Caller, host::Host};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/diagram_sources.rs"]
mod diagram_sources;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/lanes.rs"]
mod lanes;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/cement.rs"]
mod native;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/review.rs"]
mod review;
#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/sources.rs"]
mod sources;
fn pair() -> View {
    View::new(
        "r",
        vec![
            Record::new("n", "node")
                .with("label", "Engine")
                .with("status", "agreed"),
            Record::new("other", "node")
                .with("label", "Other")
                .with("status", "agreed"),
            Record::new("c", "claim")
                .with("about", "n")
                .with("text", "Reviewed")
                .with("basis", "inferred")
                .with("verdict", "accepted"),
        ],
    )
    .unwrap()
}
#[test]
fn merge_and_write_upserts_by_id_and_preserves_untouched_entries() {
    let existing = json!({"meta":{"keep":"yes"},"obligations":[{"id":"A","note":"old"},{"id":"B","note":"retain"}]});
    let merged = samepage_cement::merge(
        existing,
        vec![json!({"id":"A","note":"new"}), json!({"id":"C"})],
    )
    .unwrap();
    assert_eq!(merged["meta"]["keep"], "yes");
    assert_eq!(merged["obligations"][0]["note"], "new");
    assert_eq!(merged["obligations"][1]["note"], "retain");
    assert_eq!(merged["obligations"].as_array().unwrap().len(), 3);
}
#[test]
fn unsettled_and_disputed_assertions_fail_closed_and_name_every_refusal() {
    let e = samepage_cement::advisory(
        &[
            json!({"id":"first","status":"open"}),
            json!({"id":"second","status":"disputed"}),
        ],
        "r",
        "now",
    )
    .unwrap_err();
    assert!(e.to_string().contains("first") && e.to_string().contains("second"));
    let v = View::new(
        "r",
        vec![
            Record::new("n", "node").with("status", "agreed"),
            Record::new("a", "claim")
                .with("about", "n")
                .with("basis", "inferred")
                .with("verdict", "open"),
            Record::new("b", "claim")
                .with("about", "n")
                .with("basis", "inferred")
                .with("verdict", "rejected"),
        ],
    )
    .unwrap();
    let e = samepage_cement::settled(&v, &["n".into()])
        .unwrap_err()
        .to_string();
    assert!(e.contains("claim a") && e.contains("claim b"));
}
#[test]
fn the_draft_is_explicitly_advisory_and_prose_only() {
    let a = json!({"id":"A","text":"Agreed words","status":"settled","human_mark":"agree","signed_by":"human","authored_by":"agent","signed_revision":2,"changed_revision":2,"authored_revision":1,"authored_at":"then","signed_at":"now"});
    let p = samepage_cement::advisory(std::slice::from_ref(&a), "r", "now").unwrap();
    assert_eq!(p["receipt"]["assertions"][0], a);
    assert_eq!(p["obligations"]["meta"]["advisory_only"], true);
    assert_eq!(
        p["obligations"]["obligations"][0]["checker"]["mode"],
        "prose_only"
    );
    assert_eq!(p["obligations"]["obligations"][0]["signal"]["gate"], "none");
}
#[test]
fn cement_coverage_tracks_the_whole_selection_but_not_geometry_or_outside_edits() {
    let v = pair();
    let ids = vec!["n".into()];
    let h = samepage_cement::content(&v, &ids).unwrap();
    let mut records: Vec<_> = v.records().cloned().collect();
    records
        .iter_mut()
        .find(|n| n.id == "n")
        .unwrap()
        .props
        .insert("x".into(), 42.0.into());
    records
        .iter_mut()
        .find(|n| n.id == "other")
        .unwrap()
        .props
        .insert("label".into(), "unrelated".into());
    let moved = View::new("next", records.clone()).unwrap();
    assert_eq!(samepage_cement::content(&moved, &ids).unwrap(), h);
    records
        .iter_mut()
        .find(|n| n.id == "c")
        .unwrap()
        .props
        .insert("text".into(), "Changed accepted claim".into());
    assert_ne!(
        samepage_cement::content(&View::new("next", records).unwrap(), &ids).unwrap(),
        h
    );
    let both = samepage_cement::content(&v, &["n".into(), "other".into()]).unwrap();
    let mut rs: Vec<_> = v.records().cloned().collect();
    rs.push(
        Record::new("edge", "edge")
            .with("from", "n")
            .with("to", "other")
            .with("label", "new dependency"),
    );
    assert_ne!(
        samepage_cement::content(
            &View::new("next", rs).unwrap(),
            &["n".into(), "other".into()]
        )
        .unwrap(),
        both
    );
}
#[test]
fn legacy_cement_is_history_and_cannot_attest_current_content() {
    let v = pair();
    let n = Record::new("n", "node").with("cemented", "old.md\nold-revision");
    assert!(!samepage_cement::covers(&v, &n));
}
fn input(node: &str) -> Value {
    json!({"ids":[node],"title":"State contract","decision":"Keep State explicit","rationale":"Reviewable variants","alternatives":[{"option":"Strings","reason_not_chosen":"No exhaustive matching"}],"tradeoffs":"Changes need review","consequences":"Gate detects drift","checks":[{"id":"CEMENT-DEMO-STATE","requirement":"State has Ready and Done","check":{"backend":"rust_enum_shape","args":{"file":"src/lib.rs","enum_name":"State","expected_variants":["Ready","Done"],"expected_serde_rename_all":null}},"evidence_files":["src/lib.rs"]}],"not_enforced":["Runtime behavior"]})
}
static SEQ: AtomicU64 = AtomicU64::new(0);
fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cement-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::create_dir_all(p.join(".local")).unwrap();
    std::fs::write(p.join("src/lib.rs"), "pub enum State { Ready, Done }\n").unwrap();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname='cement-demo'\nversion='0.1.0'\n",
    )
    .unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&p)
        .status()
        .unwrap();
    assert!(status.success());
    p
}
fn setup(p: &std::path::Path) -> (Host, Arc<native::CementHost>, String, String) {
    let state = p.join(".local/document.json");
    let cement = Arc::new(native::CementHost::open(p, &state).unwrap());
    let h = Host::open(
        &state,
        Composition::new(vec![
            samepage_map::descriptor(),
            samepage_sources::descriptor(),
            samepage_lanes::descriptor(),
            samepage_review::descriptor(),
            samepage_cement::descriptor(),
        ])
        .unwrap(),
        vec![
            Arc::new(samepage_map::Map),
            Arc::new(samepage_sources::Sources),
            Arc::new(samepage_lanes::Lanes),
            Arc::new(samepage_review::Review),
            Arc::new(Cement),
        ],
    )
    .unwrap()
    .with_verifier("sources", Arc::new(sources::Repository::open(p).unwrap()))
    .unwrap()
    .with_verifier("review", Arc::new(review::ReviewHost::open(p).unwrap()))
    .unwrap()
    .with_verifier("cement", cement.clone())
    .unwrap();
    let a = Actor::new("agent", Caller::Agent);
    let human = Actor::new("human", Caller::Human);
    let b = samepage_sources::place_batch(
        &Call {
            action: "card_place".into(),
            args: json!({"label":"Engine","path":"src/lib.rs","lines":"1"}),
        },
        &h.view(),
    )
    .unwrap();
    let n = h.invoke_composed(&a, "card_place", &b).unwrap().ids["card"].clone();
    h.invoke(
        &a,
        "context_set",
        json!({"question":"What must State preserve?","altitude":"implementation","subjects":[n]}),
    )
    .unwrap();
    let c=h.invoke(&a,"claim_add",json!({"about":n,"text":"Two variants","basis":"verified","path":"src/lib.rs","lines":"1"})).unwrap().ids["claim"].clone();
    h.invoke(
        &human,
        "claim_verdict",
        json!({"id":c,"verdict":"accepted"}),
    )
    .unwrap();
    h.invoke(&human, "card_place", json!({"id":n,"status":"agreed"}))
        .unwrap();
    let d = h
        .invoke(&a, "decision_draft", json!({"draft":input(&n)}))
        .unwrap()
        .ids["draft"]
        .clone();
    (h, cement, n, d)
}
#[test]
fn cement_refuses_a_settled_selection_changed_during_publication() {
    let p = root();
    let (h, c, n, d) = setup(&p);
    let preview = c
        .preview(&h.view(), &json!({"ids":[n],"draft_id":d}), true)
        .unwrap();
    let mut pending = c
        .publish(
            &h.view(),
            &json!({"ids":[n],"draft_id":d,"review_token":preview["bundle"]["review_token"]}),
        )
        .unwrap();
    let human = Actor::new("human", Caller::Human);
    h.invoke(
        &human,
        "card_place",
        json!({"id":n,"note":"changed during publication"}),
    )
    .unwrap();
    let before = h.revision();
    assert!(
        h.invoke_composed(&human, "decision_cement", &pending.batch)
            .is_err()
    );
    pending.rollback().unwrap();
    assert_eq!(h.revision(), before);
    assert!(!p.join("docs/decisions/index.json").exists());
    assert!(!p.join("specs/obligations-v0.1.json").exists());
    std::fs::remove_dir_all(p).unwrap();
}
#[test]
fn reviewed_cement_activates_the_real_g8_gate_and_records_the_adr() {
    let p = root();
    let (h, c, n, d) = setup(&p);
    let agent = Actor::new("agent", Caller::Agent);
    let human = Actor::new("human", Caller::Human);
    assert!(
        h.invoke(&agent, "decision_preview", json!({"ids":[n],"draft_id":d}))
            .is_err()
    );
    h.invoke(&agent, "mode_set", json!({"mode":"alignment"}))
        .unwrap();
    let preview = c
        .preview(&h.view(), &json!({"ids":[n],"draft_id":d}), true)
        .unwrap();
    let args = json!({"ids":[n],"draft_id":d,"review_token":preview["bundle"]["review_token"]});
    let pending = c.publish(&h.view(), &args).unwrap();
    assert_eq!(pending.result["receipt"]["g8_status"], "active_passing");
    assert!(
        h.invoke_composed(&agent, "decision_cement", &pending.batch)
            .is_err()
    );
    let committed = h
        .invoke_composed(&human, "atlas_cement_decision", &pending.batch)
        .unwrap();
    assert_eq!(committed.action, "decision_cement");
    pending.accept();
    assert!(samepage_cement::covers(
        &h.view(),
        h.view().get(&n).unwrap()
    ));
    assert!(p.join("g8.lock").exists());
    assert_eq!(c.history().unwrap().as_array().unwrap().len(), 1);
    assert!(c.gates(&h.view()).unwrap().0[&n].met);
    std::fs::write(
        p.join("src/lib.rs"),
        "pub enum State { Ready, Done, Broken }\n",
    )
    .unwrap();
    let (facts, result) = c.gates(&h.view()).unwrap();
    assert!(!facts[&n].met);
    assert_ne!(result["obligations"][0]["status"], "passed");
    assert!(c.publish(&h.view(), &args).is_err());
    h.save().unwrap();
    assert!(
        native::CementHost::open(&p, &p.join(".local/document.json"))
            .unwrap()
            .history()
            .unwrap()
            .as_array()
            .unwrap()
            .len()
            == 1
    );
    std::fs::remove_dir_all(p).unwrap();
}
#[test]
fn native_adapter_has_no_foreign_script_or_checkout_reference() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pattern = ["python", "3|", "../../", "dev"].concat();
    let result = std::process::Command::new("rg")
        .args(["-n", "--", &pattern])
        .arg(root.join("apps/samepage/src/host/cement.rs"))
        .arg(root.join("crates/samepage-cement/src"))
        .arg(root.join("apps/samepage/modules/cement.js"))
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(1),
        "forbidden adapter reference: {}",
        String::from_utf8_lossy(&result.stdout)
    );
}
#[test]
fn structural_draft_schema_is_closed_and_explicit() {
    let mut value = input("n");
    value["checks"][0]["check"]["backend"] = json!("arbitrary_command");
    assert!(serde_json::from_value::<DraftInput>(value).is_err());
    assert!(
        serde_json::from_value::<DraftInput>(input("n"))
            .unwrap()
            .validate()
            .is_ok()
    );
    let _: BTreeMap<String, String> = BTreeMap::new();
}
#[test]
fn a_real_g8_refusal_rolls_back_files_and_never_stamps_the_document() {
    let p = root();
    let (h, c, n, d) = setup(&p);
    std::fs::create_dir_all(p.join("specs")).unwrap();
    let original = r#"{"obligations":[{"id":"BROKEN","checker":{"mode":"unsupported"}}]}"#;
    std::fs::write(p.join("specs/obligations-v0.1.json"), original).unwrap();
    let preview = c
        .preview(&h.view(), &json!({"ids":[n],"draft_id":d}), true)
        .unwrap();
    let before = (h.revision(), h.encode_full(), h.journal().seq);
    let result = c.publish(
        &h.view(),
        &json!({"ids":[n],"draft_id":d,"review_token":preview["bundle"]["review_token"]}),
    );
    assert!(result.is_err());
    assert_eq!((h.revision(), h.encode_full(), h.journal().seq), before);
    assert_eq!(
        std::fs::read_to_string(p.join("specs/obligations-v0.1.json")).unwrap(),
        original
    );

    assert!(!p.join("docs/decisions/index.json").exists());
    std::fs::remove_dir_all(p).unwrap();
}
