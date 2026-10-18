//! ABI v2 in the kernel: one action plans one atomic batch across owners, the
//! planner's reach is what its descriptor declares, and a capability that only
//! reads a kind is still consulted about changes to it.

mod support;

use std::sync::{Arc, Mutex};

use samepage_capability::{
    ActionDecl, Artifact, Audience, Batch, BatchEntry, Call, CallerKind, CapError, Capability,
    Change, Composition, Delta, Descriptor, Effect, Op, Origin, Plan, Scalar, Target, Value, View,
};
use samepage_doc::host::{CommitError, FrameRefusal, Host, InvokeError, Verifier};
use samepage_doc::{Actor, Caller, sync};
use serde_json::json;
use support::*;
use yrs::{Map as _, Transact};

// ── a planner that composes another capability's records ────────────────

#[derive(Clone, Default)]
struct Typed {
    seen: Arc<Mutex<Vec<(String, Origin)>>>,
}

impl Typed {
    fn descriptor() -> Descriptor {
        Descriptor::new("typed", "0.1.0", Artifact::Wasm)
            .depends_on("cards", "0.1.0", "types the cards it draws")
            .extends("card", &["diagram_kind"], "typed cards")
            .reads("link")
            .action(ActionDecl::new(
                "diagram_draw",
                Audience::Both,
                Effect::Mutate,
                "Draw two typed cards and link them.",
            ))
            .action(ActionDecl::new(
                "diagram_read",
                Audience::Both,
                Effect::Query,
                "Describe the typed cards.",
            ))
            .action(ActionDecl::new(
                "diagram_sneak",
                Audience::Both,
                Effect::Query,
                "A query whose batch wrongly carries an op.",
            ))
    }

    fn cards_entry(ops: Vec<Op>, readback: &str) -> BatchEntry {
        BatchEntry {
            capability: "cards".into(),
            plan: Plan {
                ops,
                readback: readback.to_string(),
            },
        }
    }
}

