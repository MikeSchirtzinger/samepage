//! Replica frames: honest peers converge, dishonest ones change nothing.

mod support;

use std::sync::Arc;

use samepage_capability::{Op, Plan, Scalar, Target, Value};
use samepage_doc::host::{FrameOutcome, FrameRefusal};
use samepage_doc::{sync, Replica};
use serde_json::json;
use support::*;
use yrs::{Any, Map, MapPrelim, Transact};

fn connect(host: &samepage_doc::host::Host) -> Replica {
    let mut replica = Replica::new();
    for frame in host.hello() {
        for reply in replica.receive(&frame).unwrap() {
            host.receive_frame(&alice(), &reply).unwrap();
        }
    }
    replica
        .set_composition(vec![Cards::descriptor(), Sources::descriptor()])
        .unwrap();
    replica
}

fn create_plan(label: &str) -> Plan {
    Plan {
        ops: vec![Op::Create {
            reference: "card".into(),
            kind: "card".into(),
            props: [
                ("label".to_string(), Value::from(label)),
                ("x".to_string(), Value::from(5.0)),
            ]
            .into(),
        }],
        readback: String::new(),
    }
}

fn move_plan(id: &str, x: f64) -> Plan {
    Plan {
        ops: vec![Op::Set {
            target: Target::Id(id.into()),
            props: [("x".to_string(), Value::from(x))].into(),
        }],
        readback: String::new(),
    }
}

fn send(
    host: &samepage_doc::host::Host,
    actor: &samepage_doc::Actor,
    replica: &mut Replica,
) -> Vec<FrameOutcome> {
    replica
        .take_outbound()
        .iter()
        .map(|frame| host.receive_frame(actor, frame).unwrap().outcome)
        .collect()
}

#[test]
fn an_honest_edit_commits_with_the_host_stamp_in_the_same_change() {
    let host = Arc::new(host());
    let seen = record_commits(&host);
    let mut replica = connect(&host);

    let applied = replica.apply("cards", create_plan("Queue")).unwrap();
    let id = applied.ids["card"].clone();
    assert!(id.starts_with(&format!("{:016x}-", replica.client_id())));
    // The browser does not stamp.
    assert!(!replica
        .view()
        .get(&id)
        .unwrap()
        .props
        .contains_key("touched_by"));

    let before = host.encode_full();
    assert!(matches!(
        send(&host, &alice(), &mut replica)[..],
        [FrameOutcome::Committed { .. }]
    ));

    let record = host.view().get(&id).unwrap().clone();
    assert_eq!(record.props["created_by"], Scalar::from("Alice"));
    assert_eq!(
        record.props["touched_by_participant"],
        Scalar::from("p-alice")
    );

    // The observer, called after the commit, already sees the stamp, and the
    // broadcast update alone carries the change and its stamp together.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (commit, view_at_callback) = &seen[0];
    assert_eq!(commit.created, vec![id.clone()]);
    assert_eq!(
        view_at_callback.get(&id).unwrap().props["created_by_participant"],
        Scalar::from("p-alice")
    );
    let bystander = Replica::new();
    let mut bystander = bystander;
    bystander.receive(&sync::step2(before)).unwrap();
    bystander.receive(&commit.update).unwrap();
    let broadcast = bystander.view().get(&id).unwrap().clone();
    assert_eq!(broadcast.props["touched_by"], Scalar::from("Alice"));
    assert_eq!(broadcast.props["label"], Scalar::from("Queue"));

    // The author receives the stamp too and converges.
    replica.receive(&commit.update).unwrap();
    assert_eq!(replica.view(), host.view());
    assert_eq!(replica.revision(), host.revision());
    assert!(host.journal().bindings.contains_key(&replica.client_id()));
}

#[test]
fn concurrent_edits_converge_including_a_losing_write_to_the_same_key() {
    let host = host();
    let id = place(&host, "API");
    let mut replica = connect(&host);

    // Offline: the human moves the card; meanwhile the agent moves it too and
    // edits its note. Per-key last-write-wins keeps both keys' writes.
    replica.apply("cards", move_plan(&id, 40.0)).unwrap();
    host.invoke(&agent(), "card_move", json!({ "id": id, "x": 90.0 }))
        .unwrap();

    // Reconnect: the replica catches up, then sends its edit.
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    let outcomes = send(&host, &alice(), &mut replica);
    assert!(
        matches!(outcomes[..], [FrameOutcome::Committed { .. }]),
        "{outcomes:?}"
    );
    // Deliver every commit back to the replica.
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    assert_eq!(replica.view(), host.view());
    assert_eq!(replica.revision(), host.revision());
    let x = host.view().get(&id).unwrap().props["x"].clone();
    assert!(
        x == Scalar::Num(40.0) || x == Scalar::Num(90.0),
        "one of the writes wins: {x:?}"
    );
}

