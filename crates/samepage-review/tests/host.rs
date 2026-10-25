use samepage_capability::{
    Batch, BatchEntry, Call, Capability, Composition, Op, Plan, Target, Value,
};
use samepage_doc::{Actor, Caller, Replica, host::Host};
use samepage_map::Map;
use samepage_review::{Review, context};
use serde_json::{Value as JsonValue, json};
use std::sync::Arc;
#[path = "../../../apps/samepage/src/diagram_sources.rs"]
mod diagram_sources;
#[path = "../../../apps/samepage/src/host/review.rs"]
mod native_review;
fn descriptors() -> Vec<samepage_capability::Descriptor> {
    vec![samepage_map::descriptor(), samepage_review::descriptor()]
}
fn host() -> Host {
    Host::in_memory(
        Composition::new(descriptors()).unwrap(),
        vec![Arc::new(Map), Arc::new(Review)],
    )
    .unwrap()
}
fn agent() -> Actor {
    Actor::new("agent-1", Caller::Agent).labelled("Agent")
}
fn human() -> Actor {
    Actor::new("human-1", Caller::Human).labelled("Human")
}
fn node(h: &Host) -> String {
    h.invoke(&agent(), "card_place", json!({"label":"Persistence"}))
        .unwrap()
        .ids["card"]
        .clone()
}
fn claim(h: &Host, n: &str) -> String {
    h.invoke(
        &agent(),
        "claim_add",
        json!({"about":n,"text":"Writes before acknowledging","basis":"inferred"}),
    )
    .unwrap()
    .ids["claim"]
        .clone()
}
fn verdict(h: &Host, c: &str, v: &str) {
    h.invoke(&human(), "claim_verdict", json!({"id":c,"verdict":v}))
        .unwrap();
}
fn set(id: &str, key: &str, value: impl Into<Value>) -> Op {
    Op::Set {
        target: Target::Id(id.into()),
        props: [(key.into(), value.into())].into(),
    }
}
fn batch(h: &Host, cap: &str, ops: Vec<Op>) -> Batch {
    Batch {
        base_revision: h.revision(),
        entries: vec![BatchEntry {
            capability: cap.into(),
            plan: Plan {
                ops,
                readback: String::new(),
            },
        }],
    }
}
fn fingerprint(h: &Host) -> (String, Vec<u8>, u64) {
    (h.revision(), h.encode_full(), h.journal().seq)
}
fn ctx(question: &str, replaces: Vec<String>) -> JsonValue {
    json!({"question":question,"altitude":"system","replaces":replaces})
}
fn peer(h: &Host) -> Replica {
    let mut p = Replica::new();
    p.set_composition(descriptors()).unwrap();
    for frame in h.hello() {
        p.receive(&frame).unwrap();
    }
    p
}
fn send(h: &Host, p: &mut Replica, who: &Actor) {
    for frame in p.take_outbound() {
        h.receive_frame(who, &frame).unwrap();
    }
}

