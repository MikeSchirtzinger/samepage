//! Actions and batches: resolved inside the kernel, atomic, attributed.

mod support;

use std::sync::Arc;

use samepage_capability::{Batch, BatchEntry, Op, Plan, PlanError, Scalar, Target, Value};
use samepage_doc::host::{CommitError, InvokeError};
use serde_json::json;
use support::*;
use yrs::{Map as _, Transact as _};

#[test]
fn an_action_commits_with_its_actor_and_preferred_name() {
    let host = Arc::new(host());
    let seen = record_commits(&host);
    let invoked = host
        .invoke(
            &agent(),
            "atlas_place",
            json!({ "label": "Queue", "x": 3.0 }),
        )
        .unwrap();
    assert_eq!(
        invoked.action, "card_place",
        "an alias records the preferred name"
    );
    let id = invoked.ids["card"].clone();
    assert!(id.starts_with(&format!("{:016x}-", host.client_id())));
    let record = host.view().get(&id).unwrap().clone();
    assert_eq!(record.props["created_by"], Scalar::from("Model"));
    assert_eq!(
        record.props["touched_by_participant"],
        Scalar::from("p-agent")
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.action.as_deref(), Some("card_place"));
    assert_eq!(
        seen[0].1.get(&id).unwrap().props["created_by"],
        Scalar::from("Model")
    );
    assert_eq!(host.journal().latest["p-agent"], seen[0].0.seq);
}

#[test]
fn a_query_that_plans_a_write_is_refused_and_changes_nothing() {
    let host = host();
    let id = place(&host, "A");
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(&agent(), "page_sneak", json!({ "id": id })),
        Err(InvokeError::QueryMutates { ops: 1, .. })
    ));
    assert_eq!(fingerprint(&host), before);
    let count = host.invoke(&agent(), "page_count", json!({})).unwrap();
    assert_eq!(count.readback, vec!["1 cards".to_string()]);
    assert!(count.commit.is_none());
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn audience_is_enforced_at_invocation() {
    let host = host();
    let id = place(&host, "A");
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(&agent(), "card_seal", json!({ "id": id })),
        Err(InvokeError::Audience {
            caller: "agent",
            ..
        })
    ));
    assert!(matches!(
        host.invoke(&alice(), "source_bind", json!({ "id": id, "path": "a.rs" })),
        Err(InvokeError::Audience {
            caller: "human",
            ..
        })
    ));
    assert_eq!(fingerprint(&host), before);
    host.invoke(&alice(), "card_seal", json!({ "id": id }))
        .unwrap();
    assert!(matches!(
        host.invoke(&agent(), "no_such", json!({})),
        Err(InvokeError::UnknownAction(_))
    ));
}

fn bound_card_batch(host: &samepage_doc::host::Host) -> Batch {
    Batch {
        base_revision: host.revision(),
        entries: vec![
            BatchEntry {
                capability: "cards".into(),
                plan: Plan {
                    ops: vec![Op::Create {
                        reference: "c".into(),
                        kind: "card".into(),
                        props: [("label".to_string(), Value::from("Bound"))].into(),
                    }],
                    readback: String::new(),
                },
            },
            BatchEntry {
                capability: "sources".into(),
                plan: Plan {
                    ops: vec![Op::Set {
                        target: Target::Ref("c".into()),
                        props: [("path".to_string(), Value::from("src/lib.rs"))].into(),
                    }],
                    readback: String::new(),
                },
            },
        ],
    }
}

#[test]
fn a_mixed_capability_batch_is_one_atomic_attributed_commit() {
    let host = host();
    let commit = host
        .commit_batch(&agent(), &bound_card_batch(&host))
        .unwrap();
    assert_eq!(commit.created.len(), 1);
    let record = host.view().get(&commit.created[0]).unwrap().clone();
    assert_eq!(record.props["path"], Scalar::from("src/lib.rs"));
    assert_eq!(
        record.props["created_by_participant"],
        Scalar::from("p-agent")
    );
}

