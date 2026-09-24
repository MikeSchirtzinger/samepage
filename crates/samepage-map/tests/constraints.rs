//! Constraints: membership as records, the frozen legacy scalar, and the
//! differential rules that refuse what a change introduces while leaving what
//! it inherits alone.

use std::sync::Arc;

use samepage_capability::{
    Batch, BatchEntry, Capability, Composition, Op, Plan, Scalar, Target, Value,
};
use samepage_doc::host::{CommitError, FrameRefusal, Host, InvokeError};
use samepage_doc::{sync, Actor, Caller};
use samepage_map::rules::*;
use samepage_map::Map;
use serde_json::{json, Value as JsonValue};
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

fn place(host: &Host, label: &str) -> String {
    host.invoke(&agent(), "card_place", json!({ "label": label }))
        .unwrap()
        .ids["card"]
        .clone()
}

fn project(host: &Host) -> JsonValue {
    Map.project(&host.view()).unwrap()
}

fn constraint<'a>(projection: &'a JsonValue, id: &str) -> &'a JsonValue {
    projection["constraints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
}

fn targets(projection: &JsonValue, id: &str) -> Vec<String> {
    constraint(projection, id)["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["target"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[derive(Debug, PartialEq)]
struct Fingerprint(String, Vec<u8>, u64);

fn fingerprint(host: &Host) -> Fingerprint {
    Fingerprint(host.revision(), host.encode_full(), host.journal().seq)
}

fn batch(host: &Host, ops: Vec<Op>) -> Batch {
    Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "map".into(),
            plan: Plan {
                ops,
                readback: String::new(),
            },
        }],
    }
}

fn set(id: &str, key: &str, value: Value) -> Op {
    Op::Set {
        target: Target::Id(id.to_string()),
        props: [(key.to_string(), value)].into(),
    }
}

// ── authoring ───────────────────────────────────────────────────────────

#[test]
fn a_constraint_and_its_memberships_are_created_together_in_one_commit() {
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let invoked = host
        .invoke(
            &agent(),
            "atlas_constrain",
            json!({ "op": "sequence", "members": [a, b], "axis": "y" }),
        )
        .unwrap();
    assert_eq!(invoked.action, "constraint_add");
    let id = invoked.ids["constraint"].clone();

    let projected = project(&host);
    let c = constraint(&projected, &id);
    assert_eq!(c["op"], "sequence");
    assert_eq!(c["axis"], "y");
    assert_eq!(c["status"], "sat");
    // Born converted: a constraint authored here never has a legacy scalar.
    assert_eq!(c["membership"], SCHEMA_RECORDS);
    assert_eq!(c["legacy_members"], "");
    assert_eq!(targets(&projected, &id), vec![a.clone(), b.clone()]);

    // The memberships point back at the constraint through the minted id.
    let view = host.view();
    for member in member_records(&view, &id) {
        assert_eq!(
            member.props.get(K_CONSTRAINT).and_then(Scalar::as_str),
            Some(id.as_str())
        );
    }
}

#[test]
fn authoring_refusals_store_nothing() {
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let before = fingerprint(&host);
    for (args, why) in [
        (json!({ "op": "attaches", "members": [a.clone()] }), "arity"),
        (
            json!({ "op": "attaches", "members": [a.clone(), b.clone(), a.clone()] }),
            "arity and duplicate",
        ),
        (
            json!({ "op": "group", "members": [a.clone(), a.clone()] }),
            "duplicate",
        ),
        (
            json!({ "op": "group", "members": [a.clone(), "no-such-card"] }),
            "missing target",
        ),
        (json!({ "op": "sideways", "members": [a.clone()] }), "op"),
        (
            json!({ "op": "voids", "members": [a.clone()], "axis": "x" }),
            "axis on a non-sequence",
        ),
        (json!({ "op": "labels", "members": [a.clone()] }), "no text"),
    ] {
        assert!(
            matches!(
                host.invoke(&agent(), "constraint_add", args.clone()),
                Err(InvokeError::Capability { .. })
            ),
            "{why}: {args} was accepted"
        );
    }
    assert_eq!(fingerprint(&host), before);

    // A constraint may not name another constraint.
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "group", "members": [a, b] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "voids", "members": [id] })
        ),
        Err(InvokeError::Capability { .. })
    ));
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn removing_a_constraint_takes_its_memberships_in_one_commit() {
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "group", "members": [a, b] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    assert_eq!(member_records(&host.view(), &id).len(), 2);

    let invoked = host
        .invoke(&alice(), "atlas_unconstrain", json!({ "id": id.clone() }))
        .unwrap();
    assert_eq!(invoked.action, "constraint_remove");
    let commit = invoked.commit.unwrap();
    assert_eq!(
        commit.deleted.len(),
        3,
        "the constraint and both memberships"
    );
    let view = host.view();
    assert!(view.get(&id).is_none());
    assert!(member_records(&view, &id).is_empty());
    assert!(
        !view.records().any(|r| r.kind == CONSTRAINT_MEMBER),
        "no orphan memberships"
    );
}

