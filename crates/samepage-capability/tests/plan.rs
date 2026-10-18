mod common;

use std::collections::BTreeMap;

use common::{composition, counter, view};
use samepage_capability::{
    Batch, BatchBuilder, BatchEntry, Op, Plan, PlanError, ResolvedOp, Scalar, Target, Value,
};

fn props(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

fn create(reference: &str, kind: &str, pairs: &[(&str, Value)]) -> Op {
    Op::Create {
        reference: reference.into(),
        kind: kind.into(),
        props: props(pairs),
    }
}

fn set(target: Target, pairs: &[(&str, Value)]) -> Op {
    Op::Set {
        target,
        props: props(pairs),
    }
}

fn plan(ops: Vec<Op>) -> Plan {
    Plan {
        ops,
        readback: String::new(),
    }
}

fn batch(entries: Vec<(&str, Vec<Op>)>) -> Batch {
    Batch {
        base_revision: "rev-1".into(),
        entries: entries
            .into_iter()
            .map(|(capability, ops)| BatchEntry {
                capability: capability.into(),
                plan: plan(ops),
            })
            .collect(),
    }
}

fn resolve(entries: Vec<(&str, Vec<Op>)>) -> Result<samepage_capability::ResolvedBatch, PlanError> {
    batch(entries).resolve(&composition(), &view(), &mut counter())
}

#[test]
fn one_batch_links_records_created_by_different_capabilities() {
    let composition = composition();
    let base = view();
    let before = base.clone();
    let resolved = Batch {
        base_revision: "rev-1".into(),
        entries: vec![
            BatchEntry {
                capability: "map".into(),
                plan: Plan {
                    ops: vec![
                        create(
                            "card",
                            "node",
                            &[("label", "Queue".into()), ("x", 40.0.into())],
                        ),
                        create(
                            "link",
                            "edge",
                            &[("from", Value::reference("card")), ("to", "n2".into())],
                        ),
                    ],
                    readback: "Placed Queue.".into(),
                },
            },
            BatchEntry {
                capability: "sources".into(),
                plan: plan(vec![set(
                    Target::Ref("card".into()),
                    &[("path", "src/queue.rs".into())],
                )]),
            },
            BatchEntry {
                capability: "sketch".into(),
                plan: plan(vec![create(
                    "ring",
                    "shape",
                    &[("form", "ellipse".into()), ("on", Value::reference("card"))],
                )]),
            },
        ],
    }
    .resolve(&composition, &base, &mut counter())
    .expect("batch resolves");

    assert_eq!(base, before, "the caller's view is never modified");
    assert_eq!(resolved.ids()["card"], "id-0");
    assert_eq!(resolved.ids()["link"], "id-1");
    assert_eq!(resolved.ids()["ring"], "id-2");
    assert_eq!(resolved.readback(), ["Placed Queue.".to_string()]);

    let card = resolved.projected().get("id-0").expect("card projected");
    assert_eq!(card.kind, "node");
    assert_eq!(card.props["path"], Scalar::from("src/queue.rs"));
    assert_eq!(
        resolved.projected().get("id-1").unwrap().props["from"],
        Scalar::from("id-0"),
        "a reference becomes the minted id"
    );
    assert_eq!(
        resolved.projected().get("id-2").unwrap().props["on"],
        Scalar::from("id-0")
    );
    assert_eq!(resolved.projected().revision(), "rev-1");

    assert!(matches!(
        &resolved.ops()[2],
        ResolvedOp::Set { capability, id, .. } if capability == "sources" && id == "id-0"
    ));
    assert_eq!(resolved.ops().len(), 4);
    resolved.ensure_current("rev-1").expect("current");
    assert!(matches!(
        resolved.ensure_current("rev-2"),
        Err(PlanError::Stale { .. })
    ));
}

#[test]
fn builder_offers_the_batch_so_far_and_keeps_a_refused_plan_out() {
    let composition = composition();
    let base = view();
    let mut minter = counter();
    let mut builder = BatchBuilder::new(&composition, &base);
    builder
        .push(
            "map",
            &plan(vec![create("card", "node", &[("label", "Cache".into())])]),
            &mut minter,
        )
        .expect("map plan");
    assert!(builder.working_view().contains("id-0"));

    // The second op fails, so the first op of the same plan must not stick.
    let refused = plan(vec![
        set(Target::Ref("card".into()), &[("path", "a.rs".into())]),
        set(Target::Ref("card".into()), &[("label", "stolen".into())]),
    ]);
    let before = builder.working_view().clone();
    assert!(matches!(
        builder.push("sources", &refused, &mut minter),
        Err(PlanError::KeyNotWritable { key, .. }) if key == "label"
    ));
    assert_eq!(builder.working_view(), &before);
    assert_eq!(builder.ids().len(), 1);

    let done = builder.finish().expect("no dangling references");
    assert_eq!(done.ops().len(), 1);
    assert_eq!(base, view(), "base view untouched");
}

#[test]
fn a_failing_later_entry_fails_the_whole_batch() {
    let error = resolve(vec![
        (
            "map",
            vec![create("card", "node", &[("label", "A".into())])],
        ),
        (
            "sketch",
            vec![create(
                "s",
                "shape",
                &[("form", "box".into()), ("colour", "red".into())],
            )],
        ),
    ])
    .expect_err("second entry refused");
    assert!(matches!(error, PlanError::KeyNotWritable { key, .. } if key == "colour"));
}

#[test]
fn a_stale_batch_is_refused_before_anything_resolves() {
    let mut stale = batch(vec![("map", vec![create("c", "node", &[])])]);
    stale.base_revision = "rev-0".into();
    let base = view();
    assert!(matches!(
        stale.resolve(&composition(), &base, &mut counter()),
        Err(PlanError::Stale { expected, found }) if expected == "rev-0" && found == "rev-1"
    ));
    assert_eq!(base, view());
}

#[test]
fn minted_ids_must_be_new_and_non_empty() {
    let composition = composition();
    let base = view();
    let collide = batch(vec![("map", vec![create("c", "node", &[])])]);
    assert!(matches!(
        collide.resolve(&composition, &base, &mut || "n1".to_string()),
        Err(PlanError::DuplicateMintedId(id)) if id == "n1"
    ));
    let twice = batch(vec![(
        "map",
        vec![create("a", "node", &[]), create("b", "node", &[])],
    )]);
    assert!(matches!(
        twice.resolve(&composition, &base, &mut || "same".to_string()),
        Err(PlanError::DuplicateMintedId(id)) if id == "same"
    ));
    assert!(matches!(
        collide.resolve(&composition, &base, &mut String::new),
        Err(PlanError::EmptyMintedId)
    ));
    assert_eq!(base, view());
}

#[test]
fn set_and_delete_need_a_target_that_exists_at_that_point() {
    assert!(matches!(
        resolve(vec![("map", vec![set(Target::Id("ghost".into()), &[("label", "x".into())])])]),
        Err(PlanError::TargetMissing(id)) if id == "ghost"
    ));
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![Op::Delete {
                target: Target::Id("ghost".into())
            }]
        )]),
        Err(PlanError::TargetMissing(_))
    ));
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![
                Op::Delete { target: Target::Id("n2".into()) },
                set(Target::Id("n2".into()), &[("label", "back".into())]),
            ]
        )]),
        Err(PlanError::TargetMissing(id)) if id == "n2"
    ));
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![
                Op::Delete {
                    target: Target::Id("n2".into())
                },
                Op::Delete {
                    target: Target::Id("n2".into())
                },
            ]
        )]),
        Err(PlanError::TargetMissing(_))
    ));
}