#[test]
fn per_key_writes_by_human_and_agent_both_survive() {
    // Ported from the scene layer's convergence test: the human drags x while
    // the agent writes the note on the same card; neither write is lost.
    let host = host();
    let id = place(&host, "API");
    let mut replica = connect(&host);
    replica.apply("cards", move_plan(&id, 12.0)).unwrap();
    host.invoke(&agent(), "card_seal", json!({ "id": id }))
        .unwrap_err(); // human-only
    let human = Arc::new(samepage_doc::Actor::new(
        "p-alice",
        samepage_doc::Caller::Human,
    ));
    host.invoke(&human, "card_seal", json!({ "id": id }))
        .unwrap();
    send(&host, &alice(), &mut replica);
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    let record = host.view().get(&id).unwrap().clone();
    assert_eq!(record.props["x"], Scalar::Num(12.0));
    assert_eq!(record.props["note"], Scalar::from("sealed"));
    assert_eq!(replica.view(), host.view());
}

#[test]
fn reordered_frames_are_held_then_commit_in_full_as_their_author() {
    let host = host();
    let mut replica = connect(&host);
    let id = replica.apply("cards", create_plan("A")).unwrap().ids["card"].clone();
    replica.apply("cards", move_plan(&id, 7.0)).unwrap();
    let frames = replica.take_outbound();
    assert_eq!(frames.len(), 2);

    let before = fingerprint(&host);
    let second = host.receive_frame(&alice(), &frames[1]).unwrap();
    assert_eq!(second.outcome, FrameOutcome::Held { pending_updates: 1 });
    let held = fingerprint(&host);
    assert_eq!(
        held.revision, before.revision,
        "held content never enters the live document"
    );
    assert_eq!(held.state, before.state);
    assert_eq!(held.seq, before.seq);
    assert_eq!(held.held.get("p-alice"), Some(&1));

    let first = host.receive_frame(&alice(), &frames[0]).unwrap();
    assert!(matches!(first.outcome, FrameOutcome::Committed { .. }));
    assert!(host.held().is_empty());
    let record = host.view().get(&id).unwrap().clone();
    assert_eq!(record.props["x"], Scalar::Num(7.0));
    assert_eq!(
        record.props["touched_by_participant"],
        Scalar::from("p-alice")
    );
}

#[test]
fn a_delete_that_arrives_before_its_insert_waits_for_it() {
    let host = host();
    let mut replica = connect(&host);
    let id = replica.apply("cards", create_plan("Temp")).unwrap().ids["card"].clone();
    replica
        .apply(
            "cards",
            Plan {
                ops: vec![Op::Delete {
                    target: Target::Id(id.clone()),
                }],
                readback: String::new(),
            },
        )
        .unwrap();
    let frames = replica.take_outbound();
    let rev = host.revision();
    assert!(matches!(
        host.receive_frame(&alice(), &frames[1]).unwrap().outcome,
        FrameOutcome::Held { .. }
    ));
    assert_eq!(host.revision(), rev);
    host.receive_frame(&alice(), &frames[0]).unwrap();
    assert!(host.held().is_empty());
    assert!(host.view().get(&id).is_none());
    for frame in host.hello() {
        replica.receive(&frame).unwrap();
    }
    assert_eq!(replica.revision(), host.revision());
}

