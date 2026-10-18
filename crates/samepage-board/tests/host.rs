use samepage_board::assertion::{RelationKind, catalog};
use samepage_board::*;
const NOW: &str = "2026-10-08T00:00:00Z";
trait TestClock {
    fn assert(&mut self, assertion: Assertion, author: Author) -> Result<String, String>;
    fn mark(&mut self, id: &str, mark: Mark, note: Option<String>) -> Result<String, String>;
}
impl TestClock for Board {
    fn assert(&mut self, a: Assertion, author: Author) -> Result<String, String> {
        self.assert_at(a, author, NOW.into())
    }
    fn mark(&mut self, id: &str, m: Mark, n: Option<String>) -> Result<String, String> {
        self.mark_at(id, m, n, NOW.into())
    }
}

fn claim(id: &str) -> Assertion {
    Assertion::Claim {
        id: id.to_string(),
        text: "text".to_string(),
        status: Status::Proposed,
    }
}

#[test]
fn catalog_covers_every_variant() {
    let catalog = catalog();
    let described: Vec<&str> = catalog["assertions"]
        .as_array()
        .expect("assertions is an array")
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind is a string"))
        .collect();
    // Every variant the board can hold must appear in the catalog, or an
    // agent reading /primitives would not know it exists.
    for kind in [
        "claim", "question", "choice", "relation", "group", "evidence", "decision",
    ] {
        assert!(described.contains(&kind), "catalog is missing `{kind}`");
    }
    assert_eq!(
        described.len(),
        7,
        "catalog describes a kind the board cannot hold"
    );
}

#[test]
fn a_relation_to_nothing_is_refused() {
    let mut board = Board::default();
    board.assert(claim("a"), Author::Agent).expect("claim a");
    let refused = board.assert(
        Assertion::Relation {
            id: "r".to_string(),
            from: "a".to_string(),
            to: "ghost".to_string(),
            how: RelationKind::Supports,
        },
        Author::Agent,
    );
    assert!(refused.is_err(), "a dangling relation must be refused");
}

#[test]
fn a_mark_survives_the_agent_rewriting_underneath_it() {
    let mut board = Board::default();
    board.assert(claim("a"), Author::Agent).expect("claim a");
    board
        .mark("a", Mark::Question, Some("why?".to_string()))
        .expect("mark a");
    board
        .assert(
            Assertion::Claim {
                id: "a".to_string(),
                text: "a better answer".to_string(),
                status: Status::Supported,
            },
            Author::Agent,
        )
        .expect("rewrite a");
    let entry = board.get("a").expect("a is still there");
    assert_eq!(
        entry.mark.as_ref().map(|(mark, _)| *mark),
        Some(Mark::Question),
        "the question was the human's to close"
    );
}

#[test]
fn only_a_human_agreeing_settles_a_claim() {
    let mut board = Board::default();
    board.assert(claim("a"), Author::Agent).expect("claim a");
    // The agent cannot write Settled directly...
    board
        .assert(
            Assertion::Claim {
                id: "a".to_string(),
                text: "text".to_string(),
                status: Status::Supported,
            },
            Author::Agent,
        )
        .expect("agent supports");
    assert!(matches!(
        board.get("a").expect("a").assertion,
        Assertion::Claim {
            status: Status::Supported,
            ..
        }
    ));
    board.mark("a", Mark::Agree, None).expect("human agrees");
    assert!(matches!(
        board.get("a").expect("a").assertion,
        Assertion::Claim {
            status: Status::Settled,
            ..
        }
    ));
}

#[test]
fn the_delta_is_session_state_not_document_state() {
    let mut board = Board::default();
    board.assert(claim("a"), Author::Agent).expect("claim a");
    let first = board.read();
    assert!(first.contains("written: a"), "first read reports the write");
    let second = board.read();
    assert!(
        second.contains("nothing changed"),
        "a second read with no writes between must be empty"
    );
    // The document did not change, but a different audience still has
    // its own unseen set.
    assert!(
        !board.human_delta().is_empty(),
        "the human has not looked yet"
    );
}