#[test]
fn deleting_a_member_leaves_a_broken_constraint() {
    // Retained behaviour: the constraint keeps the membership and reports it.
    // The deletion is not refused, and the membership is not cascaded away.
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "group", "members": [a.clone(), b.clone()] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    host.invoke(&agent(), "object_remove", json!({ "id": b.clone() }))
        .unwrap();

    let projected = project(&host);
    let c = constraint(&projected, &id);
    assert_eq!(c["status"], "unresolved");
    assert_eq!(targets(&projected, &id), vec![a, b.clone()]);
    let member = c["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["target"] == b)
        .unwrap();
    assert_eq!(
        member["state"], "unresolved",
        "not 'missing': the map cannot see the id's kind"
    );
}

// ── the differential rules ──────────────────────────────────────────────

#[test]
fn an_arity_shortfall_is_refused_when_introduced_and_kept_when_inherited() {
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let c = place(&host, "C");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "attaches", "members": [a.clone(), b.clone()] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let members = member_records(&host.view(), &id)
        .iter()
        .map(|m| m.id.clone())
        .collect::<Vec<_>>();

    // Introduced: a batch that grows the constraint past its arity.
    let before = fingerprint(&host);
    let grow = batch(
        &host,
        vec![Op::Create {
            reference: "extra".into(),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::from(id.as_str())),
                (K_TARGET.to_string(), Value::from(c.as_str())),
                (K_ORDER.to_string(), Value::from(9.0)),
            ]
            .into(),
        }],
    );
    let refused = host.commit_batch(&agent(), &grow).unwrap_err();
    assert!(
        matches!(refused, CommitError::Validate { ref capability, .. } if capability == "map"),
        "{refused}"
    );
    assert_eq!(fingerprint(&host), before);

    // Inherited: the same change removes a membership, so the shortfall comes
    // from the removal and the commit stands.
    let shrink = batch(
        &host,
        vec![Op::Delete {
            target: Target::Id(members[1].clone()),
        }],
    );
    host.commit_batch(&agent(), &shrink).unwrap();
    let projected = project(&host);
    assert_eq!(constraint(&projected, &id)["status"], "arity");
    assert_eq!(targets(&projected, &id), vec![a]);

    // And an unrelated edit to the now-short constraint still commits.
    host.commit_batch(
        &alice(),
        &batch(&host, vec![set(&id, K_SALIENCE, "fore".into())]),
    )
    .unwrap();
    assert_eq!(constraint(&project(&host), &id)["salience"], "fore");
}

#[test]
fn a_newly_assigned_target_must_exist_while_an_inherited_broken_one_may_stay() {
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "group", "members": [a.clone(), b.clone()] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let members = member_records(&host.view(), &id)
        .iter()
        .map(|m| m.id.clone())
        .collect::<Vec<_>>();
    host.invoke(&agent(), "object_remove", json!({ "id": b }))
        .unwrap();

    // Inherited: editing the constraint's own keys is fine although one member
    // no longer resolves.
    host.invoke(
        &alice(),
        "constraint_add",
        json!({ "op": "group", "members": [a.clone(), place(&host, "D")] }),
    )
    .unwrap();

    // Introduced: pointing a membership at an id that does not exist.
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(
            &alice(),
            "constraint_member_retarget",
            json!({ "id": members[0].clone(), "target": "no-such-object" })
        ),
        Err(InvokeError::Capability { .. })
    ));
    let raw = batch(
        &host,
        vec![set(&members[0], K_TARGET, "no-such-object".into())],
    );
    assert!(host.commit_batch(&agent(), &raw).is_err());
    assert_eq!(fingerprint(&host), before);

    // A legitimate retarget commits.
    let d = place(&host, "E");
    host.invoke(
        &alice(),
        "constraint_member_retarget",
        json!({ "id": members[0].clone(), "target": d.clone() }),
    )
    .unwrap();
    assert!(targets(&project(&host), &id).contains(&d));
}