#[test]
fn a_held_payload_resolves_when_another_actor_commits_and_stays_its_authors() {
    let host = Arc::new(host());
    let seen = record_commits(&host);
    // Bob creates a card but has not sent it yet.
    let mut bob_peer = Peer::of(&host);
    let bob_card = format!("{:016x}-{:08x}", bob_peer.client(), 0);
    {
        let objects = bob_peer.objects();
        let mut txn = bob_peer.doc.transact_mut();
        let card = objects.insert(&mut txn, bob_card.as_str(), MapPrelim::default());
        card.insert(&mut txn, "kind", "card");
        card.insert(&mut txn, "label", "Bob's");
    }
    let bob_frame = bob_peer.frame();
    // Alice got Bob's card directly and moves it; her update depends on it.
    let mut alice_peer = Peer::of(&host);
    alice_peer.apply_frame(&bob_frame);
    let _ = alice_peer.frame();
    {
        let objects = alice_peer.objects();
        let mut txn = alice_peer.doc.transact_mut();
        let Some(yrs::Out::YMap(card)) = objects.get(&txn, &bob_card) else {
            panic!()
        };
        card.insert(&mut txn, "x", 33.0);
    }
    let alice_frame = alice_peer.frame();

    assert!(matches!(
        host.receive_frame(&alice(), &alice_frame).unwrap().outcome,
        FrameOutcome::Held { .. }
    ));
    // Bob sends nothing more than his own frame; Alice sends nothing at all.
    host.receive_frame(&bob(), &bob_frame).unwrap();
    assert!(
        host.held().is_empty(),
        "Alice's payload drained after Bob's commit"
    );
    let record = host.view().get(&bob_card).unwrap().clone();
    assert_eq!(record.props["x"], Scalar::Num(33.0));
    assert_eq!(
        record.props["created_by_participant"],
        Scalar::from("p-bob")
    );
    assert_eq!(
        record.props["touched_by_participant"],
        Scalar::from("p-alice"),
        "the drained edit is attributed to its original author"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0.actor.participant, "p-bob");
    assert_eq!(seen[1].0.actor.participant, "p-alice");
}

#[test]
fn an_exact_replay_adds_no_commit_stamp_or_journal_entry() {
    let host = Arc::new(host());
    let seen = record_commits(&host);
    let mut replica = connect(&host);
    replica.apply("cards", create_plan("Once")).unwrap();
    let frame = replica.take_outbound().remove(0);
    host.receive_frame(&alice(), &frame).unwrap();
    let after_first = fingerprint(&host);
    let replay = host.receive_frame(&alice(), &frame).unwrap();
    assert_eq!(replay.outcome, FrameOutcome::SyncOnly);
    assert_eq!(fingerprint(&host), after_first);
    // A full-state replay from any peer is also nothing new.
    let full = sync::step2(host.encode_full());
    assert_eq!(
        host.receive_frame(&bob(), &full).unwrap().outcome,
        FrameOutcome::SyncOnly
    );
    assert_eq!(fingerprint(&host), after_first);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_delete_only_frame_is_audited_and_moves_the_revision() {
    let host = host();
    let id = place(&host, "Doomed");
    let mut replica = connect(&host);
    replica
        .apply(
            "cards",
            Plan {
                ops: vec![Op::Delete {
                    target: Target::Id(id.clone()),
                }],
                readback: String::new(),
            },
        )
        .unwrap();
    let rev = host.revision();
    let state_vector_before = host.encode_full().len();
    let outcomes = send(&host, &alice(), &mut replica);
    assert!(matches!(outcomes[..], [FrameOutcome::Committed { .. }]));
    assert_ne!(host.revision(), rev, "a deletion changes the revision");
    assert!(host.view().get(&id).is_none());
    let entry = host.journal().recent.back().unwrap().clone();
    assert_eq!(entry.deleted, vec![id]);
    assert!(state_vector_before > 0);
}

// ── refusals: each leaves every observable part of the host unchanged ─────

fn refused(
    host: &samepage_doc::host::Host,
    actor: &samepage_doc::Actor,
    frame: &[u8],
) -> FrameRefusal {
    let before = fingerprint(host);
    let refusal = host
        .receive_frame(actor, frame)
        .expect_err("frame must be refused");
    assert_eq!(
        fingerprint(host),
        before,
        "a refusal changed the host: {refusal}"
    );
    refusal
}

fn peer_with_card() -> (samepage_doc::host::Host, Peer, String) {
    let host = host();
    let id = place(&host, "API");
    let peer = Peer::of(&host);
    (host, peer, id)
}

fn edit(peer: &Peer, id: &str, f: impl FnOnce(&mut yrs::TransactionMut, &yrs::MapRef)) {
    let objects = peer.objects();
    let mut txn = peer.doc.transact_mut();
    let Some(yrs::Out::YMap(card)) = objects.get(&txn, id) else {
        panic!("no {id}")
    };
    f(&mut txn, &card);
}

#[test]
fn a_same_value_write_to_an_authorship_key_is_refused() {
    let (host, mut peer, id) = peer_with_card();
    let current = host.view().get(&id).unwrap().props["touched_by"].clone();
    let Scalar::Str(current) = current else {
        panic!()
    };
    edit(&peer, &id, |txn, card| {
        card.insert(txn, "touched_by", current.as_str());
    });
    let frame = peer.frame();
    assert!(host.view().get(&id).unwrap().props["touched_by"] == Scalar::from(current.as_str()));
    assert!(
        matches!(refused(&host, &alice(), &frame), FrameRefusal::Write { key, .. } if key == "touched_by")
    );
}

#[test]
fn a_write_then_restore_of_an_authorship_key_is_refused() {
    let (host, mut peer, id) = peer_with_card();
    let Scalar::Str(current) = host.view().get(&id).unwrap().props["touched_by"].clone() else {
        panic!()
    };
    edit(&peer, &id, |txn, card| {
        card.insert(txn, "touched_by", "Mallory");
        card.insert(txn, "touched_by", current.as_str());
    });
    let frame = peer.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::Write { .. }
    ));
}