fn create(reference: &str, kind: &str, props: Vec<(&str, Value)>) -> Op {
    Op::Create {
        reference: reference.into(),
        kind: kind.into(),
        props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn set_ref(reference: &str, props: Vec<(&str, Value)>) -> Op {
    Op::Set {
        target: Target::Ref(reference.into()),
        props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

impl Capability for Typed {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Err(CapError::new(
            "use_plan_batch",
            "typed diagrams are planned as a batch",
        ))
    }

    fn plan_batch(&self, call: &Call, view: &View) -> Result<Batch, CapError> {
        let entries = match call.action.as_str() {
            "diagram_draw" => vec![
                Typed::cards_entry(
                    vec![
                        create(
                            "s0",
                            "card",
                            vec![("label", "Idle".into()), ("x", 0.0.into())],
                        ),
                        create(
                            "s1",
                            "card",
                            vec![("label", "Running".into()), ("x", 300.0.into())],
                        ),
                        create(
                            "t0",
                            "link",
                            vec![
                                ("from", Value::reference("s0")),
                                ("to", Value::reference("s1")),
                            ],
                        ),
                    ],
                    "Placed two states and one transition.",
                ),
                BatchEntry {
                    capability: "typed".into(),
                    plan: Plan {
                        ops: vec![
                            set_ref("s0", vec![("diagram_kind", "state".into())]),
                            set_ref("s1", vec![("diagram_kind", "state".into())]),
                        ],
                        readback: "Typed them as states.".into(),
                    },
                },
            ],
            "diagram_read" => vec![BatchEntry {
                capability: "typed".into(),
                plan: Plan {
                    ops: vec![],
                    readback: format!("{} typed cards", view.of_kinds(&["card"]).len()),
                },
            }],
            "diagram_sneak" => vec![Typed::cards_entry(
                vec![create("x", "card", vec![("label", "Sneaked".into())])],
                "",
            )],
            "diagram_stale" => vec![BatchEntry {
                capability: "typed".into(),
                plan: Plan {
                    ops: vec![],
                    readback: String::new(),
                },
            }],
            other => return Err(CapError::new("unknown_action", other)),
        };
        let base_revision = if call.args["stale"].as_bool() == Some(true) {
            "0".repeat(64)
        } else {
            view.revision().to_string()
        };
        Ok(Batch {
            base_revision,
            entries,
        })
    }

    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        self.seen
            .lock()
            .unwrap()
            .push(("typed".to_string(), change.origin.clone()));
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

/// Plans an entry for a capability it does not declare.
struct Rogue;

impl Rogue {
    fn descriptor() -> Descriptor {
        Descriptor::new("rogue", "0.1.0", Artifact::Wasm)
            .owns("rogue-note", &["text"], "its own records")
            .action(ActionDecl::new(
                "rogue_write",
                Audience::Both,
                Effect::Mutate,
                "Reach sideways.",
            ))
    }
}

impl Capability for Rogue {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan::default())
    }
    fn plan_batch(&self, _call: &Call, view: &View) -> Result<Batch, CapError> {
        Ok(Batch {
            base_revision: view.revision().to_string(),
            entries: vec![BatchEntry {
                capability: "cards".into(),
                plan: Plan {
                    ops: vec![create("r", "card", vec![("label", "Not mine".into())])],
                    readback: String::new(),
                },
            }],
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

/// Reads cards, owns nothing on them, and refuses a label it dislikes. This is
/// the involvement rule: a reader is consulted and may refuse; it can never
/// write.
#[derive(Clone, Default)]
struct Watcher {
    seen: Arc<Mutex<Vec<(String, Origin, bool)>>>,
}

impl Watcher {
    fn descriptor() -> Descriptor {
        Descriptor::new("watcher", "0.1.0", Artifact::Wasm)
            .depends_on("cards", "0.1.0", "watches cards")
            .owns("watch-note", &["text"], "its own records")
            .reads("card")
            .reads("link")
    }
}

impl Capability for Watcher {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan::default())
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn validate_change(&self, change: &Change<'_>) -> Result<(), CapError> {
        let ids: Vec<&String> = change
            .attempted
            .keys()
            .chain(change.delta.modified.keys())
            .chain(change.delta.created.iter())
            .collect();
        self.seen.lock().unwrap().push((
            ids.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            change.origin.clone(),
            change.facts.is_some(),
        ));
        for id in ids {
            let record = change.after.get(id).or_else(|| change.before.get(id));
            let watched = record.is_some_and(|r| r.kind == "card" || r.kind == "link");
            if !watched {
                continue;
            }
            if change
                .after
                .get(id)
                .and_then(|r| r.props.get("label"))
                .and_then(|v| v.as_str())
                == Some("forbidden")
            {
                return Err(CapError::new(
                    "watched",
                    format!("{id}: the watcher refuses this label"),
                ));
            }
            if change.principal.caller == CallerKind::Agent
                && change
                    .delta
                    .modified
                    .get(id)
                    .is_some_and(|keys| keys.contains("to"))
            {
                return Err(CapError::new(
                    "watched",
                    format!("{id}: an agent may not re-point a link"),
                ));
            }
            if change
                .attempted
                .get(id)
                .is_some_and(|keys| keys.contains("parent"))
                && change.principal.caller == CallerKind::Agent
            {
                return Err(CapError::new(
                    "watched",
                    format!("{id}: an agent may not re-parent a card"),
                ));
            }
        }
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

struct NoFacts;

impl Verifier for NoFacts {
    fn verify(&self, _change: &Change<'_>) -> Result<serde_json::Value, CapError> {
        Ok(json!({ "for": "cards only" }))
    }
}

fn typed_host(typed: &Typed) -> Arc<Host> {
    let composition =
        Composition::new(vec![Cards::descriptor(), Typed::descriptor()]).expect("composition");
    Arc::new(Host::in_memory(composition, vec![Arc::new(Cards), Arc::new(typed.clone())]).unwrap())
}

fn watcher_host(watcher: &Watcher) -> Arc<Host> {
    let composition =
        Composition::new(vec![Cards::descriptor(), Watcher::descriptor()]).expect("composition");
    Arc::new(
        Host::in_memory(
            composition,
            vec![Arc::new(Cards), Arc::new(watcher.clone())],
        )
        .unwrap(),
    )
}

fn agent() -> Actor {
    Actor::new("p-agent", Caller::Agent).labelled("Model")
}

#[test]
fn one_action_creates_a_whole_scaffold_in_one_commit_attributed_to_the_planner() {
    let typed = Typed::default();
    let host = typed_host(&typed);
    let seen = record_commits(&host);

    let invoked = host.invoke(&agent(), "diagram_draw", json!({})).unwrap();
    assert_eq!(
        invoked.readback,
        [
            "Placed two states and one transition.",
            "Typed them as states."
        ]
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "one atomic commit");

    let view = host.view();
    let s0 = invoked.ids["s0"].clone();
    let s1 = invoked.ids["s1"].clone();
    let t0 = invoked.ids["t0"].clone();
    assert_eq!(view.get(&s0).unwrap().props["label"], Scalar::from("Idle"));
    assert_eq!(
        view.get(&s0).unwrap().props["diagram_kind"],
        Scalar::from("state")
    );
    assert_eq!(
        view.get(&s1).unwrap().props["diagram_kind"],
        Scalar::from("state")
    );
    let link = view.get(&t0).unwrap();
    assert_eq!(link.props["from"], Scalar::from(s0.as_str()));
    assert_eq!(link.props["to"], Scalar::from(s1.as_str()));
    // The host stamped every record, whoever's entry created it.
    assert_eq!(
        view.get(&t0).unwrap().props["created_by_participant"],
        Scalar::from("p-agent")
    );

    // Both owners were asked, and both were told the planner and the action.
    let origins = typed.seen.lock().unwrap().clone();
    assert!(!origins.is_empty());
    for (_, origin) in origins {
        assert_eq!(
            origin,
            Origin::HostAction {
                capability: "typed".into(),
                action: "diagram_draw".into()
            },
            "the origin names the planner, not the first entry's owner"
        );
    }
}

#[test]
fn a_query_carrying_an_op_is_refused_and_a_clean_query_still_speaks() {
    let typed = Typed::default();
    let host = typed_host(&typed);
    let before = fingerprint(&host);
    assert!(matches!(
        host.invoke(&agent(), "diagram_sneak", json!({})),
        Err(InvokeError::QueryMutates { ops: 1, .. })
    ));
    assert_eq!(fingerprint(&host), before);

    let read = host.invoke(&agent(), "diagram_read", json!({})).unwrap();
    assert_eq!(read.readback, ["0 typed cards"]);
    assert!(read.commit.is_none());
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_batch_planned_at_another_revision_is_refused_before_anything_is_written() {
    let typed = Typed::default();
    let host = typed_host(&typed);
    let before = fingerprint(&host);
    let result = host.invoke(&agent(), "diagram_draw", json!({ "stale": true }));
    assert!(
        matches!(&result, Err(InvokeError::PlannerBatch { capability, reason, .. })
            if capability == "typed" && reason.contains("base revision")),
        "{result:?}"
    );
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_planner_cannot_target_a_capability_it_does_not_declare() {
    let composition =
        Composition::new(vec![Cards::descriptor(), Rogue::descriptor()]).expect("composition");
    let host =
        Arc::new(Host::in_memory(composition, vec![Arc::new(Cards), Arc::new(Rogue)]).unwrap());
    let before = fingerprint(&host);
    let result = host.invoke(&agent(), "rogue_write", json!({}));
    assert!(
        matches!(&result, Err(InvokeError::PlannerBatch { reason, .. })
            if reason.contains("does not declare as a dependency")),
        "{result:?}"
    );
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn an_entry_label_does_not_grant_ownership_of_another_capabilitys_keys() {
    // The typed planner may write cards' records, but only keys cards owns;
    // its own extension key belongs on its own entry. Here it wrongly puts a
    // typed key in the cards entry.
    struct Mislabelled;
    impl Capability for Mislabelled {
        fn descriptor(&self) -> Descriptor {
            Typed::descriptor()
        }
        fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
            Ok(json!({}))
        }
        fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
            Ok(Plan::default())
        }
        fn plan_batch(&self, _call: &Call, view: &View) -> Result<Batch, CapError> {
            Ok(Batch {
                base_revision: view.revision().to_string(),
                entries: vec![Typed::cards_entry(
                    vec![create(
                        "s",
                        "card",
                        vec![("label", "State".into()), ("diagram_kind", "state".into())],
                    )],
                    "",
                )],
            })
        }
        fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
            Ok(())
        }
        fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
            Ok(json!({}))
        }
    }
    let composition =
        Composition::new(vec![Cards::descriptor(), Typed::descriptor()]).expect("composition");
    let host = Arc::new(
        Host::in_memory(composition, vec![Arc::new(Cards), Arc::new(Mislabelled)]).unwrap(),
    );
    let before = fingerprint(&host);
    let result = host.invoke(&agent(), "diagram_draw", json!({}));
    assert!(
        matches!(&result, Err(InvokeError::Commit(CommitError::Plan(
            samepage_capability::PlanError::KeyNotWritable { capability, key, .. }
        ))) if capability == "cards" && key == "diagram_kind"),
        "{result:?}"
    );
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_capability_that_only_reads_a_kind_is_consulted_on_every_route() {
    let watcher = Watcher::default();
    let host = watcher_host(&watcher);
    let card = place(&host, "API");
    let other = place(&host, "DB");

    // Host action.
    let before = fingerprint(&host);
    let result = host.invoke(&agent(), "card_place", json!({ "label": "forbidden" }));
    assert!(
        matches!(&result, Err(InvokeError::Commit(CommitError::Validate { capability, error }))
            if capability == "watcher" && error.code == "watched"),
        "{result:?}"
    );
    assert_eq!(fingerprint(&host), before);

    // Untrusted batch.
    let batch = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "cards".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(card.clone()),
                    props: [("label".to_string(), Value::from("forbidden"))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    let before = fingerprint(&host);
    assert!(matches!(
        host.commit_batch(&alice(), &batch),
        Err(CommitError::Validate { ref capability, .. }) if capability == "watcher"
    ));
    assert_eq!(fingerprint(&host), before);

    // Replica frame.
    let mut peer = Peer::of(&host);
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, card.as_str()) else {
            panic!()
        };
        record.insert(&mut txn, "label", "forbidden");
    }
    let frame = peer.frame();
    let before = fingerprint(&host);
    let refusal = host.receive_frame(&alice(), &frame).unwrap_err();
    assert!(
        matches!(refusal, FrameRefusal::Validate { ref capability, .. } if capability == "watcher"),
        "{refusal}"
    );
    assert_eq!(fingerprint(&host), before);
    let _ = other;
}

#[test]
fn a_reader_sees_same_value_writes_and_changed_endpoints_and_parents() {
    let watcher = Watcher::default();
    let host = watcher_host(&watcher);
    let a = place(&host, "A");
    let b = place(&host, "B");
    let c = place(&host, "C");
    let link = host
        .invoke(&agent(), "card_link", json!({ "from": a, "to": b }))
        .unwrap()
        .ids["link"]
        .clone();

    // A same-value write: the delta is empty, the reader is still consulted.
    watcher.seen.lock().unwrap().clear();
    let mut peer = Peer::of(&host);
    {
        let objects = peer.objects();
        let mut txn = peer.doc.transact_mut();
        let Some(yrs::Out::YMap(record)) = objects.get(&txn, a.as_str()) else {
            panic!()
        };
        record.insert(&mut txn, "label", "A");
    }
    host.receive_frame(&alice(), &peer.frame()).unwrap();
    let seen = watcher.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|(ids, origin, _)| ids.contains(&a) && origin == &Origin::ReplicaFrame),
        "the reader saw the same-value write: {seen:?}"
    );

    // A foreign endpoint move: the reader owns nothing on the link and refuses.
    let before = fingerprint(&host);
    let repoint = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "cards".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(link.clone()),
                    props: [("to".to_string(), Value::from(c.as_str()))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    assert!(matches!(
        host.commit_batch(&agent(), &repoint),
        Err(CommitError::Validate { ref capability, ref error })
            if capability == "watcher" && error.code == "watched"
    ));
    assert_eq!(fingerprint(&host), before);

    // A foreign parent move, refused the same way.
    let reparent = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "cards".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(b.clone()),
                    props: [("parent".to_string(), Value::from(c.as_str()))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    assert!(matches!(
        host.commit_batch(&agent(), &reparent),
        Err(CommitError::Validate { ref capability, .. }) if capability == "watcher"
    ));
    assert_eq!(fingerprint(&host), before);
}

#[test]
fn a_reader_never_receives_another_capabilitys_facts() {
    let watcher = Watcher::default();
    let composition =
        Composition::new(vec![Cards::descriptor(), Watcher::descriptor()]).expect("composition");
    let host = Arc::new(
        Host::in_memory(
            composition,
            vec![Arc::new(Cards), Arc::new(watcher.clone())],
        )
        .unwrap()
        .with_verifier("cards", Arc::new(NoFacts))
        .unwrap(),
    );
    place(&host, "API");
    let seen = watcher.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "the reader was consulted");
    assert!(
        seen.iter().all(|(_, _, had_facts)| !had_facts),
        "cards' verifier facts never reach the watcher: {seen:?}"
    );
}

#[test]
fn a_reader_cannot_write_the_kind_it_reads() {
    let watcher = Watcher::default();
    let host = watcher_host(&watcher);
    let card = place(&host, "API");
    let before = fingerprint(&host);
    let batch = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "watcher".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(card),
                    props: [("label".to_string(), Value::from("mine now"))].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    assert!(matches!(
        host.commit_batch(&agent(), &batch),
        Err(CommitError::Plan(
            samepage_capability::PlanError::KeyNotWritable { .. }
        ))
    ));
    assert_eq!(fingerprint(&host), before);
    let _ = sync::step1;
}

// ── origin and the empty batch ──────────────────────────────────────────

/// Plans one entry belonging to `cards`, and one action that plans nothing at
/// all.
struct Probe;

impl Probe {
    fn descriptor() -> Descriptor {
        Descriptor::new("probe", "0.1.0", Artifact::Wasm)
            .depends_on("cards", "0.1.0", "composes cards")
            .owns("probe-note", &["text"], "its own records")
            .action(ActionDecl::new(
                "probe_compose",
                Audience::Both,
                Effect::Mutate,
                "Plan one entry that belongs to cards.",
            ))
            .action(ActionDecl::new(
                "probe_nothing",
                Audience::Both,
                Effect::Mutate,
                "Plan a batch with no entries at all.",
            ))
    }
}

impl Capability for Probe {
    fn descriptor(&self) -> Descriptor {
        Self::descriptor()
    }
    fn project(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
    fn plan(&self, _call: &Call, _view: &View) -> Result<Plan, CapError> {
        Ok(Plan::default())
    }
    fn plan_batch(&self, call: &Call, view: &View) -> Result<Batch, CapError> {
        let entries = match call.action.as_str() {
            "probe_compose" => vec![Typed::cards_entry(
                vec![create("p", "card", vec![("label", "Composed".into())])],
                "Composed one card.",
            )],
            _ => vec![],
        };
        Ok(Batch {
            base_revision: view.revision().to_string(),
            entries,
        })
    }
    fn validate(&self, _delta: &Delta, _view: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn digest(&self, _view: &View) -> Result<serde_json::Value, CapError> {
        Ok(json!({}))
    }
}

fn probe_host(watcher: &Watcher) -> Arc<Host> {
    let composition = Composition::new(vec![
        Cards::descriptor(),
        Watcher::descriptor(),
        Probe::descriptor(),
    ])
    .expect("composition");
    Arc::new(
        Host::in_memory(
            composition,
            vec![Arc::new(Cards), Arc::new(watcher.clone()), Arc::new(Probe)],
        )
        .unwrap(),
    )
}

#[test]
fn the_origin_names_the_invoked_capability_not_the_first_entrys_owner() {
    let watcher = Watcher::default();
    let host = probe_host(&watcher);
    watcher.seen.lock().unwrap().clear();

    let invoked = host.invoke(&agent(), "probe_compose", json!({})).unwrap();
    assert_eq!(invoked.readback, ["Composed one card."]);
    assert!(host.view().get(&invoked.ids["p"]).is_some());

    let seen = watcher.seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "the reader was consulted");
    for (ids, origin, _) in seen {
        assert_eq!(
            origin,
            Origin::HostAction {
                capability: "probe".into(),
                action: "probe_compose".into()
            },
            "{ids}: the first entry belongs to cards, but probe was invoked"
        );
    }
}

#[test]
fn a_mutating_action_that_plans_no_entries_is_a_no_op_not_a_panic() {
    let watcher = Watcher::default();
    let host = probe_host(&watcher);
    let before = fingerprint(&host);
    let seen = record_commits(&host);

    let invoked = host.invoke(&agent(), "probe_nothing", json!({})).unwrap();
    assert!(invoked.commit.is_none(), "nothing was committed");
    assert!(invoked.readback.is_empty());
    assert!(invoked.ids.is_empty());
    assert_eq!(
        fingerprint(&host),
        before,
        "revision, state and journal unchanged"
    );
    assert!(seen.lock().unwrap().is_empty(), "no commit was announced");
}