#[test]
fn the_same_bytes_as_a_replica_frame_are_judged_the_same_way() {
    // No rule may key on an action name: a frame carries none.
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "attaches", "members": [a, b] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let member = member_records(&host.view(), &id)[0].id.clone();

    let peer = yrs::Doc::new();
    peer.get_or_insert_map("objects");
    peer.transact_mut()
        .apply_update(yrs::Update::decode_v1(&host.encode_full()).unwrap())
        .unwrap();
    let mark = yrs::ReadTxn::state_vector(&peer.transact());
    {
        let objects = peer.get_or_insert_map("objects");
        let mut txn = peer.transact_mut();
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, member.as_str()) else {
            panic!("no membership {member}");
        };
        record.insert(&mut txn, K_TARGET, "no-such-object");
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

// ── the frozen legacy scalar ────────────────────────────────────────────

/// A document the Atlas wrote: a constraint whose membership is a comma-joined
/// scalar, holding one card that still exists and one that never will.
fn legacy_host(tag: &str) -> Arc<Host> {
    let doc = yrs::Doc::new();
    let objects = doc.get_or_insert_map("objects");
    {
        let mut txn = doc.transact_mut();
        for (id, label) in [("n1", "A"), ("n2", "B")] {
            let node = objects.insert(&mut txn, id, MapPrelim::default());
            node.insert(&mut txn, "kind", "node");
            node.insert(&mut txn, "label", label);
        }
        let c = objects.insert(&mut txn, "c1", MapPrelim::default());
        c.insert(&mut txn, "kind", CONSTRAINT);
        c.insert(&mut txn, K_OP, "group");
        // n3 was deleted long ago: the membership is already broken.
        c.insert(&mut txn, K_MEMBERS, "n1,n2,n3");
        c.insert(&mut txn, K_SALIENCE, "back");
    }
    let state =
        yrs::ReadTxn::encode_state_as_update_v1(&doc.transact(), &yrs::StateVector::default());
    let dir = std::env::temp_dir().join(format!(
        "samepage-map-constraints-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let legacy = dir.join("atlas.json");
    std::fs::write(
        &legacy,
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, state),
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

#[test]
fn an_unconverted_constraint_reads_from_the_scalar_and_says_so() {
    let host = legacy_host("read");
    let projected = project(&host);
    let c = constraint(&projected, "c1");
    assert_eq!(c["membership"], "legacy");
    assert_eq!(c["legacy_members"], "n1,n2,n3");
    assert_eq!(targets(&projected, "c1"), vec!["n1", "n2", "n3"]);
    let states: Vec<&str> = c["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["resolved", "resolved", "unresolved"]);
    assert_eq!(c["status"], "unresolved");
}

#[test]
fn conversion_carries_the_legacy_membership_over_broken_members_and_all() {
    let host = legacy_host("convert");
    let view = host.view();
    let legacy = membership(&view, "c1");
    assert_eq!(legacy, ["n1", "n2", "n3"]);

    // One batch: the memberships and the marker together. n3 does not resolve
    // and is carried over anyway, because conversion reproduces what was
    // authored rather than quietly dropping it.
    let mut ops: Vec<Op> = legacy
        .iter()
        .enumerate()
        .map(|(index, target)| Op::Create {
            reference: format!("m{index}"),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::from("c1")),
                (K_TARGET.to_string(), Value::from(target.as_str())),
                (K_ORDER.to_string(), Value::from(index as f64)),
            ]
            .into(),
        })
        .collect();
    ops.push(set("c1", K_MEMBER_SCHEMA, SCHEMA_RECORDS.into()));
    host.commit_batch(&alice(), &batch(&host, ops)).unwrap();

    let projected = project(&host);
    let c = constraint(&projected, "c1");
    assert_eq!(c["membership"], SCHEMA_RECORDS);
    assert_eq!(targets(&projected, "c1"), vec!["n1", "n2", "n3"]);
    assert_eq!(
        c["legacy_members"], "n1,n2,n3",
        "the scalar's bytes are untouched"
    );
}

#[test]
fn a_conversion_that_invents_a_member_is_refused() {
    let host = legacy_host("invent");
    let before = fingerprint(&host);
    let ops = vec![
        Op::Create {
            reference: "m0".into(),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::from("c1")),
                (K_TARGET.to_string(), Value::from("n1")),
                (K_ORDER.to_string(), Value::from(0.0)),
            ]
            .into(),
        },
        Op::Create {
            reference: "m1".into(),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::from("c1")),
                (K_TARGET.to_string(), Value::from("invented")),
                (K_ORDER.to_string(), Value::from(1.0)),
            ]
            .into(),
        },
        set("c1", K_MEMBER_SCHEMA, SCHEMA_RECORDS.into()),
    ];
    assert!(host.commit_batch(&alice(), &batch(&host, ops)).is_err());
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_converted_constraint_refuses_every_attempted_write_of_the_frozen_scalar() {
    let host = legacy_host("frozen");
    let legacy = membership(&host.view(), "c1");
    let mut ops: Vec<Op> = legacy
        .iter()
        .enumerate()
        .map(|(index, target)| Op::Create {
            reference: format!("m{index}"),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::from("c1")),
                (K_TARGET.to_string(), Value::from(target.as_str())),
                (K_ORDER.to_string(), Value::from(index as f64)),
            ]
            .into(),
        })
        .collect();
    ops.push(set("c1", K_MEMBER_SCHEMA, SCHEMA_RECORDS.into()));
    host.commit_batch(&alice(), &batch(&host, ops)).unwrap();

    let before = fingerprint(&host);
    // Non-empty, empty and same-value: each is an attempt to own history.
    for value in ["n1", "", "n1,n2,n3"] {
        let refused = host
            .commit_batch(
                &agent(),
                &batch(&host, vec![set("c1", K_MEMBERS, value.into())]),
            )
            .unwrap_err();
        assert!(
            format!("{refused}").contains("read-only"),
            "{value:?}: {refused}"
        );
    }
    // And clearing the marker, which would reinstate the scalar.
    for value in ["", "legacy"] {
        let refused = host
            .commit_batch(
                &agent(),
                &batch(&host, vec![set("c1", K_MEMBER_SCHEMA, value.into())]),
            )
            .unwrap_err();
        assert!(
            format!("{refused}").contains("cannot be cleared"),
            "{value:?}: {refused}"
        );
    }
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn deleting_the_last_membership_leaves_it_empty_and_never_reinstates_the_scalar() {
    let host = legacy_host("empty");
    let legacy = membership(&host.view(), "c1");
    let mut ops: Vec<Op> = legacy
        .iter()
        .enumerate()
        .map(|(index, target)| Op::Create {
            reference: format!("m{index}"),
            kind: CONSTRAINT_MEMBER.into(),
            props: [
                (K_CONSTRAINT.to_string(), Value::from("c1")),
                (K_TARGET.to_string(), Value::from(target.as_str())),
                (K_ORDER.to_string(), Value::from(index as f64)),
            ]
            .into(),
        })
        .collect();
    ops.push(set("c1", K_MEMBER_SCHEMA, SCHEMA_RECORDS.into()));
    host.commit_batch(&alice(), &batch(&host, ops)).unwrap();

    let members: Vec<String> = member_records(&host.view(), "c1")
        .iter()
        .map(|m| m.id.clone())
        .collect();
    host.commit_batch(
        &alice(),
        &batch(
            &host,
            members
                .iter()
                .map(|id| Op::Delete {
                    target: Target::Id(id.clone()),
                })
                .collect(),
        ),
    )
    .unwrap();

    let projected = project(&host);
    let c = constraint(&projected, "c1");
    assert!(
        targets(&projected, "c1").is_empty(),
        "empty, not resurrected"
    );
    assert_eq!(c["membership"], SCHEMA_RECORDS);
    assert_eq!(c["legacy_members"], "n1,n2,n3", "still there as history");
    assert_eq!(c["status"], "arity");
}