/// `Settled` is not a status the agent may write — it is the shadow of a
/// human `✓`. So taking the `✓` back has to take the settlement with it,
/// or the board would show a claim as agreed with nobody's agreement
/// behind it, which is the exact authority this vocabulary exists to deny.
#[test]
fn unmarking_an_agreement_unsettles_the_claim() {
    let mut board = Board::default();
    board.assert(claim("a"), Author::Agent).expect("claim a");
    board.mark("a", Mark::Agree, None).expect("human agrees");
    board.unmark("a").expect("human takes it back");
    let entry = board.get("a").expect("a is still there");
    assert!(entry.mark.is_none(), "the mark is gone");
    assert!(
        !matches!(
            entry.assertion,
            Assertion::Claim {
                status: Status::Settled,
                ..
            }
        ),
        "a claim cannot stay settled once the agreement that settled it is withdrawn"
    );
}

#[test]
fn authorship_and_human_signoff_have_distinct_timestamps_and_revisions() {
    let mut board = Board::default();
    board
        .assert_at(
            claim("a"),
            Author::Named("reviewer".to_string()),
            "2026-07-31T10:00:00Z".to_string(),
        )
        .expect("claim a");
    board
        .mark_at("a", Mark::Agree, None, "2026-07-31T10:01:00Z".to_string())
        .expect("human agrees");

    let entry = board.get("a").expect("a");
    assert_eq!(entry.authored_at.as_deref(), Some("2026-07-31T10:00:00Z"));
    assert_eq!(entry.authored_at_revision, Some(1));
    assert_eq!(entry.marked_by, Some(Author::You));
    assert_eq!(entry.marked_at.as_deref(), Some("2026-07-31T10:01:00Z"));
    assert_eq!(entry.marked_at_revision, Some(2));
}

#[test]
fn rewriting_after_signoff_makes_the_signoff_revision_stale() {
    let mut board = Board::default();
    board
        .assert_at(
            claim("a"),
            Author::Agent,
            "2026-07-31T10:00:00Z".to_string(),
        )
        .expect("claim a");
    board
        .mark_at("a", Mark::Agree, None, "2026-07-31T10:01:00Z".to_string())
        .expect("human agrees");
    board
        .assert_at(
            Assertion::Claim {
                id: "a".to_string(),
                text: "rewritten after approval".to_string(),
                status: Status::Supported,
            },
            Author::Agent,
            "2026-07-31T10:02:00Z".to_string(),
        )
        .expect("rewrite a");

    let entry = board.get("a").expect("a");
    assert_eq!(
        entry.mark.as_ref().map(|(mark, _)| *mark),
        Some(Mark::Agree)
    );
    assert!(
        entry.marked_at_revision < entry.authored_at_revision,
        "the mark stays visible, but its older revision cannot cement newer words"
    );
}

#[test]
fn retracting_takes_dangling_relations_with_it() {
    let mut board = Board::default();
    board.assert(claim("a"), Author::Agent).expect("claim a");
    board.assert(claim("b"), Author::Agent).expect("claim b");
    board
        .assert(
            Assertion::Relation {
                id: "r".to_string(),
                from: "a".to_string(),
                to: "b".to_string(),
                how: RelationKind::Supports,
            },
            Author::Agent,
        )
        .expect("relation");
    board.retract("b").expect("retract b");
    assert!(board.get("r").is_none(), "the relation pointed at nothing");
}