#[test]
fn a_reference_must_be_created_earlier_and_only_once() {
    assert!(matches!(
        resolve(vec![("map", vec![set(Target::Ref("later".into()), &[("label", "x".into())])])]),
        Err(PlanError::UnknownRef(r)) if r == "later"
    ));
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![
                create("link", "edge", &[("from", Value::reference("card"))]),
                create("card", "node", &[]),
            ]
        )]),
        Err(PlanError::UnknownRef(r)) if r == "card"
    ));
    assert!(matches!(
        resolve(vec![("map", vec![create("a", "node", &[]), create("a", "node", &[])])]),
        Err(PlanError::DuplicateRef(r)) if r == "a"
    ));
    assert!(matches!(
        resolve(vec![("map", vec![create("", "node", &[])])]),
        Err(PlanError::EmptyRef)
    ));
    // A reference to a record deleted earlier in the batch would dangle.
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![
                create("card", "node", &[]),
                Op::Delete {
                    target: Target::Ref("card".into())
                },
                create("link", "edge", &[("from", Value::reference("card"))]),
            ]
        )]),
        Err(PlanError::TargetMissing(_))
    ));
}

#[test]
fn only_the_kind_owner_creates_or_deletes() {
    assert!(matches!(
        resolve(vec![("sketch", vec![create("n", "node", &[("label", "x".into())])])]),
        Err(PlanError::NotKindOwner { owner, .. }) if owner == "map"
    ));
    assert!(matches!(
        resolve(vec![(
            "sources",
            vec![Op::Delete {
                target: Target::Id("n1".into())
            }]
        )]),
        Err(PlanError::NotKindOwner { .. })
    ));
}

