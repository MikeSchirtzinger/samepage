mod common;

use common::{composition, view};
use samepage_capability::{audit, Delta, Record, View, Violation};

fn after(records: Vec<Record>) -> View {
    View::new("rev-2", records).expect("valid view")
}

/// The base document with `n1` replaced.
fn with_n1(n1: Record) -> View {
    let mut records: Vec<Record> = view().records().filter(|r| r.id != "n1").cloned().collect();
    records.push(n1);
    after(records)
}

fn base_n1() -> Record {
    view().get("n1").unwrap().clone()
}

#[test]
fn an_owned_change_passes_beside_untouched_unknown_extension_keys_and_kinds() {
    // n1 carries `state_variable` (no enabled writer) and `touched_by`
    // (host-written). Neither changes. The document also holds a whole
    // `agent-ink-variable` record no enabled capability owns.
    let moved = base_n1().with("x", 99.0);
    let delta = audit(&composition(), &view(), &with_n1(moved)).expect("owned change passes");
    assert_eq!(delta.modified.len(), 1);
    assert_eq!(
        delta.modified["n1"].iter().collect::<Vec<_>>(),
        vec!["x"],
        "only the owned key is in the delta"
    );
    assert!(delta.created.is_empty() && delta.deleted.is_empty());
}

#[test]
fn an_extension_key_change_by_its_writer_passes() {
    let bound = base_n1().with("path", "src/api.rs");
    audit(&composition(), &view(), &with_n1(bound)).expect("sources owns path");
}

#[test]
fn an_unchanged_frame_passes_with_an_empty_delta() {
    let delta = audit(&composition(), &view(), &view()).expect("nothing changed");
    assert!(delta.is_empty());
}

#[test]
fn creating_a_known_kind_with_claimed_keys_passes() {
    let mut records: Vec<Record> = view().records().cloned().collect();
    records.push(
        Record::new("n9", "node")
            .with("label", "New")
            .with("path", "a.rs"),
    );
    records.push(Record::new("n10", "node"));
    let delta = audit(&composition(), &view(), &after(records)).expect("create passes");
    assert_eq!(delta.created.len(), 2);
}

#[test]
fn deleting_a_known_record_that_carries_unknown_keys_passes() {
    let records: Vec<Record> = view().records().filter(|r| r.id != "n1").cloned().collect();
    let delta = audit(&composition(), &view(), &after(records)).expect("delete passes");
    assert!(delta.deleted.contains("n1"));
}

#[test]
fn a_replica_cannot_write_authorship() {
    let forged = base_n1().with("touched_by", "agent");
    let error = audit(&composition(), &view(), &with_n1(forged)).expect_err("forged author");
    assert_eq!(
        error.violations,
        vec![Violation::AuthorshipWritten {
            id: "n1".into(),
            key: "touched_by".into()
        }]
    );

    let mut records: Vec<Record> = view().records().cloned().collect();
    records.push(Record::new("n9", "node").with("created_by_participant", "p-1"));
    let error = audit(&composition(), &view(), &after(records)).expect_err("forged on create");
    assert!(
        matches!(&error.violations[0], Violation::AuthorshipWritten { key, .. } if key == "created_by_participant")
    );

    let mut stripped = base_n1();
    stripped.props.remove("touched_by");
    assert!(
        audit(&composition(), &view(), &with_n1(stripped)).is_err(),
        "removing authorship is writing it"
    );
}

#[test]
fn a_replica_cannot_write_reserved_keys() {
    let reserved = base_n1().with("_rev", 1.0);
    let error = audit(&composition(), &view(), &with_n1(reserved)).expect_err("reserved");
    assert!(matches!(&error.violations[0], Violation::ReservedKey { key, .. } if key == "_rev"));
}

