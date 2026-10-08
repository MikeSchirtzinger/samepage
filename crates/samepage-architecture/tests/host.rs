use samepage_architecture::{
    compare, describe, inspect, proposals, Architecture, Graph, Impact, ProposalInput, Relationship,
};
use samepage_capability::{
    Batch, BatchEntry, Call, Capability, Composition, Op, Plan, Target, Value,
};
use samepage_doc::{host::Host, Actor, Caller, Document, Replica};
use samepage_map::Map;
use serde_json::json;
use std::{collections::BTreeSet, sync::Arc};
mod decision_schema;
fn descriptors() -> Vec<samepage_capability::Descriptor> {
    vec![
        samepage_map::descriptor(),
        samepage_architecture::descriptor(),
    ]
}
fn host() -> Host {
    Host::in_memory(
        Composition::new(descriptors()).unwrap(),
        vec![Arc::new(Map), Arc::new(Architecture)],
    )
    .unwrap()
}
fn agent() -> Actor {
    Actor::new("agent-1", Caller::Agent).labelled("agent")
}
fn human() -> Actor {
    Actor::new("human-1", Caller::Human).labelled("human")
}
fn setup() -> (Host, Vec<String>) {
    let h = host();
    let ids: Vec<_> = ["UI", "Host", "Core", "Unrelated"]
        .iter()
        .map(|l| {
            h.invoke(&agent(), "card_place", json!({"label":l}))
                .unwrap()
                .ids["card"]
                .clone()
        })
        .collect();
    for (a, b) in [(0, 1), (1, 2)] {
        h.invoke(
            &agent(),
            "card_link",
            json!({"from":ids[a],"to":ids[b],"label":"depends on"}),
        )
        .unwrap();
    }
    (h, ids)
}
fn input(h: &Host) -> ProposalInput {
    ProposalInput {
        title: "Change host dependency".into(),
        baseline_revision: Graph::capture(&h.view()).revision().unwrap(),
        previous: String::new(),
        remove: vec![],
        add: vec![],
    }
}
fn propose(h: &Host, input: &ProposalInput, who: &Actor) -> String {
    h.invoke(who, "architecture_propose", json!(input))
        .unwrap()
        .ids["proposal"]
        .clone()
}
fn edge(ids: &[String], from: usize, to: usize, label: &str) -> Relationship {
    Relationship {
        from: ids[from].clone(),
        to: ids[to].clone(),
        label: label.into(),
    }
}
fn fingerprint(h: &Host) -> (String, Vec<u8>, u64) {
    (h.revision(), h.encode_full(), h.journal().seq)
}
fn batch(h: &Host, ops: Vec<Op>) -> Batch {
    Batch {
        base_revision: h.revision(),
        entries: vec![BatchEntry {
            capability: "architecture".into(),
            plan: Plan {
                ops,
                readback: String::new(),
            },
        }],
    }
}
fn set(id: &str, key: &str, value: &str) -> Op {
    Op::Set {
        target: Target::Id(id.into()),
        props: [(key.into(), Value::from(value))].into(),
    }
}
fn peer(h: &Host) -> Replica {
    let mut p = Replica::new();
    p.set_composition(descriptors()).unwrap();
    for f in h.hello() {
        p.receive(&f).unwrap();
    }
    p.take_outbound();
    p
}
fn send(h: &Host, p: &mut Replica, who: &Actor) {
    for f in p.take_outbound() {
        h.receive_frame(who, &f).unwrap();
    }
}
fn sync(h: &Host, p: &mut Replica) {
    for f in h.hello() {
        p.receive(&f).unwrap();
    }
}