#[test]
fn an_extra_root_is_refused() {
    let (host, mut peer, _) = peer_with_card();
    peer.doc
        .get_or_insert_map("hidden")
        .insert(&mut peer.doc.transact_mut(), "k", "v");
    let frame = peer.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::HiddenStructure(_)
    ));
}

#[test]
fn a_nested_type_inside_a_record_is_refused() {
    let (host, mut peer, id) = peer_with_card();
    edit(&peer, &id, |txn, card| {
        card.insert(
            txn,
            "label",
            MapPrelim::from([("inner".to_string(), Any::from("x"))]),
        );
    });
    let frame = peer.frame();
    let refusal = refused(&host, &alice(), &frame);
    assert!(
        matches!(refusal, FrameRefusal::HiddenStructure(_)),
        "{refusal}"
    );
}

#[test]
fn hidden_sequence_content_under_a_record_is_refused() {
    let (host, mut peer, id) = peer_with_card();
    edit(&peer, &id, |txn, card| {
        card.insert(txn, "note", yrs::TextPrelim::new("hidden"));
    });
    let frame = peer.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::HiddenStructure(_)
    ));
}

#[test]
fn an_identical_looking_replacement_map_is_refused() {
    let (host, mut peer, id) = peer_with_card();
    let record = host.view().get(&id).unwrap().clone();
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let replacement = objects.insert(&mut txn, id.as_str(), MapPrelim::default());
        replacement.insert(&mut txn, "kind", record.kind.as_str());
        for (key, value) in &record.props {
            let value = match value {
                Scalar::Str(s) => Any::from(s.as_str()),
                Scalar::Num(n) => Any::Number(yrs::Number::try_i64(*n)),
                Scalar::Bool(b) => Any::Bool(*b),
            };
            replacement.insert(&mut txn, key.as_str(), value);
        }
    }
    let frame = peer.frame();
    let refusal = refused(&host, &alice(), &frame);
    assert!(
        matches!(
            refusal,
            FrameRefusal::Replaced(_) | FrameRefusal::Write { .. }
        ),
        "{refusal}"
    );
}

#[test]
fn a_packed_valid_then_invalid_frame_changes_nothing() {
    let (host, mut peer, id) = peer_with_card();
    edit(&peer, &id, |txn, card| {
        card.insert(txn, "x", 50.0);
    });
    let valid = peer.frame();
    peer.doc
        .get_or_insert_map("hidden")
        .insert(&mut peer.doc.transact_mut(), "k", "v");
    let invalid = peer.frame();
    let mut packed = valid.clone();
    packed.extend_from_slice(&invalid);
    let host = Arc::new(host);
    let seen = record_commits(&host);
    refused(&host, &alice(), &packed);
    assert!(
        seen.lock().unwrap().is_empty(),
        "no callback for a refused frame"
    );
    // The valid half alone would have been accepted.
    assert!(matches!(
        host.receive_frame(&alice(), &valid).unwrap().outcome,
        FrameOutcome::Committed { .. }
    ));
}

#[test]
fn new_content_under_the_hosts_or_another_participants_client_is_refused() {
    let (host, _, id) = peer_with_card();
    // Impersonating the host's client id.
    let mut impostor = Peer::with_client(host.client_id(), &host);
    edit(&impostor, &id, |txn, card| {
        card.insert(txn, "x", 2.0);
    });
    let frame = impostor.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::Identity { .. }
    ));

    // Bob registers a client; Alice may not write under it.
    let mut bob_peer = Peer::of(&host);
    edit(&bob_peer, &id, |txn, card| {
        card.insert(txn, "x", 3.0);
    });
    host.receive_frame(&bob(), &bob_peer.frame()).unwrap();
    let mut stolen = Peer::with_client(bob_peer.client(), &host);
    edit(&stolen, &id, |txn, card| {
        card.insert(txn, "x", 4.0);
    });
    let frame = stolen.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::Identity { .. }
    ));
    // Bob himself may keep using it, including after reconnecting.
    edit(&bob_peer, &id, |txn, card| {
        card.insert(txn, "x", 5.0);
    });
    assert!(host.receive_frame(&bob(), &bob_peer.frame()).is_ok());
}