#[test]
fn one_batch_converts_retargets_and_dedups_while_a_broken_member_is_carried_over() {
    // The lift shape, as one atomic batch: a legacy constraint converts, the
    // lifted member is pointed at a card created in the same batch, a second
    // lifted member collapsing onto it is dropped, and a member that was
    // already broken before any of this survives untouched.
    let host = legacy_host("lift");
    let legacy = membership(&host.view(), "c1");
    assert_eq!(legacy, ["n1", "n2", "n3"], "n3 is already broken");

    let mut ops = vec![Op::Create {
        reference: "card".into(),
        kind: "node".into(),
        props: [("label".to_string(), Value::from("Lifted"))].into(),
    }];
    // n1 and n2 both become the new card: the lower order survives and the
    // other is simply not created, which is the dedup expressed as ops.
    ops.push(Op::Create {
        reference: "m0".into(),
        kind: CONSTRAINT_MEMBER.into(),
        props: [
            (K_CONSTRAINT.to_string(), Value::from("c1")),
            (K_TARGET.to_string(), Value::reference("card")),
            (K_ORDER.to_string(), Value::from(0.0)),
        ]
        .into(),
    });
    ops.push(Op::Create {
        reference: "m1".into(),
        kind: CONSTRAINT_MEMBER.into(),
        props: [
            (K_CONSTRAINT.to_string(), Value::from("c1")),
            (K_TARGET.to_string(), Value::from("n3")),
            (K_ORDER.to_string(), Value::from(1.0)),
        ]
        .into(),
    });
    ops.push(set("c1", K_MEMBER_SCHEMA, SCHEMA_RECORDS.into()));
    ops.push(Op::Delete {
        target: Target::Id("n1".into()),
    });
    ops.push(Op::Delete {
        target: Target::Id("n2".into()),
    });

    let commit = host.commit_batch(&alice(), &batch(&host, ops)).unwrap();
    assert_eq!(commit.deleted, ["n1", "n2"]);

    let projected = project(&host);
    let c = constraint(&projected, "c1");
    assert_eq!(c["membership"], SCHEMA_RECORDS);
    let members = targets(&projected, "c1");
    assert_eq!(members.len(), 2, "deduped: {members:?}");
    assert_eq!(
        members[1], "n3",
        "the already-broken member is carried over"
    );
    assert!(
        !members[0].is_empty() && members[0] != "n1" && members[0] != "n2",
        "the survivor points at the card minted in this batch: {members:?}"
    );
    assert_eq!(
        c["members"].as_array().unwrap()[0]["state"],
        "resolved",
        "the minted card resolves"
    );
    assert_eq!(
        c["members"].as_array().unwrap()[1]["state"],
        "unresolved",
        "n3 still does not, and is still not called missing"
    );
    assert_eq!(c["legacy_members"], "n1,n2,n3", "the scalar is untouched");
}