#[test]
fn proposal_changes_relationships_without_changing_the_current_map_and_reports_upstream_path() {
    let (h, ids) = setup();
    let baseline = Graph::capture(&h.view());
    let map: Vec<_> = h
        .view()
        .records()
        .filter(|r| r.kind == "node" || r.kind == "edge")
        .cloned()
        .collect();
    let mut request = input(&h);
    request.remove.push(edge(&ids, 1, 2, "depends on"));
    propose(&h, &request, &human());
    let view = h.view();
    let p = proposals(&view).unwrap();
    assert_eq!(Graph::capture(&view), baseline);
    assert_eq!(
        view.records()
            .filter(|r| r.kind == "node" || r.kind == "edge")
            .cloned()
            .collect::<Vec<_>>(),
        map
    );
    let diff = compare(&p[0], &baseline).unwrap();
    assert_eq!(diff.removed, request.remove);
    assert!(diff.added.is_empty());
    assert_eq!(
        diff.direct.iter().collect::<BTreeSet<_>>(),
        [&ids[1], &ids[2]].into_iter().collect()
    );
    assert_eq!(
        diff.upstream,
        vec![Impact {
            id: ids[0].clone(),
            via: vec![ids[0].clone(), ids[1].clone()]
        }]
    );
    assert!(!diff.direct.contains(&ids[3]));
    assert!(!diff.baseline_changed);
    assert_eq!(p[0].created_by, "human");
    let restored = Document::from_state(&h.encode_full()).unwrap();
    assert_eq!(inspect(&restored.view()).unwrap(), inspect(&view).unwrap());
    assert!(describe(&view).unwrap().contains("UPSTREAM"));
}
#[test]
fn no_change_and_layout_changes_have_no_impact_but_semantic_edits_stale_the_baseline() {
    let (h, ids) = setup();
    let request = input(&h);
    propose(&h, &request, &agent());
    h.invoke(&human(), "card_move", json!({"id":ids[0],"x":500,"y":150}))
        .unwrap();
    h.invoke(
        &human(),
        "card_resize",
        json!({"id":ids[0],"w":400,"h_pref":150}),
    )
    .unwrap();
    let p = proposals(&h.view()).unwrap();
    let diff = compare(&p[0], &Graph::capture(&h.view())).unwrap();
    assert!(!diff.baseline_changed);
    assert!(diff.direct.is_empty() && diff.upstream.is_empty());
    h.invoke(
        &human(),
        "card_place",
        json!({"id":ids[0],"label":"Changed UI"}),
    )
    .unwrap();
    assert!(
        inspect(&h.view()).unwrap()["comparisons"][0]["baseline_changed"]
            .as_bool()
            .unwrap()
    );
    assert!(describe(&h.view())
        .unwrap()
        .contains("captured baseline is stale"));
}
#[test]
fn invalid_and_stale_writes_are_atomic_and_revisions_keep_alternatives() {
    let (h, ids) = setup();
    let request = input(&h);
    let first = propose(&h, &request, &agent());
    let before = fingerprint(&h);
    let mut bad = request.clone();
    bad.baseline_revision = "stale".into();
    assert!(h
        .invoke(&human(), "architecture_propose", json!(bad))
        .is_err());
    bad = request.clone();
    bad.add.push(Relationship {
        from: ids[0].clone(),
        to: "missing".into(),
        label: "calls".into(),
    });
    assert!(h
        .invoke(&human(), "architecture_propose", json!(bad))
        .is_err());
    assert_eq!(fingerprint(&h), before);
    let mut change = request.clone();
    change.previous = first.clone();
    change.add.push(edge(&ids, 2, 1, "calls"));
    let second = propose(&h, &change, &human());
    let third = propose(&h, &change, &agent());
    assert_ne!(second, third);
    let p = proposals(&h.view()).unwrap();
    assert_eq!(p.len(), 3);
    for p in p.iter().filter(|p| p.id != first) {
        let d = compare(p, &Graph::capture(&h.view())).unwrap();
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.upstream.len(), 1);
    }
    change.previous = second;
    assert!(h
        .invoke(&human(), "architecture_propose", json!(change))
        .unwrap_err()
        .to_string()
        .contains("already exists"));
}
#[test]
fn concurrent_peer_proposals_merge_and_each_reader_sees_the_other_authors_revision() {
    let (h, _) = setup();
    let baseline = Architecture.digest(&h.view()).unwrap();
    let mut left = peer(&h);
    let mut right = peer(&h);
    let request = input(&h);
    let call = Call {
        action: "architecture_propose".into(),
        args: json!(request),
    };
    let a = left
        .apply_batch(&Architecture.plan_batch(&call, &left.view()).unwrap())
        .unwrap()
        .ids["proposal"]
        .clone();
    let b = right
        .apply_batch(&Architecture.plan_batch(&call, &right.view()).unwrap())
        .unwrap()
        .ids["proposal"]
        .clone();
    send(&h, &mut left, &human());
    send(&h, &mut right, &agent());
    sync(&h, &mut left);
    sync(&h, &mut right);
    assert_eq!(proposals(&h.view()).unwrap().len(), 2);
    assert_eq!(
        inspect(&left.view()).unwrap(),
        inspect(&right.view()).unwrap()
    );
    assert_eq!(inspect(&left.view()).unwrap(), inspect(&h.view()).unwrap());
    let human = Architecture
        .changes(&baseline, &Architecture.digest(&h.view()).unwrap(), "human")
        .unwrap()
        .join("\n");
    let agent = Architecture
        .changes(&baseline, &Architecture.digest(&h.view()).unwrap(), "agent")
        .unwrap()
        .join("\n");
    assert!(human.contains(&b) && !human.contains(&a));
    assert!(agent.contains(&a) && !agent.contains(&b));
}
#[test]
fn decision_draft_readback_has_canonical_order_across_restored_peers() {
    let (h, ids) = setup();
    let definition=json!({"id":"","context_ids":[],"content":"unreviewed","created_by":"agent",
        "input":{"ids":[ids[0]],"title":"Review dependency","decision":"Keep the dependency explicit","rationale":"Keep both peers aligned","tradeoffs":"Changes need review","consequences":"Check the source","alternatives":[{"option":"Remove it","reason_not_chosen":"Needs further review"}],"checks":[{"id":"STATE-01","requirement":"Keep known states","check":{"backend":"rust_enum_shape","args":{"file":"lib.rs","enum_name":"State","expected_variants":["Ready"],"expected_serde_rename_all":null}},"evidence_files":["lib.rs"]}],"not_enforced":[]}}).to_string();
    let d = Document::from_state(&h.encode_full()).unwrap();
    use yrs::{Map, Transact};
    for i in (0..8).rev() {
        let mut tx = d.doc().transact_mut();
        let r = d
            .objects_ref()
            .insert(&mut tx, format!("draft-{i}"), yrs::MapPrelim::default());
        r.insert(&mut tx, "kind", "decision-draft");
        r.insert(&mut tx, "definition", definition.clone());
        r.insert(&mut tx, "created_by", "agent");
    }
    let read = |v: samepage_capability::View| {
        v.records()
            .filter(|r| r.kind == "decision-draft")
            .map(|r| {
                let mut draft: decision_schema::Draft =
                    serde_json::from_str(samepage_architecture::string(r, "definition")).unwrap();
                draft.id = r.id.clone();
                draft.created_by = samepage_architecture::string(r, "created_by").into();
                draft
            })
            .collect::<Vec<_>>()
    };
    let drafts = read(d.view());
    assert!(drafts.windows(2).all(|p| p[0].id < p[1].id));
    let peer = Document::from_state(&d.encode_full()).unwrap();
    assert_eq!(drafts, read(peer.view()));
}
#[test]
fn human_verdict_is_enforced_on_actions_batches_frames_and_same_value_attempts() {
    let (h, _) = setup();
    let id = propose(&h, &input(&h), &agent());
    let before = fingerprint(&h);
    assert!(h
        .invoke(
            &agent(),
            "architecture_decide",
            json!({"id":id,"verdict":"accepted"})
        )
        .is_err());
    let verdict = batch(&h, vec![set(&id, "verdict", "accepted")]);
    assert!(h
        .commit_batch(&agent(), &verdict)
        .unwrap_err()
        .to_string()
        .contains("architecture_human_only"));
    assert_eq!(before, fingerprint(&h));
    let mut p = peer(&h);
    p.apply_batch(&verdict).unwrap();
    let mut refused = false;
    for f in p.take_outbound() {
        if h.receive_frame(&agent(), &f).is_err() {
            refused = true;
        }
    }
    assert!(refused);
    assert_eq!(before, fingerprint(&h));
    let graph = Graph::capture(&h.view());
    h.invoke(
        &human(),
        "architecture_decide",
        json!({"id":id,"verdict":"accepted"}),
    )
    .unwrap();
    assert_eq!(Graph::capture(&h.view()), graph);
    assert_eq!(proposals(&h.view()).unwrap()[0].decided_by, "human");
    let before = fingerprint(&h);
    assert!(h
        .commit_batch(&agent(), &batch(&h, vec![set(&id, "verdict", "accepted")]))
        .is_err());
    assert_eq!(before, fingerprint(&h));
    h.invoke(
        &human(),
        "architecture_decide",
        json!({"id":id,"verdict":"rejected"}),
    )
    .unwrap();
    assert_eq!(proposals(&h.view()).unwrap()[0].verdict, "rejected");
}
#[test]
fn proposal_history_cannot_be_rewritten_deleted_or_forged() {
    let (h, _) = setup();
    let id = propose(&h, &input(&h), &agent());
    let view = h.view();
    let before = fingerprint(&h);
    for who in [human(), agent()] {
        assert!(h
            .commit_batch(
                &who,
                &batch(
                    &h,
                    vec![set(
                        &id,
                        "definition",
                        samepage_architecture::string(view.get(&id).unwrap(), "definition")
                    )]
                )
            )
            .is_err());
        assert!(h
            .commit_batch(
                &who,
                &batch(
                    &h,
                    vec![Op::Delete {
                        target: Target::Id(id.clone())
                    }]
                )
            )
            .is_err());
    }
    let mut p = Architecture
        .plan(
            &Call {
                action: "architecture_propose".into(),
                args: json!(input(&h)),
            },
            &h.view(),
        )
        .unwrap();
    if let Op::Create { props, .. } = &mut p.ops[0] {
        props.insert("verdict".into(), "accepted".into());
    }
    assert!(h.commit_batch(&human(), &batch(&h, p.ops)).is_err());
    assert_eq!(before, fingerprint(&h));
}
#[test]
fn aliases_share_the_preferred_action_and_journal_name() {
    let (h, _) = setup();
    let id = h
        .invoke(&agent(), "atlas_architecture_propose", json!(input(&h)))
        .unwrap();
    assert_eq!(id.action, "architecture_propose");
    assert_eq!(
        h.journal().recent.back().unwrap().action.as_deref(),
        Some("architecture_propose")
    );
}
#[test]
fn proposed_snapshots_survive_map_deletion_and_semantic_links_stale_them() {
    let (h, ids) = setup();
    propose(&h, &input(&h), &agent());
    h.invoke(
        &agent(),
        "card_link",
        json!({"from":ids[0],"to":ids[2],"label":"calls"}),
    )
    .unwrap();
    assert_eq!(
        inspect(&h.view()).unwrap()["comparisons"][0]["baseline_changed"],
        true
    );
    h.invoke(&agent(), "object_remove", json!({"id":ids[2]}))
        .unwrap();
    assert_eq!(
        proposals(&h.view()).unwrap()[0]
            .definition
            .baseline
            .components
            .len(),
        4
    );
    assert!(describe(&h.view()).unwrap().contains("stale"));
}
#[test]
fn typed_transition_labels_are_semantic_but_geometry_and_author_stamps_are_not() {
    use samepage_capability::{Record, View};
    let v = View::new(
        "a",
        vec![
            Record::new("a", "node").with("label", "A"),
            Record::new("b", "node").with("label", "B"),
            Record::new("e", "edge")
                .with("from", "a")
                .with("to", "b")
                .with("event", "Send")
                .with("guard", "ready"),
        ],
    )
    .unwrap();
    assert_eq!(Graph::capture(&v).relationships[0].label, "Send [ready]");
    let other = View::new(
        "b",
        v.records()
            .cloned()
            .map(|r| r.with("x", 600.0).with("created_by", "different")),
    )
    .unwrap();
    assert_eq!(
        Graph::capture(&v).revision().unwrap(),
        Graph::capture(&other).revision().unwrap()
    );
}
#[test]
fn restored_verdicts_and_alternatives_keep_host_authors() {
    let (h, _) = setup();
    let first = propose(&h, &input(&h), &agent());
    let mut next = input(&h);
    next.previous = first.clone();
    propose(&h, &next, &human());
    h.invoke(
        &human(),
        "architecture_decide",
        json!({"id":first,"verdict":"accepted"}),
    )
    .unwrap();
    let d = Document::from_state(&h.encode_full()).unwrap();
    assert_eq!(inspect(&d.view()).unwrap(), inspect(&h.view()).unwrap());
    assert_eq!(describe(&d.view()).unwrap(), describe(&h.view()).unwrap());
    assert_eq!(proposals(&d.view()).unwrap().len(), 2);
}