#[test]
fn a_human_batch_writing_an_agent_only_binding_is_refused_by_the_capability_policy() {
    // Before the authority hook, this exact batch from a human committed.
    let host = Arc::new(host());
    let seen = record_commits(&host);
    let before = fingerprint(&host);
    assert!(matches!(
        host.commit_batch(&alice(), &bound_card_batch(&host)),
        Err(CommitError::Validate { ref capability, ref error }) if capability == "sources" && error.code == "agent_only"
    ));
    assert_eq!(fingerprint(&host), before);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_failing_entry_or_stale_base_leaves_state_and_revision_unchanged() {
    let host = host();
    let before = fingerprint(&host);
    let bad = Batch {
        base_revision: host.revision(),
        entries: vec![
            BatchEntry {
                capability: "cards".into(),
                plan: Plan {
                    ops: vec![Op::Create {
                        reference: "c".into(),
                        kind: "card".into(),
                        props: [("label".to_string(), Value::from("A"))].into(),
                    }],
                    readback: String::new(),
                },
            },
            BatchEntry {
                capability: "sources".into(),
                plan: Plan {
                    ops: vec![Op::Set {
                        target: Target::Ref("c".into()),
                        props: [("label".to_string(), Value::from("stolen"))].into(),
                    }],
                    readback: String::new(),
                },
            },
        ],
    };
    assert!(matches!(
        host.commit_batch(&alice(), &bad),
        Err(CommitError::Plan(PlanError::KeyNotWritable { .. }))
    ));
    assert_eq!(fingerprint(&host), before);

    let stale = Batch {
        base_revision: "0".repeat(64),
        entries: vec![],
    };
    assert!(matches!(
        host.commit_batch(&alice(), &stale),
        Err(CommitError::Plan(PlanError::Stale { .. }))
    ));
    // A capability's own validation refusal is atomic too.
    let empty_label = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "cards".into(),
            plan: Plan {
                ops: vec![Op::Create {
                    reference: "c".into(),
                    kind: "card".into(),
                    props: [("label".to_string(), Value::from(""))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    assert!(matches!(
        host.commit_batch(&alice(), &empty_label),
        Err(CommitError::Validate { .. })
    ));
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_batch_cannot_write_authorship() {
    let host = host();
    let id = place(&host, "A");
    let before = fingerprint(&host);
    let forged = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "cards".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(id),
                    props: [("touched_by".to_string(), Value::from("Alice"))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    assert!(matches!(
        host.commit_batch(&agent(), &forged),
        Err(CommitError::Plan(PlanError::ReservedKey { .. }))
    ));
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn removing_a_card_cascades_to_its_links_in_the_same_commit() {
    let host = host();
    let a = place(&host, "A");
    let b = place(&host, "B");
    let link = host
        .invoke(&agent(), "card_link", json!({ "from": a, "to": b }))
        .unwrap()
        .ids["link"]
        .clone();
    let commit = host
        .invoke(&alice(), "object_remove", json!({ "id": a }))
        .unwrap()
        .commit
        .unwrap();
    let mut deleted = commit.deleted.clone();
    deleted.sort();
    let mut expected = vec![a, link];
    expected.sort();
    assert_eq!(deleted, expected);
}

/// Refuses any path that names a file outside `src/`.
pub struct StrictSources;

impl samepage_capability::Capability for StrictSources {
    fn descriptor(&self) -> samepage_capability::Descriptor {
        Sources::descriptor()
    }
    fn project(
        &self,
        _v: &samepage_capability::View,
    ) -> Result<serde_json::Value, samepage_capability::CapError> {
        Ok(json!({}))
    }
    fn plan(
        &self,
        call: &samepage_capability::Call,
        view: &samepage_capability::View,
    ) -> Result<Plan, samepage_capability::CapError> {
        Sources.plan(call, view)
    }
    fn validate(
        &self,
        delta: &samepage_capability::Delta,
        view: &samepage_capability::View,
    ) -> Result<(), samepage_capability::CapError> {
        for id in delta.created.iter().chain(delta.modified.keys()) {
            if let Some(path) = view
                .get(id)
                .and_then(|r| r.props.get("path"))
                .and_then(|p| p.as_str())
            {
                if !path.starts_with("src/") {
                    return Err(samepage_capability::CapError::new("outside_src", path));
                }
            }
        }
        Ok(())
    }
    fn digest(
        &self,
        _v: &samepage_capability::View,
    ) -> Result<serde_json::Value, samepage_capability::CapError> {
        Ok(json!({}))
    }
}

fn strict_host() -> Arc<samepage_doc::host::Host> {
    Arc::new(
        samepage_doc::host::Host::in_memory(
            composition(),
            vec![Arc::new(Cards), Arc::new(StrictSources)],
        )
        .unwrap(),
    )
}

#[test]
fn an_extension_key_on_a_created_record_is_validated_by_its_writer() {
    let host = strict_host();
    let seen = record_commits(&host);
    let before = fingerprint(&host);
    let batch = Batch {
        base_revision: host.revision(),
        entries: vec![
            BatchEntry {
                capability: "cards".into(),
                plan: Plan {
                    ops: vec![Op::Create {
                        reference: "c".into(),
                        kind: "card".into(),
                        props: [("label".to_string(), Value::from("New"))].into(),
                    }],
                    readback: String::new(),
                },
            },
            BatchEntry {
                capability: "sources".into(),
                plan: Plan {
                    ops: vec![Op::Set {
                        target: Target::Ref("c".into()),
                        props: [("path".to_string(), Value::from("/etc/passwd"))].into(),
                    }],
                    readback: String::new(),
                },
            },
        ],
    };
    assert!(matches!(
        host.commit_batch(&alice(), &batch),
        Err(CommitError::Validate { ref capability, .. }) if capability == "sources"
    ));
    assert_eq!(fingerprint(&host), before);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn an_extension_key_on_a_record_created_by_a_replica_is_validated_by_its_writer() {
    let host = strict_host();
    let seen = record_commits(&host);
    let mut peer = Peer::of(&host);
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let name = format!("{:016x}-{:08x}", peer.client(), 0);
        let card = objects.insert(&mut txn, name.as_str(), yrs::MapPrelim::default());
        yrs::Map::insert(&card, &mut txn, "kind", "card");
        yrs::Map::insert(&card, &mut txn, "label", "New");
        yrs::Map::insert(&card, &mut txn, "path", "/etc/passwd");
    }
    let before = fingerprint(&host);
    let refusal = host.receive_frame(&alice(), &peer.frame()).unwrap_err();
    assert!(
        matches!(refusal, samepage_doc::host::FrameRefusal::Validate { ref capability, .. } if capability == "sources"),
        "{refusal}"
    );
    assert_eq!(fingerprint(&host), before);
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn an_empty_batch_or_empty_mutating_plan_is_an_explicit_no_op() {
    let host = strict_host();
    let seen = record_commits(&host);
    place(&host, "A");
    let before = fingerprint(&host);
    let calls = seen.lock().unwrap().len();

    let empty = Batch {
        base_revision: host.revision(),
        entries: vec![],
    };
    assert!(matches!(
        host.commit_batch(&alice(), &empty),
        Err(CommitError::Empty)
    ));
    let empty_plan = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "cards".into(),
            plan: Plan::default(),
        }],
    };
    assert!(matches!(
        host.commit_batch(&alice(), &empty_plan),
        Err(CommitError::Empty)
    ));
    let invoked = host.invoke(&agent(), "card_tidy", json!({})).unwrap();
    assert!(invoked.commit.is_none());
    assert_eq!(invoked.readback, vec!["Nothing to tidy.".to_string()]);

    assert_eq!(
        fingerprint(&host),
        before,
        "no journal entry, no state change"
    );
    assert_eq!(seen.lock().unwrap().len(), calls, "no announcement");
}