#[test]
fn membership_order_is_the_order_key_then_the_record_id() {
    // Two memberships sharing an order: the record id breaks the tie, so every
    // peer and the native host compute the same sequence.
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "sequence", "members": [a.clone(), b.clone()] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    let members: Vec<String> = member_records(&host.view(), &id)
        .iter()
        .map(|m| m.id.clone())
        .collect();
    let mut sorted = members.clone();
    sorted.sort();

    host.commit_batch(
        &alice(),
        &batch(
            &host,
            members
                .iter()
                .map(|member| set(member, K_ORDER, 3.0.into()))
                .collect(),
        ),
    )
    .unwrap();

    let view = host.view();
    let ordered: Vec<String> = member_records(&view, &id)
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(ordered, sorted, "ties break on the record id, ascending");
}

#[test]
fn a_raw_deletion_of_a_constraint_cascades_its_memberships() {
    // Not through `constraint_remove`, whose plan carries the deletions
    // itself: a batch that deletes only the constraint must still not leave
    // memberships behind, because the cascade is the kernel's, not the
    // action's.
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let id = host
        .invoke(
            &agent(),
            "constraint_add",
            json!({ "op": "group", "members": [a, b] }),
        )
        .unwrap()
        .ids["constraint"]
        .clone();
    assert_eq!(member_records(&host.view(), &id).len(), 2);

    let commit = host
        .commit_batch(
            &alice(),
            &batch(
                &host,
                vec![Op::Delete {
                    target: Target::Id(id.clone()),
                }],
            ),
        )
        .unwrap();
    assert_eq!(
        commit.deleted.len(),
        3,
        "the constraint and both memberships"
    );
    let view = host.view();
    assert!(
        !view.records().any(|r| r.kind == CONSTRAINT_MEMBER),
        "no orphan memberships"
    );
}