#[test]
fn a_record_named_outside_the_senders_namespace_is_refused() {
    let (host, mut peer, _) = peer_with_card();
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let card = objects.insert(&mut txn, "n1", MapPrelim::default());
        card.insert(&mut txn, "kind", "card");
        card.insert(&mut txn, "label", "x");
    }
    let frame = peer.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::ForeignId { .. }
    ));
}

#[test]
fn unknown_kinds_unclaimed_keys_and_capability_refusals_change_nothing() {
    let (host, mut peer, id) = peer_with_card();
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let name = format!("{:016x}-{:08x}", peer.doc.client_id().get(), 0);
        let segment = objects.insert(&mut txn, name.as_str(), MapPrelim::default());
        segment.insert(&mut txn, "kind", "segment-mask");
    }
    let frame = peer.frame();
    let refusal = refused(&host, &alice(), &frame);
    assert!(
        matches!(refusal, FrameRefusal::Write { .. } | FrameRefusal::Audit(_)),
        "{refusal}"
    );

    let (host, mut peer, id2) = peer_with_card();
    let _ = id;
    edit(&peer, &id2, |txn, card| {
        card.insert(txn, "colour", "red");
    });
    let frame = peer.frame();
    assert!(
        matches!(refused(&host, &alice(), &frame), FrameRefusal::Write { key, .. } if key == "colour")
    );

    let (host, mut peer, id3) = peer_with_card();
    edit(&peer, &id3, |txn, card| {
        card.insert(txn, "label", "");
    });
    let frame = peer.frame();
    assert!(matches!(
        refused(&host, &alice(), &frame),
        FrameRefusal::Validate { .. }
    ));
}

#[test]
fn non_document_and_truncated_frames_are_refused() {
    let host = host();
    let awareness = {
        use yrs::updates::encoder::Encode;
        yrs::sync::Message::AwarenessQuery.encode_v1()
    };
    assert!(matches!(
        refused(&host, &alice(), &awareness),
        FrameRefusal::Sync(_)
    ));
    let mut replica = connect(&host);
    replica.apply("cards", create_plan("Cut")).unwrap();
    let frame = replica.take_outbound().remove(0);
    let truncated = &frame[..frame.len() - 3];
    assert!(matches!(
        refused(&host, &alice(), truncated),
        FrameRefusal::Sync(_)
    ));
}

#[test]
fn quarantine_is_bounded_and_overflow_changes_nothing() {
    let host = host();
    let peer = Peer::of(&host);
    let base = peer.sv();
    let id = format!("{:016x}-{:08x}", peer.client(), 0);
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let card = objects.insert(&mut txn, id.as_str(), MapPrelim::default());
        card.insert(&mut txn, "kind", "card");
        card.insert(&mut txn, "label", "first");
    }
    let first_sv = peer.sv();
    let mut frames = Vec::new();
    for step in 0..(samepage_doc::host::MAX_HELD_UPDATES + 1) {
        let before = peer.sv();
        edit(&peer, &id, |txn, card| {
            card.insert(txn, "x", step as f64);
        });
        frames.push(sync::update(peer.since(&before)));
    }
    let _ = base;
    let _ = first_sv;
    // Every later frame depends on the first, which never arrives.
    for frame in &frames[..samepage_doc::host::MAX_HELD_UPDATES] {
        assert!(matches!(
            host.receive_frame(&alice(), frame).unwrap().outcome,
            FrameOutcome::Held { .. }
        ));
    }
    let last = frames.last().unwrap();
    let held_before = host.held();
    assert!(matches!(
        refused(&host, &alice(), last),
        FrameRefusal::QuarantineFull { .. }
    ));
    assert_eq!(
        host.held(),
        held_before,
        "overflow keeps what was already held"
    );
}