#[test]
fn a_known_kind_can_be_created_with_no_properties() {
    let resolved =
        resolve(vec![("map", vec![create("bare", "node", &[])])]).expect("structural create");
    let record = resolved.projected().get("id-0").unwrap();
    assert_eq!(record.kind, "node");
    assert!(record.props.is_empty());
}

#[test]
fn keys_are_written_only_by_their_one_writer() {
    assert!(matches!(
        resolve(vec![("map", vec![set(Target::Id("n1".into()), &[("path", "x".into())])])]),
        Err(PlanError::KeyNotWritable { capability, key, .. }) if capability == "map" && key == "path"
    ));
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![set(
                Target::Id("n1".into()),
                &[("state_variable", "y".into())]
            )]
        )]),
        Err(PlanError::KeyNotWritable { .. })
    ));
}

#[test]
fn structural_authorship_and_reserved_keys_are_never_planned() {
    for key in [
        "kind",
        "touched_by",
        "created_by",
        "created_by_participant",
        "touched_by_participant",
        "_x",
    ] {
        assert!(
            matches!(
                resolve(vec![("map", vec![set(Target::Id("n1".into()), &[(key, "x".into())])])]),
                Err(PlanError::ReservedKey { key: k }) if k == key
            ),
            "{key}"
        );
        assert!(matches!(
            resolve(vec![(
                "map",
                vec![create("c", "node", &[(key, "x".into())])]
            )]),
            Err(PlanError::ReservedKey { .. })
        ));
    }
}

#[test]
fn unknown_kinds_capabilities_empty_sets_and_bad_numbers_are_refused() {
    assert!(matches!(
        resolve(vec![("map", vec![create("c", "segment-mask", &[])])]),
        Err(PlanError::UnknownKind(kind)) if kind == "segment-mask"
    ));
    assert!(matches!(
        resolve(vec![("map", vec![set(Target::Id("v1".into()), &[("value", 4.0.into())])])]),
        Err(PlanError::UnknownKind(kind)) if kind == "agent-ink-variable"
    ));
    assert!(matches!(
        resolve(vec![("ghost", vec![])]),
        Err(PlanError::UnknownCapability(id)) if id == "ghost"
    ));
    assert!(matches!(
        resolve(vec![("map", vec![set(Target::Id("n1".into()), &[])])]),
        Err(PlanError::EmptySet(_))
    ));
    assert!(matches!(
        resolve(vec![(
            "map",
            vec![set(Target::Id("n1".into()), &[("x", f64::NAN.into())])]
        )]),
        Err(PlanError::NonFinite { .. })
    ));
}

#[test]
fn an_owner_delete_of_a_record_with_foreign_extension_keys_is_allowed() {
    let resolved = resolve(vec![(
        "map",
        vec![Op::Delete {
            target: Target::Id("n1".into()),
        }],
    )])
    .expect("owner deletes");
    assert!(!resolved.projected().contains("n1"));
    assert!(
        resolved.projected().contains("v1"),
        "unrelated unknown record kept"
    );
}