#[allow(dead_code)]
#[path = "../../../apps/samepage/src/host/board.rs"]
mod native;
use ag_ui_surface::{Actor, Caller};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
fn dir(name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
        "samepage-board-{}-{name}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn agent() -> Actor {
    let mut a = Actor::anonymous(Caller::Agent);
    a.participant_id = Some("agent-board-test".into());
    a.label = Some("Board reviewer".into());
    a
}
fn human() -> Actor {
    let mut a = Actor::anonymous(Caller::Human);
    a.participant_id = Some("person-board-test".into());
    a
}
fn host(root: &std::path::Path) -> Arc<native::BoardHost> {
    native::BoardHost::open(root.join("board.json"), Arc::new(|_, _| {})).unwrap()
}
fn write_claim(h: &native::BoardHost, id: &str) -> Result<String, String> {
    h.invoke_as(
        &agent(),
        "board_assert",
        json!({"assertions":[{"kind":"claim","id":id,"text":"the port is done"}]}),
        "doc-revision",
    )
}
#[test]
fn the_agent_cannot_reach_the_humans_half_and_the_reverse() {
    let d = dir("audience");
    let h = host(&d);
    for action in samepage_board::descriptor().actions {
        let wrong = if action.audience == samepage_capability::Audience::Agent {
            human()
        } else {
            agent()
        };
        assert!(
            h.invoke_as(&wrong, &action.name, json!({}), "revision")
                .unwrap_err()
                .contains("cannot invoke"),
            "{}",
            action.name
        );
    }
    write_claim(&h, "a").unwrap();
    assert!(
        h.invoke_as(
            &agent(),
            "board_mark",
            json!({"id":"a","mark":"agree"}),
            "revision"
        )
        .is_err()
    );
    let board = h.snapshot();
    assert_eq!(
        board["board"]["entries"][0]["author"],
        json!({"named":"Board reviewer"})
    );
    assert_eq!(
        board["board"]["entries"][0]["author_participant"],
        "agent-board-test"
    );
    assert!(board["board"]["entries"][0]["mark"].is_null());
}
#[test]
fn a_board_json_written_before_named_agents_still_loads() {
    let d = dir("legacy");
    std::fs::write(
        d.join("board.json"),
        include_str!("fixtures/old-board.json"),
    )
    .unwrap();
    let h = host(&d);
    let read = h.read("legacy-reader").unwrap();
    assert!(read.contains("[old-agent, by agent]"));
    assert!(read.contains("[old-you, by you]"));
    assert!(
        h.snapshot()["board"]["entries"][0]["authored_at"].is_null(),
        "migration must never invent provenance"
    );
}
#[test]
fn the_board_survives_a_restart_but_the_delta_does_not() {
    let d = dir("restart");
    let h = host(&d);
    write_claim(&h, "a").unwrap();
    assert!(h.read("reader").unwrap().contains("written: a"));
    assert!(h.read("reader").unwrap().contains("nothing changed"));
    let saved = std::fs::read_to_string(d.join("board.json")).unwrap();
    assert!(!saved.contains("agent_saw") && !saved.contains("humans") && !saved.contains("agents"));
    drop(h);
    let h = host(&d);
    let read = h.read("reader").unwrap();
    assert!(read.contains("the port is done") && read.contains("written: a"));
    assert!(h.read("other-reader").unwrap().contains("written: a"));
}
#[test]
fn a_batch_that_fails_midway_still_publishes_what_landed() {
    let d = dir("partial");
    let events = Arc::new(Mutex::new(Vec::new()));
    let published = events.clone();
    let h = native::BoardHost::open(
        d.join("board.json"),
        Arc::new(move |name, value| published.lock().unwrap().push((name.to_string(), value))),
    )
    .unwrap();
    let error = h
        .invoke_as(
            &agent(),
            "board_assert",
            json!({"assertions":[
                {"kind":"claim","id":"a","text":"this one is fine"},
                {"kind":"question","id":"b","text":"this is fine too"},
                {"kind":"relation","id":"third","from":"a","to":"ghost","how":"supports"}
            ]}),
            "revision",
        )
        .unwrap_err();
    assert!(
        error.contains("third")
            && error.contains("ghost")
            && error.contains("Already written: added a, added b"),
        "{error}"
    );
    assert_eq!(h.revision(), 2);
    assert_eq!(events.lock().unwrap().len(), 1);
    assert_eq!(events.lock().unwrap()[0].0, "board.changed");
    assert_eq!(
        events.lock().unwrap()[0].1["board"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        host(&d).snapshot()["board"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[test]
fn malformed_json_refuses_before_the_successful_prefix() {
    let d = dir("malformed");
    let h = host(&d);
    assert!(h.invoke_as(&agent(),"board_assert",json!({"assertions":[{"kind":"claim","id":"a","text":"fine"},{"kind":"unrecognized","id":"b"}]}),"revision").is_err());
    assert_eq!(h.revision(), 0);
    assert!(!d.join("board.json").exists());
}
#[test]
fn the_agent_can_preview_cement_but_only_the_human_action_writes_it() {
    let d = dir("advisory");
    let h = host(&d);
    write_claim(&h, "settled").unwrap();
    h.invoke_as(
        &human(),
        "board_mark",
        json!({"id":"settled","mark":"agree"}),
        "revision",
    )
    .unwrap();
    let preview = h
        .invoke_as(&agent(), "atlas_cement_propose", json!({}), "revision")
        .unwrap();
    let preview: Value = serde_json::from_str(&preview).unwrap();
    assert_eq!(
        preview["writes"]["obligations-v0.1.json"]["meta"]["advisory_only"],
        true
    );
    assert_eq!(
        preview["writes"]["obligations-v0.1.json"]["obligations"][0]["signal"]["gate"],
        "none"
    );
    let target = d.join("cemented");
    assert!(!target.exists());
    assert!(
        h.invoke_as(
            &agent(),
            "atlas_cement",
            json!({"directory":target}),
            "revision"
        )
        .is_err()
    );
    h.invoke_as(
        &human(),
        "board_cement",
        json!({"directory":target}),
        "revision",
    )
    .unwrap();
    assert!(
        target.join("obligations-v0.1.json").is_file()
            && target.join("cement-receipt.json").is_file()
    );
    assert!(
        h.invoke_as(
            &human(),
            "board_cement",
            json!({"directory":target}),
            "revision"
        )
        .is_err()
    );
}
#[test]
fn save_failure_refuses_publication_and_does_not_change_memory() {
    let d = dir("save-failure");
    let h = host(&d);
    std::fs::create_dir(d.join("board.json")).unwrap();
    assert!(
        write_claim(&h, "a")
            .unwrap_err()
            .contains("persistence failed")
    );
    assert_eq!(h.revision(), 0);
}
#[test]
fn direct_settlement_and_nonhuman_transition_context_are_refused() {
    let board = Board::default();
    let ctx = Context {
        author: Author::Agent,
        participant: "agent".into(),
        timestamp: NOW.into(),
    };
    let outcome=samepage_board::transition_at(&board,&samepage_capability::Call{action:"board_assert".into(),args:json!({"assertions":[{"kind":"claim","id":"a","text":"settle myself","status":"settled"}]})},&ctx).unwrap();
    assert!(outcome.error.unwrap().contains("only a human"));
    assert_eq!(outcome.board.revision(), 0);
    let e = samepage_board::transition_at(
        &board,
        &samepage_capability::Call {
            action: "board_mark".into(),
            args: json!({"id":"a","mark":"agree"}),
        },
        &ctx,
    )
    .unwrap_err();
    assert_eq!(e.code, "human_context_required");
}
#[test]
fn replacing_an_agreement_and_transitive_retraction_keep_the_board_honest() {
    let mut b = Board::default();
    b.assert(claim("a"), Author::Agent).unwrap();
    b.mark("a", Mark::Agree, None).unwrap();
    b.mark("a", Mark::Disagree, None).unwrap();
    assert!(!matches!(
        b.get("a").unwrap().assertion,
        Assertion::Claim {
            status: Status::Settled,
            ..
        }
    ));
    b.assert(
        Assertion::Evidence {
            id: "e".into(),
            about: "a".into(),
            source: "source".into(),
            verdict: Default::default(),
        },
        Author::Agent,
    )
    .unwrap();
    b.assert(
        Assertion::Group {
            id: "g".into(),
            label: "group".into(),
            members: vec!["e".into()],
        },
        Author::Agent,
    )
    .unwrap();
    b.retract("a").unwrap();
    assert!(b.entries().is_empty());
}
#[test]
fn advisory_golden_bytes_and_old_missing_provenance_are_preserved() {
    let board: Board = serde_json::from_str(include_str!("fixtures/settled-board.json")).unwrap();
    let bundle =
        samepage_board::advisory::propose_at(&board, "AQIDBA", "2026-07-31T12:00:00Z").unwrap();
    assert_eq!(
        bundle.files()[0].1,
        include_bytes!("fixtures/obligations-v0.1.json")
    );
    assert_eq!(
        bundle.files()[1].1,
        include_bytes!("fixtures/cement-receipt.json")
    );
    let old: Board = serde_json::from_str(include_str!("fixtures/old-board.json")).unwrap();
    assert!(
        samepage_board::advisory::propose_at(&old, "revision", NOW)
            .unwrap_err()
            .contains("missing authorship timestamp")
    );
}
