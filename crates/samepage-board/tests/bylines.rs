use samepage_board::{
    Assertion, Author, Board, BoardCapability, Context, Mark, fresh_agreement, project_for,
};
use samepage_capability::{Call, Capability};
use serde_json::{Value, json};
use std::sync::Arc;

const NOW: &str = "2026-10-08T12:00:00Z";
fn marked(mark: Mark) -> Board {
    let mut b = Board::default();
    b.bind_participant("human-a", "Alice");
    b.bind_participant("human-b", "Blair");
    b.assert_at(
        Assertion::Claim {
            id: "claim".into(),
            text: "Shared claim".into(),
            status: Default::default(),
        },
        Author::Agent,
        NOW.into(),
    )
    .unwrap();
    for (participant, mark) in [("human-a", Mark::Agree), ("human-b", mark)] {
        b = samepage_board::transition_at(
            &b,
            &Call {
                action: "board_mark".into(),
                args: json!({"id":"claim","mark":mark.word()}),
            },
            &Context {
                author: Author::You,
                participant: participant.into(),
                timestamp: NOW.into(),
            },
        )
        .unwrap()
        .board;
    }
    b
}

#[test]
fn a_second_humans_mark_is_named_to_the_first_and_you_to_the_second() {
    let b = marked(Mark::Question);
    let before = serde_json::to_value(&b).unwrap();
    for (reader, marker) in [("human-a", "Blair"), ("human-b", "you"), ("agent", "Blair")] {
        assert!(
            b.render_for(0, Some(reader))
                .contains(&format!("marked question by {marker}"))
        );
        assert!(
            b.render_for(0, Some(reader))
                .contains(&format!("marked by {marker}: claim"))
        );
        assert_eq!(
            project_for(&b, 0, Some(reader))["entries"][0]["mark"]["by"],
            marker
        );
        let view = samepage_board::snapshot_view_for(&b, Some(reader));
        assert_eq!(
            BoardCapability.project(&view).unwrap()["entries"][0]["mark"]["by"],
            marker
        );
        assert!(
            BoardCapability
                .plan(
                    &Call {
                        action: "board_read".into(),
                        args: json!({})
                    },
                    &view
                )
                .unwrap()
                .readback
                .contains(&format!("marked question by {marker}"))
        );
        assert_eq!(
            BoardCapability
                .changes(
                    &json!({"board":Board::default()}),
                    &json!({"board":b}),
                    reader
                )
                .unwrap(),
            vec![format!("marked by {marker}: claim")]
        );
    }
    assert_eq!(
        serde_json::to_value(&b).unwrap(),
        before,
        "projection must not mutate the sign-off"
    );
    assert_eq!(b.get("claim").unwrap().marked_by, Some(Author::You));
    assert_eq!(
        b.get("claim").unwrap().marked_by_participant.as_deref(),
        Some("human-b")
    );
}

#[test]
fn signoff_names_are_viewer_relative_without_changing_freshness() {
    let mut b = marked(Mark::Agree);
    assert!(fresh_agreement(b.get("claim").unwrap()));
    assert!(
        b.render_for(0, Some("human-a"))
            .contains("marked agree by Blair")
    );
    assert!(
        b.render_for(0, Some("human-b"))
            .contains("marked agree by you")
    );
    b.assert_at(
        Assertion::Claim {
            id: "claim".into(),
            text: "Changed words".into(),
            status: Default::default(),
        },
        Author::Agent,
        NOW.into(),
    )
    .unwrap();
    assert!(!fresh_agreement(b.get("claim").unwrap()));
    assert!(
        b.render_for(0, Some("human-a"))
            .contains("marked agree by Blair")
    );
    assert!(
        b.render_for(0, Some("human-b"))
            .contains("STALE: human sign-off")
    );
    b.unmark("claim").unwrap();
    assert!(project_for(&b, 0, Some("human-b"))["entries"][0]["mark"].is_null());
    assert!(!b.render_for(0, Some("human-b")).contains("marked agree"));
}