#[test]
fn unclaimed_keys_cannot_be_added_changed_or_removed() {
    let added = base_n1().with("colour", "red");
    let error = audit(&composition(), &view(), &with_n1(added)).expect_err("added");
    assert!(matches!(&error.violations[0], Violation::UnclaimedKey { key, .. } if key == "colour"));

    let changed = base_n1().with("state_variable", "other");
    let error = audit(&composition(), &view(), &with_n1(changed)).expect_err("changed");
    assert!(
        matches!(&error.violations[0], Violation::UnclaimedKey { key, .. } if key == "state_variable")
    );

    let mut removed = base_n1();
    removed.props.remove("state_variable");
    let error = audit(&composition(), &view(), &with_n1(removed)).expect_err("removed");
    assert!(
        matches!(&error.violations[0], Violation::UnclaimedKey { key, .. } if key == "state_variable")
    );
}

#[test]
fn unknown_kinds_cannot_be_created_changed_or_deleted() {
    let mut created: Vec<Record> = view().records().cloned().collect();
    created.push(Record::new("s1", "segment-mask").with("runs", "AAAA"));
    assert!(matches!(
        &audit(&composition(), &view(), &after(created)).unwrap_err().violations[0],
        Violation::UnknownKindCreated { kind, .. } if kind == "segment-mask"
    ));

    let changed: Vec<Record> = view()
        .records()
        .map(|r| {
            if r.id == "v1" {
                r.clone().with("value", 4.0)
            } else {
                r.clone()
            }
        })
        .collect();
    assert!(matches!(
        &audit(&composition(), &view(), &after(changed)).unwrap_err().violations[0],
        Violation::UnknownKindModified { kind, .. } if kind == "agent-ink-variable"
    ));

    let deleted: Vec<Record> = view().records().filter(|r| r.id != "v1").cloned().collect();
    assert!(matches!(
        &audit(&composition(), &view(), &after(deleted))
            .unwrap_err()
            .violations[0],
        Violation::UnknownKindDeleted { .. }
    ));
}

#[test]
fn a_kind_cannot_change_under_the_same_id() {
    let mut rekinded = base_n1();
    rekinded.kind = "shape".into();
    rekinded.props.retain(|key, _| key == "x");
    let error = audit(&composition(), &view(), &with_n1(rekinded)).expect_err("rekinded");
    assert!(
        matches!(&error.violations[0], Violation::KindChanged { from, to, .. } if from == "node" && to == "shape")
    );
}

#[test]
fn every_violation_in_a_frame_is_reported() {
    let bad = base_n1().with("touched_by", "agent").with("colour", "red");
    let mut records: Vec<Record> = view().records().filter(|r| r.id != "n1").cloned().collect();
    records.push(bad);
    records.push(Record::new("s1", "segment-mask"));
    let error = audit(&composition(), &view(), &after(records)).expect_err("several");
    assert_eq!(error.violations.len(), 3, "{error}");
}

#[test]
fn delta_between_lists_created_modified_deleted_and_rekinded() {
    let before = View::new(
        "a",
        vec![
            Record::new("keep", "node").with("x", 1.0),
            Record::new("move", "node").with("x", 1.0),
            Record::new("gone", "node"),
            Record::new("swap", "node"),
        ],
    )
    .unwrap();
    let after = View::new(
        "b",
        vec![
            Record::new("keep", "node").with("x", 1.0),
            Record::new("move", "node").with("x", 2.0).with("y", 0.0),
            Record::new("swap", "edge"),
            Record::new("new", "edge"),
        ],
    )
    .unwrap();
    let delta = Delta::between(&before, &after);
    assert_eq!(delta.created.iter().collect::<Vec<_>>(), vec!["new"]);
    assert_eq!(delta.deleted.iter().collect::<Vec<_>>(), vec!["gone"]);
    assert_eq!(delta.rekinded.iter().collect::<Vec<_>>(), vec!["swap"]);
    assert_eq!(
        delta.modified["move"].iter().collect::<Vec<_>>(),
        vec!["x", "y"]
    );
    assert!(!delta.modified.contains_key("keep"));
}