#[test]
fn a_refused_frame_leaves_that_client_unregistered_and_the_live_document_clean() {
    let (host, mut peer, _) = peer_with_card();
    peer.doc
        .get_or_insert_map("hidden")
        .insert(&mut peer.doc.transact_mut(), "k", "v");
    let frame = peer.frame();
    refused(&host, &alice(), &frame);
    assert!(!host.journal().bindings.contains_key(&peer.client()));
    let live = samepage_doc::Document::from_state(&host.encode_full()).unwrap();
    assert!(!live.has_missing_updates());
    assert!(live.inspect().anomalies.is_empty());
    let _ = Value::from(1.0);
    let _ = json!({});
}

#[test]
fn sequence_content_aliased_under_a_map_branch_is_refused() {
    // A peer that declares the `objects` root as an array writes indexed
    // sequence content into the same branch. Map reads and map events do not
    // show it; the layout check does.
    let (host, mut peer, _) = peer_with_card();
    {
        let alias = peer.doc.get_or_insert_array("objects");
        let _ = alias;
    }
    let host_state = host.encode_full();
    let alias_doc = yrs::Doc::new();
    let alias = alias_doc.get_or_insert_array("objects");
    alias_doc
        .transact_mut()
        .apply_update(yrs::updates::decoder::Decode::decode_v1(&host_state).unwrap())
        .unwrap();
    let mark = yrs::ReadTxn::state_vector(&alias_doc.transact());
    yrs::Array::push_back(&alias, &mut alias_doc.transact_mut(), "hidden");
    let frame = sync::update(yrs::ReadTxn::encode_state_as_update_v1(
        &alias_doc.transact(),
        &mark,
    ));
    let _ = peer.frame();
    let refusal = refused(&host, &alice(), &frame);
    assert!(
        matches!(refusal, FrameRefusal::HiddenStructure(ref at) if at.iter().any(|a| a == "sequence:objects")),
        "{refusal}"
    );
}

#[test]
fn sequence_content_aliased_under_a_record_branch_is_refused() {
    let (host, _peer, id) = peer_with_card();
    let alias_doc = yrs::Doc::new();
    alias_doc.get_or_insert_map("objects");
    alias_doc
        .transact_mut()
        .apply_update(yrs::updates::decoder::Decode::decode_v1(&host.encode_full()).unwrap())
        .unwrap();
    let mark = yrs::ReadTxn::state_vector(&alias_doc.transact());
    {
        let objects = alias_doc.get_or_insert_map("objects");
        let mut txn = alias_doc.transact_mut();
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, &id) else {
            panic!()
        };
        // Reach the record branch as a sequence through a typed cast.
        let branch: &yrs::branch::Branch = record.as_ref();
        let array = yrs::ArrayRef::from(yrs::branch::BranchPtr::from(branch));
        yrs::Array::push_back(&array, &mut txn, "hidden");
    }
    let frame = sync::update(yrs::ReadTxn::encode_state_as_update_v1(
        &alias_doc.transact(),
        &mark,
    ));
    let refusal = refused(&host, &alice(), &frame);
    assert!(
        matches!(refusal, FrameRefusal::HiddenStructure(ref at) if at.iter().any(|a| a.starts_with("sequence:record:"))),
        "{refusal}"
    );
    // The record's ordinary map content is still read normally.
    assert_eq!(
        host.view().get(&id).unwrap().props["label"],
        Scalar::from("API")
    );
}

#[test]
fn held_updates_keep_their_actor_and_a_relabelled_sender_is_refused() {
    let host = Arc::new(host());
    let seen = record_commits(&host);
    let mut replica = connect(&host);
    let id = replica.apply("cards", create_plan("A")).unwrap().ids["card"].clone();
    replica.apply("cards", move_plan(&id, 8.0)).unwrap();
    let frames = replica.take_outbound();
    assert!(matches!(
        host.receive_frame(&alice(), &frames[1]).unwrap().outcome,
        FrameOutcome::Held { .. }
    ));

    let relabelled =
        samepage_doc::Actor::new("p-alice", samepage_doc::Caller::Human).labelled("Mallory");
    let refusal = refused(&host, &relabelled, &frames[0]);
    assert!(
        matches!(refusal, FrameRefusal::ActorChanged { .. }),
        "{refusal}"
    );

    host.receive_frame(&alice(), &frames[0]).unwrap();
    let record = host.view().get(&id).unwrap().clone();
    assert_eq!(record.props["touched_by"], Scalar::from("Alice"));
    assert!(seen.lock().unwrap().iter().all(|(c, _)| c.actor == alice()));
}