#[test]
fn matching_display_names_never_make_another_marker_you() {
    let mut b = marked(Mark::Question);
    b.bind_participant("human-a", "Blair");
    assert_eq!(
        project_for(&b, 0, Some("human-a"))["entries"][0]["mark"]["by"],
        "Blair"
    );
    assert_eq!(
        project_for(&b, 0, Some("human-b"))["entries"][0]["mark"]["by"],
        "you"
    );
}

#[test]
fn readback_never_names_a_marker_that_is_not_stored() {
    let mut value = serde_json::to_value(marked(Mark::Question)).unwrap();
    for participant in [Value::Null, json!(""), json!("unknown-human")] {
        value["entries"][0]["marked_by_participant"] = participant;
        let b: Board = serde_json::from_value(value.clone()).unwrap();
        for reader in [None, Some("human-a"), Some("human-b")] {
            let read = b.render_for(0, reader);
            assert!(read.contains("marked question (marker identity unavailable)"));
            assert!(
                !read.contains("by you")
                    && !read.contains("by Alice")
                    && !read.contains("by Blair")
            );
            assert!(project_for(&b, 0, reader)["entries"][0]["mark"]["by"].is_null());
        }
    }
    let mut b = marked(Mark::Question);
    b.mark_at("claim", Mark::Important, None, NOW.into())
        .unwrap();
    assert!(
        b.get("claim").unwrap().marked_by_participant.is_none(),
        "a new unbound mark cannot inherit Blair"
    );
    assert!(!b.render_for(0, Some("human-a")).contains("Blair"));
}

#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/board.rs"]
mod native;
use ag_ui_surface::{Actor, Caller};
fn actor(id: &str, name: &str, caller: Caller) -> Actor {
    let mut a = Actor::anonymous(caller);
    a.participant_id = Some(id.into());
    a.label = Some(name.into());
    a
}

#[test]
fn the_native_host_persists_authenticated_names_and_reads_for_each_participant() {
    let dir = std::env::temp_dir().join(format!("board-bylines-{}", std::process::id()));
    let h = native::BoardHost::open(dir.join("board.json"), Arc::new(|_, _| {})).unwrap();
    let a = actor("human-a", "Alice", Caller::Human);
    let b = actor("human-b", "Blair", Caller::Human);
    let agent = actor("agent", "Reviewer", Caller::Agent);
    h.invoke_as(
        &agent,
        "board_assert",
        json!({"assertions":[{"kind":"claim","id":"claim","text":"Shared claim"}]}),
        "doc",
    )
    .unwrap();
    h.invoke_as(
        &a,
        "board_mark",
        json!({"id":"claim","mark":"agree"}),
        "doc",
    )
    .unwrap();
    h.invoke_as(
        &b,
        "board_mark",
        json!({"id":"claim","mark":"question"}),
        "doc",
    )
    .unwrap();
    for reader in [&a, &b] {
        let snapshot: Value = serde_json::from_str(
            &h.invoke_as(reader, "board_snapshot", json!({}), "doc")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            snapshot["readback"].as_str().unwrap(),
            h.read(reader.participant_id.as_deref().unwrap()).unwrap()
        );
    }
    assert!(
        h.read("human-a")
            .unwrap()
            .contains("marked question by Blair")
    );
    assert!(
        h.read("human-b")
            .unwrap()
            .contains("marked question by you")
    );
    assert!(
        h.invoke_as(&agent, "board_read", json!({}), "doc")
            .unwrap()
            .contains("marked question by Blair")
    );
    let saved = h.snapshot();
    assert_eq!(saved["board"]["participants"]["human-b"], "Blair");
    drop(h);
    let h = native::BoardHost::open(dir.join("board.json"), Arc::new(|_, _| {})).unwrap();
    assert!(h.peek().contains("marked question by Blair"));
    assert!(
        h.read("human-b")
            .unwrap()
            .contains("marked question by you")
    );
    assert_eq!(h.snapshot(), saved);
    std::fs::remove_dir_all(dir).unwrap();
}