#[test]
fn a_reference_left_naming_a_record_deleted_later_fails_the_batch() {
    let composition = composition();
    let base = view();
    let error = batch(vec![(
        "map",
        vec![
            create("card", "node", &[]),
            create("link", "edge", &[("from", Value::reference("card"))]),
            Op::Delete {
                target: Target::Ref("card".into()),
            },
        ],
    )])
    .resolve(&composition, &base, &mut counter())
    .expect_err("dangling reference");
    assert_eq!(
        error,
        PlanError::DanglingRef {
            id: "id-1".into(),
            key: "from".into(),
            target: "id-0".into(),
            reference: "card".into(),
        }
    );
    assert_eq!(base, view(), "caller view untouched");

    // The same across capabilities: sketch points at map's card, then map
    // deletes the card in a later plan.
    let error = batch(vec![
        ("map", vec![create("card", "node", &[])]),
        (
            "sketch",
            vec![create("ring", "shape", &[("on", Value::reference("card"))])],
        ),
        (
            "map",
            vec![Op::Delete {
                target: Target::Ref("card".into()),
            }],
        ),
    ])
    .resolve(&composition, &base, &mut counter())
    .expect_err("dangling across capabilities");
    assert!(matches!(error, PlanError::DanglingRef { ref key, .. } if key == "on"));

    // A reference set later by Set, then its target deleted.
    let error = batch(vec![(
        "map",
        vec![
            create("card", "node", &[]),
            set(
                Target::Id("e1".into()),
                &[("from", Value::reference("card"))],
            ),
            Op::Delete {
                target: Target::Ref("card".into()),
            },
        ],
    )])
    .resolve(&composition, &base, &mut counter())
    .expect_err("dangling via set");
    assert!(matches!(error, PlanError::DanglingRef { ref id, .. } if id == "e1"));
}

#[test]
fn a_builder_that_would_dangle_refuses_to_finish() {
    let composition = composition();
    let base = view();
    let mut minter = counter();
    let mut builder = BatchBuilder::new(&composition, &base);
    builder
        .push(
            "map",
            &plan(vec![
                create("card", "node", &[]),
                create("link", "edge", &[("to", Value::reference("card"))]),
            ]),
            &mut minter,
        )
        .unwrap();
    builder
        .push(
            "map",
            &plan(vec![Op::Delete {
                target: Target::Ref("card".into()),
            }]),
            &mut minter,
        )
        .unwrap();
    assert!(matches!(
        builder.finish(),
        Err(PlanError::DanglingRef { .. })
    ));
}

#[test]
fn a_reference_that_no_longer_points_anywhere_passes() {
    // The referring record is deleted too.
    let resolved = resolve(vec![(
        "map",
        vec![
            create("card", "node", &[]),
            create("link", "edge", &[("from", Value::reference("card"))]),
            Op::Delete {
                target: Target::Ref("link".into()),
            },
            Op::Delete {
                target: Target::Ref("card".into()),
            },
        ],
    )])
    .expect("both deleted");
    assert!(!resolved.projected().contains("id-0") && !resolved.projected().contains("id-1"));

    // The referring key is overwritten with another value before the delete.
    let resolved = resolve(vec![(
        "map",
        vec![
            create("card", "node", &[]),
            create("link", "edge", &[("from", Value::reference("card"))]),
            set(Target::Ref("link".into()), &[("from", "n1".into())]),
            Op::Delete {
                target: Target::Ref("card".into()),
            },
        ],
    )])
    .expect("overwritten");
    assert_eq!(
        resolved.projected().get("id-1").unwrap().props["from"],
        Scalar::from("n1")
    );

    // Overwritten with a reference to a record that survives.
    let resolved = resolve(vec![(
        "map",
        vec![
            create("old", "node", &[]),
            create("new", "node", &[]),
            create("link", "edge", &[("from", Value::reference("old"))]),
            set(
                Target::Ref("link".into()),
                &[("from", Value::reference("new"))],
            ),
            Op::Delete {
                target: Target::Ref("old".into()),
            },
        ],
    )])
    .expect("re-pointed");
    assert_eq!(
        resolved.projected().get("id-2").unwrap().props["from"],
        Scalar::from("id-1")
    );
}

#[test]
fn an_id_from_the_base_view_is_never_reused_even_after_its_record_is_deleted() {
    let composition = composition();
    let base = view();
    let reuse = batch(vec![(
        "map",
        vec![
            Op::Delete {
                target: Target::Id("n1".into()),
            },
            create("again", "node", &[]),
        ],
    )]);
    assert_eq!(
        reuse.resolve(&composition, &base, &mut || "n1".to_string()),
        Err(PlanError::DuplicateMintedId("n1".into()))
    );
    // The same through the builder, across two pushes.
    let mut builder = BatchBuilder::new(&composition, &base);
    builder
        .push(
            "map",
            &plan(vec![Op::Delete {
                target: Target::Id("n2".into()),
            }]),
            &mut counter(),
        )
        .unwrap();
    assert!(!builder.working_view().contains("n2"));
    assert_eq!(
        builder.push("map", &plan(vec![create("c", "node", &[])]), &mut || "n2"
            .to_string()),
        Err(PlanError::DuplicateMintedId("n2".into()))
    );
    assert_eq!(base, view());
}