#[test]
fn only_the_agent_creates_and_only_the_human_gives_a_verdict() {
    let h = host();
    let n = node(&h);
    let args = json!({"about":n,"text":"Knows","basis":"inferred"});
    let before = fingerprint(&h);
    assert!(h.invoke(&human(), "claim_add", args.clone()).is_err());
    assert_eq!(before, fingerprint(&h));
    let planned = Review
        .plan(
            &Call {
                action: "claim_add".into(),
                args,
            },
            &h.view(),
        )
        .unwrap();
    assert!(
        h.commit_batch(&human(), &batch(&h, "review", planned.ops))
            .is_err()
    );
    let c = claim(&h, &n);
    let before = fingerprint(&h);
    assert!(
        h.invoke(
            &agent(),
            "claim_verdict",
            json!({"id":c,"verdict":"accepted"})
        )
        .is_err()
    );
    assert!(
        h.commit_batch(
            &agent(),
            &batch(&h, "review", vec![set(&c, "verdict", "accepted")])
        )
        .is_err()
    );
    assert_eq!(before, fingerprint(&h));
    verdict(&h, &c, "accepted");
    assert!(
        h.invoke(&human(), "claim_withdraw", json!({"id":c}))
            .is_err()
    );
    // Even an attempted same-value write cannot impersonate a human verdict.
    assert!(
        h.commit_batch(
            &agent(),
            &batch(&h, "review", vec![set(&c, "verdict", "accepted")])
        )
        .is_err()
    );
}
#[test]
fn revising_an_accepted_claim_reopens_it() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    verdict(&h, &c, "accepted");
    assert_eq!(samepage_review::standing(&h.view(), &n), "SETTLED");
    let before = fingerprint(&h);
    assert!(
        h.commit_batch(
            &agent(),
            &batch(&h, "review", vec![set(&c, "text", "Changed words")])
        )
        .is_err()
    );
    assert_eq!(before, fingerprint(&h));
    h.invoke(
        &agent(),
        "claim_add",
        json!({"id":c,"text":"Writes before acknowledging, then broadcasts","basis":"inferred"}),
    )
    .unwrap();
    assert_eq!(
        samepage_review::string(h.view().get(&c).unwrap(), "verdict"),
        "open"
    );
    assert_eq!(samepage_review::standing(&h.view(), &n), "UNREVIEWED");
}
#[test]
fn agreed_is_refused_while_under_challenge() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    let before = fingerprint(&h);
    let error = h
        .invoke(&human(), "card_place", json!({"id":n,"status":"agreed"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("unreviewed or disputed") && error.contains(&c));
    assert_eq!(before, fingerprint(&h));
    verdict(&h, &c, "accepted");
    h.invoke(&human(), "card_place", json!({"id":n,"status":"agreed"}))
        .unwrap();
    verdict(&h, &c, "rejected");
    assert!(
        h.commit_batch(
            &human(),
            &batch(&h, "map", vec![set(&n, "status", "agreed")])
        )
        .is_err()
    );
}
#[test]
fn removing_a_node_removes_its_claims_and_their_marks() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    let m = h
        .invoke(
            &human(),
            "mark_add",
            json!({"target":c,"glyph":"?","text":"Where?"}),
        )
        .unwrap()
        .ids["mark"]
        .clone();
    h.invoke(
        &human(),
        "mark_add",
        json!({"target":n,"glyph":"!","text":"Check"}),
    )
    .unwrap();
    let seq = h.journal().seq;
    let result = h
        .invoke(&agent(), "object_remove", json!({"id":n}))
        .unwrap();
    assert_eq!(h.journal().seq, seq + 1);
    assert_eq!(result.commit.unwrap().deleted.len(), 4);
    assert!(!h.view().contains(&m));
    assert_eq!(h.view().records().count(), 0);
}
#[test]
fn a_context_change_retains_the_previous_question_and_refuses_a_stale_writer() {
    let h = host();
    let first = h
        .invoke(&human(), "context_set", ctx("Why share state?", vec![]))
        .unwrap()
        .ids["context"]
        .clone();
    let request = ctx("How does reconnect work?", vec![first.clone()]);
    let second = h
        .invoke(&human(), "context_set", request.clone())
        .unwrap()
        .ids["context"]
        .clone();
    let before = fingerprint(&h);
    assert!(h.invoke(&human(), "context_set", request).is_err());
    assert_eq!(before, fingerprint(&h));
    let state = context::project(&h.view()).unwrap();
    assert_eq!(state.heads, vec![second]);
    assert!(state.entries.iter().any(|e| e.id == first));
    assert!(
        h.commit_batch(
            &human(),
            &batch(
                &h,
                "review",
                vec![set(
                    &first,
                    "definition",
                    ctx("Rewrite history", vec![]).to_string()
                )]
            )
        )
        .is_err()
    );
}
#[test]
fn unrelated_context_heads_are_visible_until_explicitly_reconciled() {
    let h = host();
    let mut a = peer(&h);
    let mut b = peer(&h);
    let pa = Review
        .plan_batch(
            &Call {
                action: "context_set".into(),
                args: ctx("Why?", vec![]),
            },
            &a.view(),
        )
        .unwrap();
    let pb = Review
        .plan_batch(
            &Call {
                action: "context_set".into(),
                args: ctx("How?", vec![]),
            },
            &b.view(),
        )
        .unwrap();
    a.apply_batch(&pa).unwrap();
    b.apply_batch(&pb).unwrap();
    send(&h, &mut a, &human());
    send(&h, &mut b, &agent());
    let state = context::project(&h.view()).unwrap();
    assert!(state.current.is_none());
    assert!(state.describe().contains("RECONCILIATION"));
    assert_eq!(state.heads.len(), 2);
    h.invoke(
        &human(),
        "context_set",
        ctx("What is our boundary?", state.heads),
    )
    .unwrap();
    assert!(context::project(&h.view()).unwrap().current.is_some());
}
#[test]
fn an_unknown_scope_target_cannot_partially_change_the_page() {
    let h = host();
    let before = fingerprint(&h);
    assert!(
        h.invoke(
            &human(),
            "context_set",
            json!({"question":"Which component?","altitude":"component","subjects":["missing"]})
        )
        .is_err()
    );
    assert_eq!(before, fingerprint(&h));
}
#[test]
fn real_crdt_peers_preserve_concurrent_scopes_and_question_history() {
    let h = host();
    let n = node(&h);
    let first = h
        .invoke(&human(), "context_set", ctx("Why?", vec![]))
        .unwrap()
        .ids["context"]
        .clone();
    let mut left = peer(&h);
    let mut right = peer(&h);
    let a = left
        .apply_batch(
            &Review
                .plan_batch(
                    &Call {
                        action: "context_set".into(),
                        args: ctx("How do peers sync?", vec![first.clone()]),
                    },
                    &left.view(),
                )
                .unwrap(),
        )
        .unwrap();
    let a_id = a.ids["context"].clone();
    left.apply_batch(
        &Review
            .plan_batch(
                &Call {
                    action: "mark_add".into(),
                    args: json!({"target":n,"glyph":"?","text":"Offline?"}),
                },
                &left.view(),
            )
            .unwrap(),
    )
    .unwrap();
    right
        .apply_batch(
            &Review
                .plan_batch(
                    &Call {
                        action: "context_set".into(),
                        args: ctx("Which protocol?", vec![first]),
                    },
                    &right.view(),
                )
                .unwrap(),
        )
        .unwrap();
    send(&h, &mut left, &human());
    send(&h, &mut right, &agent());
    for frame in h.hello() {
        left.receive(&frame).unwrap();
        right.receive(&frame).unwrap();
    }
    assert_eq!(
        context::project(&left.view()).unwrap(),
        context::project(&right.view()).unwrap()
    );
    let state = context::project(&h.view()).unwrap();
    assert_eq!(state.heads.len(), 2);
    h.invoke(
        &human(),
        "context_set",
        ctx("What is the sync boundary?", state.heads),
    )
    .unwrap();
    assert!(
        h.view()
            .records()
            .filter(|r| r.kind == "mark")
            .all(|r| samepage_review::string(r, "context_ids") == a_id)
    );
    assert!(
        samepage_review::describe(&h.view())
            .unwrap()
            .contains("asked at System scope: How do peers sync?")
    );
}
#[test]
fn forged_replica_verdict_and_human_claim_are_refused_without_mutation() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    let mut p = peer(&h);
    p.apply(
        "review",
        Plan {
            ops: vec![set(&c, "verdict", "accepted")],
            readback: String::new(),
        },
    )
    .unwrap();
    let before = fingerprint(&h);
    for frame in p.take_outbound() {
        assert!(h.receive_frame(&agent(), &frame).is_err());
    }
    assert_eq!(before, fingerprint(&h));
    let mut p = peer(&h);
    let plan = Review
        .plan(
            &Call {
                action: "claim_add".into(),
                args: json!({"about":n,"text":"Forged","basis":"inferred"}),
            },
            &p.view(),
        )
        .unwrap();
    p.apply("review", plan).unwrap();
    for frame in p.take_outbound() {
        assert!(h.receive_frame(&human(), &frame).is_err());
    }
    assert_eq!(before, fingerprint(&h));
}
#[test]
fn only_humans_author_marks_but_both_participants_can_answer() {
    let h = host();
    let n = node(&h);
    assert!(
        h.invoke(&agent(), "mark_add", json!({"target":n,"glyph":"?"}))
            .is_err()
    );
    let m = h
        .invoke(
            &human(),
            "mark_add",
            json!({"target":n,"glyph":"?","text":"Why?"}),
        )
        .unwrap()
        .ids["mark"]
        .clone();
    assert!(
        h.commit_batch(
            &agent(),
            &batch(&h, "review", vec![set(&m, "text", "Forged question")])
        )
        .is_err()
    );
    h.invoke(
        &agent(),
        "mark_answer",
        json!({"id":m,"answer":"Durable before visible"}),
    )
    .unwrap();
    h.invoke(
        &human(),
        "mark_answer",
        json!({"id":m,"answer":"Understood"}),
    )
    .unwrap();
    let view = h.view();
    let mark = view.get(&m).unwrap();
    assert_eq!(samepage_review::string(mark, "created_by"), "Human");
    assert_eq!(samepage_review::string(mark, "answer"), "Understood");
}
#[test]
fn unknown_basis_and_withdrawal_cannot_invent_settlement() {
    let h = host();
    let n = node(&h);
    let c = h
        .invoke(
            &agent(),
            "claim_add",
            json!({"about":n,"text":"Unknown behavior","basis":"unknown"}),
        )
        .unwrap()
        .ids["claim"]
        .clone();
    assert!(
        h.invoke(
            &human(),
            "claim_verdict",
            json!({"id":c,"verdict":"accepted"})
        )
        .is_err()
    );
    h.invoke(&agent(), "claim_withdraw", json!({"id":c}))
        .unwrap();
    assert_eq!(samepage_review::standing(&h.view(), &n), "NO CLAIMS");
    assert!(
        h.invoke(
            &agent(),
            "claim_add",
            json!({"id":c,"text":"Now known","basis":"inferred"})
        )
        .is_err()
    );
    assert!(
        h.commit_batch(
            &agent(),
            &batch(&h, "review", vec![set(&c, "withdrawn", false)])
        )
        .is_err()
    );
}
#[test]
fn the_second_read_says_what_the_humans_edits_now_mean() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    let native =
        native_review::ReviewHost::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let first = native.read(&h.view(), "reader").unwrap();
    assert!(
        !first["readback"]
            .as_str()
            .unwrap()
            .contains("CHANGED SINCE")
    );
    verdict(&h, &c, "rejected");
    h.invoke(&human(), "card_place", json!({"id":n,"status":"disputed"}))
        .unwrap();
    let second = native.read(&h.view(), "reader").unwrap();
    let text = second["readback"].as_str().unwrap();
    assert!(
        text.contains("CHANGED SINCE")
            && text.contains("DISPUTED")
            && text.contains("is now disputed (it was open)")
    );
    assert!(
        native.read(&h.view(), "reader").unwrap()["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn a_plain_describe_does_not_advance_the_read_mark() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    let native =
        native_review::ReviewHost::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    native.read(&h.view(), "reader").unwrap();
    verdict(&h, &c, "accepted");
    native.inspect(&h.view()).unwrap();
    samepage_review::describe(&h.view()).unwrap();
    assert!(
        !native.read(&h.view(), "reader").unwrap()["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        native.read(&h.view(), "other-reader").unwrap()["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn identical_restatement_does_not_reopen_and_malformed_scalars_cannot_fake_a_revision() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    verdict(&h, &c, "accepted");
    let before = fingerprint(&h);
    let result = h
        .invoke(
            &agent(),
            "claim_add",
            json!({"id":c,"text":"Writes before acknowledging","basis":"inferred"}),
        )
        .unwrap();
    assert!(result.commit.is_none());
    assert_eq!(before, fingerprint(&h));
    for key in ["path", "lines", "revision"] {
        assert!(
            h.commit_batch(
                &agent(),
                &batch(
                    &h,
                    "review",
                    vec![set(&c, key, false), set(&c, "verdict", "open")]
                )
            )
            .is_err()
        );
    }
    // A raw CRDT property deletion is also an attempted write, not a revision
    // of the content the human actually accepted.
    use yrs::updates::decoder::Decode;
    use yrs::{Map as _, ReadTxn, StateVector, Transact};
    let doc = yrs::Doc::new();
    let objects = doc.get_or_insert_map("objects");
    doc.transact_mut()
        .apply_update(yrs::Update::decode_v1(&h.encode_full()).unwrap())
        .unwrap();
    let record = match objects.get(&doc.transact(), &c).unwrap() {
        yrs::Out::YMap(m) => m,
        _ => panic!("record"),
    };
    {
        let mut txn = doc.transact_mut();
        record.remove(&mut txn, "path");
        record.insert(&mut txn, "verdict", "open");
    }
    let raw = doc
        .transact()
        .encode_state_as_update_v1(&StateVector::default());
    assert!(
        h.receive_frame(&agent(), &samepage_doc::sync::update(raw))
            .is_err()
    );
    assert_eq!(before, fingerprint(&h));
}
#[test]
fn node_deletion_cannot_retarget_a_human_question_to_unrelated_new_content() {
    let h = host();
    let n = node(&h);
    let m = h
        .invoke(&human(), "mark_add", json!({"target":n,"glyph":"?"}))
        .unwrap()
        .ids["mark"]
        .clone();
    let before = fingerprint(&h);
    let b = Batch {
        base_revision: h.revision(),
        entries: vec![
            BatchEntry {
                capability: "map".into(),
                plan: Plan {
                    ops: vec![Op::Create {
                        reference: "replacement".into(),
                        kind: "node".into(),
                        props: [("label".into(), "Unrelated".into())].into(),
                    }],
                    readback: String::new(),
                },
            },
            BatchEntry {
                capability: "review".into(),
                plan: samepage_review::retarget_marks(
                    &h.view(),
                    &n,
                    Target::Ref("replacement".into()),
                ),
            },
            BatchEntry {
                capability: "map".into(),
                plan: Plan {
                    ops: vec![Op::Delete {
                        target: Target::Id(n),
                    }],
                    readback: String::new(),
                },
            },
        ],
    };
    assert!(h.commit_batch(&agent(), &b).is_err());
    assert_eq!(before, fingerprint(&h));
    assert!(h.view().contains(&m));
}
#[test]
fn offline_mark_keeps_its_question_context_when_the_other_scope_arrives_first() {
    let h = host();
    let n = node(&h);
    let first = h
        .invoke(&human(), "context_set", ctx("Initial", vec![]))
        .unwrap()
        .ids["context"]
        .clone();
    let mut left = peer(&h);
    let mut right = peer(&h);
    let context = left
        .apply_batch(
            &Review
                .plan_batch(
                    &Call {
                        action: "context_set".into(),
                        args: ctx("Offline question", vec![first.clone()]),
                    },
                    &left.view(),
                )
                .unwrap(),
        )
        .unwrap()
        .ids["context"]
        .clone();
    left.apply_batch(
        &Review
            .plan_batch(
                &Call {
                    action: "mark_add".into(),
                    args: json!({"target":n,"glyph":"?","text":"Which scope?"}),
                },
                &left.view(),
            )
            .unwrap(),
    )
    .unwrap();
    right
        .apply_batch(
            &Review
                .plan_batch(
                    &Call {
                        action: "context_set".into(),
                        args: ctx("Other question", vec![first]),
                    },
                    &right.view(),
                )
                .unwrap(),
        )
        .unwrap();
    send(&h, &mut right, &agent());
    // One full real CRDT frame contains both offline context and its mark.
    h.receive_frame(
        &human(),
        &samepage_doc::sync::update(left.document().encode_full()),
    )
    .unwrap();
    assert_eq!(context::project(&h.view()).unwrap().heads.len(), 2);
    assert!(
        h.view()
            .records()
            .filter(|r| r.kind == "mark")
            .all(|r| samepage_review::string(r, "context_ids") == context)
    );
}
fn temporary(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "review-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}
#[test]
fn source_deletion_is_visible_and_readability_is_not_claim_verification() {
    let root = temporary("source");
    std::fs::write(root.join("lib.rs"), "pub struct Actual;\n").unwrap();
    let native = Arc::new(native_review::ReviewHost::open(&root).unwrap());
    let h = host().with_verifier("review", native.clone()).unwrap();
    let n = node(&h);
    let before = fingerprint(&h);
    assert!(h.invoke(&agent(),"claim_add",json!({"about":n,"text":"Unsupported range","basis":"verified","path":"lib.rs","lines":"40"})).is_err());
    assert_eq!(before, fingerprint(&h));
    h.invoke(
        &agent(),
        "claim_add",
        json!({"about":n,"text":"Unproven prose","basis":"verified","path":"lib.rs","lines":"1"}),
    )
    .unwrap();
    let report = native.inspect(&h.view()).unwrap();
    assert_eq!(report["nodes"][0]["evidence"][0]["state"], "readable");
    assert!(
        report["evidence_boundary"]
            .as_str()
            .unwrap()
            .contains("does not verify")
    );
    std::fs::remove_file(root.join("lib.rs")).unwrap();
    assert_eq!(
        native.inspect(&h.view()).unwrap()["nodes"][0]["evidence"][0]["state"],
        "unreadable"
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn pinned_source_resolves_inside_a_git_subdirectory_project() {
    use sha2::{Digest, Sha256};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/samepage")
        .canonicalize()
        .unwrap();
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let revision = String::from_utf8(output.stdout).unwrap();
    let native = Arc::new(native_review::ReviewHost::open(&root).unwrap());
    let h = host().with_verifier("review", native.clone()).unwrap();
    let n = node(&h);
    h.invoke(&agent(),"claim_add",json!({"about":n,"text":"Package metadata","basis":"verified","path":"Cargo.toml","lines":"1-3","revision":revision.trim()})).unwrap();
    let expected = "[package]\nname = \"samepage-app\"\nversion.workspace = true\n";
    let report = native.inspect(&h.view()).unwrap();
    assert_eq!(
        report["nodes"][0]["evidence"][0]["sha256"],
        format!("{:x}", Sha256::digest(expected.as_bytes()))
    );
}
#[test]
fn missing_optional_review_refuses_destructive_orphaning_but_allows_map_edits() {
    let root = temporary("omitted");
    let file = root.join("document.json");
    let h = Host::open(
        &file,
        Composition::new(descriptors()).unwrap(),
        vec![Arc::new(Map), Arc::new(Review)],
    )
    .unwrap();
    let n = node(&h);
    claim(&h, &n);
    h.save().unwrap();
    drop(h);
    let h = Host::open(
        &file,
        Composition::new(vec![samepage_map::descriptor()]).unwrap(),
        vec![Arc::new(Map)],
    )
    .unwrap()
    .with_verifier("map", Arc::new(native_review::ReviewUnavailable))
    .unwrap();
    h.invoke(&human(), "card_move", json!({"id":n,"x":500,"y":20}))
        .unwrap();
    let before = fingerprint(&h);
    assert!(
        h.invoke(&human(), "object_remove", json!({"id":n}))
            .unwrap_err()
            .to_string()
            .contains("review_unavailable")
    );
    assert_eq!(before, fingerprint(&h));
    drop(h);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn standing_separates_unreviewed_questioned_and_disputed() {
    let h = host();
    let n = node(&h);
    let c = claim(&h, &n);
    assert_eq!(samepage_review::standing(&h.view(), &n), "UNREVIEWED");
    let mark = h
        .invoke(
            &human(),
            "mark_add",
            json!({"target":n,"glyph":"?","text":"Which port?"}),
        )
        .unwrap()
        .ids["mark"]
        .clone();
    assert_eq!(samepage_review::standing(&h.view(), &n), "QUESTIONED");
    h.invoke(&agent(), "mark_answer", json!({"id":mark,"answer":"8080"}))
        .unwrap();
    assert_eq!(samepage_review::standing(&h.view(), &n), "UNREVIEWED");
    verdict(&h, &c, "rejected");
    assert_eq!(samepage_review::standing(&h.view(), &n), "DISPUTED");
}
