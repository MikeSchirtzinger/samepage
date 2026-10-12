use super::*;
use crate::host::board::BoardHost;
fn state() -> (Arc<SamePageState>, Arc<BoardHost>, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "samepage-board-app-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let (tx, _) = broadcast::channel(32);
    let state = SamePageState::open_with_io(
        root.join("document.json"),
        tx,
        Arc::new(|h| h.save().map(|_| ()).map_err(|e| e.to_string())),
        Arc::new(|_, _| {}),
        Duration::from_secs(5),
    )
    .unwrap();
    let board = BoardHost::open(root.join("board.json"), Arc::new(|_, _| {})).unwrap();
    (state, board, root)
}
fn actor(caller: ag_ui_surface::Caller) -> SurfaceActor {
    let mut a = SurfaceActor::anonymous(caller);
    a.participant_id = Some(format!("board-{caller:?}"));
    a
}
async fn waiter(
    state: Arc<SamePageState>,
    board: Arc<BoardHost>,
) -> tokio::task::JoinHandle<Result<String, String>> {
    let def = crate::host::board_input::input_action(state, board);
    let effect = ag_ui_surface::with_actor(actor(ag_ui_surface::Caller::Agent), async {
        (def.apply)(&json!({"seconds":30}))
    })
    .await;
    match effect {
        SurfaceEffect::AsyncQuery(f) => tokio::spawn(f),
        _ => panic!("async query expected"),
    }
}
#[tokio::test]
async fn unified_wait_wakes_for_a_board_question() {
    let (s, b, _) = state();
    let w = waiter(s, b.clone()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!w.is_finished());
    b.invoke_as(
        &actor(ag_ui_surface::Caller::Human),
        "board_compose",
        json!({"kind":"question","text":"Which transition needs proof?"}),
        "revision",
    )
    .unwrap();
    let read = tokio::time::timeout(Duration::from_secs(3), w)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(read.contains("Which transition needs proof?") && read.contains("by you"));
}
#[test]
fn await_board_opts_into_the_runtime_chat_wake_path() {
    let (s, b, _) = state();
    let actions = b.actions(Arc::new(|| Ok("revision".into())));
    assert!(
        actions
            .iter()
            .find(|a| a.name == "await_board")
            .unwrap()
            .wake_on_chat
    );
    assert!(crate::host::board_input::input_action(s, b).wake_on_chat);
}
#[tokio::test]
async fn unified_wait_wakes_for_other_document_commits_but_not_own_writes() {
    let (s, b, _) = state();
    let own = actor(ag_ui_surface::Caller::Agent);
    s.invoke_as(&own, "card_place", json!({"label":"own card"}))
        .unwrap();
    let w = waiter(s.clone(), b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!w.is_finished());
    s.invoke_as(
        &actor(ag_ui_surface::Caller::Human),
        "card_place",
        json!({"label":"human card","x":450,"y":50}),
    )
    .unwrap();
    let read = tokio::time::timeout(Duration::from_secs(3), w)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(read.contains("human card"));
    assert!(
        read.contains("woken by: card_place node \"human card\"") && read.contains("by a person"),
        "the wake names what changed and who changed it: {read}"
    );
}
#[tokio::test]
async fn a_first_wait_waits_for_new_changes_instead_of_returning_history() {
    let (s, b, _) = state();
    s.invoke_as(
        &actor(ag_ui_surface::Caller::Human),
        "card_place",
        json!({"label":"old card","x":450,"y":50}),
    )
    .unwrap();
    let w = waiter(s.clone(), b).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!w.is_finished(), "history before the first wait must not wake it");
    s.invoke_as(
        &actor(ag_ui_surface::Caller::Human),
        "card_place",
        json!({"label":"new card","x":450,"y":300}),
    )
    .unwrap();
    let read = tokio::time::timeout(Duration::from_secs(3), w).await.unwrap().unwrap().unwrap();
    assert!(read.contains("woken by: card_place node \"new card\""), "{read}");
    assert!(!read.contains("woken by: card_place node \"old card\""), "{read}");
}
#[tokio::test]
async fn unified_wait_wakes_for_browser_validation_errors() {
    let (s, b, _) = state();
    let w = waiter(s.clone(), b).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!w.is_finished());
    let view = s.host.view();
    s.validation
        .observe(
            "person",
            crate::host::page_validation::Observation {
                stream: "proof".into(),
                revision: view.revision().into(),
                sequence: 1,
                bounds: BTreeMap::new(),
                errors: vec![crate::host::page_validation::BrowserError {
                    code: "renderer_failure".into(),
                    message: "board render exploded".into(),
                }],
            },
            &view,
        )
        .unwrap();
    let wake = tokio::time::timeout(Duration::from_secs(3), w)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(wake.contains("woken by: new page problems") && wake.contains("board render exploded"));
}
#[tokio::test]
async fn the_same_page_problems_wake_a_waiter_once_then_it_keeps_waiting() {
    let (s, b, _) = state();
    let observe = |sequence: u64| {
        let view = s.host.view();
        s.validation
            .observe(
                "person",
                crate::host::page_validation::Observation {
                    stream: "proof".into(),
                    revision: view.revision().into(),
                    sequence,
                    bounds: BTreeMap::new(),
                    errors: vec![crate::host::page_validation::BrowserError {
                        code: "renderer_failure".into(),
                        message: "board render exploded".into(),
                    }],
                },
                &view,
            )
            .unwrap();
    };
    observe(1);
    let first = tokio::time::timeout(Duration::from_secs(3), waiter(s.clone(), b.clone()).await)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(first.contains("board render exploded"));
    let second = waiter(s.clone(), b.clone()).await;
    observe(2);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!second.is_finished(), "the same problem must not wake the waiter again");
    s.invoke_as(
        &actor(ag_ui_surface::Caller::Human),
        "card_place",
        json!({"label":"human card","x":450,"y":50}),
    )
    .unwrap();
    let read = tokio::time::timeout(Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(read.contains("human card"), "a real change still wakes it: {read}");
}
#[test]
fn board_is_not_a_document_kind_and_aliases_share_the_native_definition() {
    let d = samepage_board::descriptor();
    assert!(d.kinds.is_empty() && d.props.is_empty() && d.depends_on.is_empty());
    let (_, b, _) = state();
    let actions = b.actions(Arc::new(|| Ok("revision".into())));
    let preview = actions
        .iter()
        .find(|a| a.name == "board_cement_preview")
        .unwrap();
    assert_eq!(preview.aliases, vec!["atlas_cement_propose"]);
    let cement = actions.iter().find(|a| a.name == "board_cement").unwrap();
    assert_eq!(cement.aliases, vec!["atlas_cement"]);
    assert_eq!(cement.audience, ActionAudience::Human);
}

#[tokio::test]
async fn unified_wait_wakes_for_human_attention_but_not_agent_pointing() {
    use ag_ui_surface::semantic_targets::{
        AttentionMode, Participant, SemanticTarget, SemanticTargetRef, SemanticTargetService,
    };
    let (s, b, _) = state();
    let attention = Arc::new(
        SemanticTargetService::with_resolver(
            ["board".to_string()],
            |target| {
                Ok(Some(SemanticTarget::new(
                    target.clone(),
                    "boundary",
                    "Board boundary claim",
                )))
            },
            |_| {},
        )
        .unwrap(),
    );
    s.input.bind_attention(attention.clone());
    let w = waiter(s, b).await;
    attention
        .attend(
            Participant::agent("agent-point", "Agent"),
            AttentionMode::Pointer,
            SemanticTargetRef::new("board", "boundary"),
            None,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!w.is_finished());
    attention
        .attend(
            Participant::human("human-point", "You"),
            AttentionMode::Selection,
            SemanticTargetRef::new("board", "boundary"),
            None,
        )
        .unwrap();
    let read = tokio::time::timeout(Duration::from_secs(3), w)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(read.contains("boundary") && read.contains("You"));
}

#[tokio::test]
async fn a_board_write_before_the_wait_future_is_polled_is_not_lost() {
    let (s, b, _) = state();
    let def = crate::host::board_input::input_action(s, b.clone());
    let effect = ag_ui_surface::with_actor(actor(ag_ui_surface::Caller::Agent), async {
        (def.apply)(&json!({"seconds":30}))
    })
    .await;
    b.invoke_as(
        &actor(ag_ui_surface::Caller::Human),
        "board_compose",
        json!({"kind":"question","text":"Already arrived before polling."}),
        "revision",
    )
    .unwrap();
    let SurfaceEffect::AsyncQuery(future) = effect else {
        panic!("async query expected")
    };
    let read = tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .unwrap()
        .unwrap();
    assert!(read.contains("Already arrived before polling."));
}
#[test]
fn an_agent_removes_a_persons_card_only_with_confirm_owner_and_hears_the_cascade() {
    let (s, _, _) = state();
    let mut person = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
    person.participant_id = Some("person-dana".into());
    person.label = Some("Dana".into());
    let agent = actor(ag_ui_surface::Caller::Agent);
    let placed: Value = serde_json::from_str(
        &s.invoke_as(&person, "card_place", json!({"label":"Human card","x":0,"y":0})).unwrap(),
    )
    .unwrap();
    let card = placed["ids"]["card"].as_str().unwrap().to_string();
    let other: Value = serde_json::from_str(
        &s.invoke_as(&agent, "card_place", json!({"label":"Agent card","x":400,"y":0})).unwrap(),
    )
    .unwrap();
    s.invoke_as(&agent, "card_link", json!({"from":other["ids"]["card"],"to":card,"label":"feeds"}))
        .unwrap();
    let refused = s.invoke_as(&agent, "object_remove", json!({"id":card})).unwrap_err();
    assert!(refused.contains("made by Dana") && refused.contains("confirm_owner"), "{refused}");
    assert!(s.host.view().get(&card).is_some());
    let removed = s
        .invoke_as(&agent, "object_remove", json!({"id":card,"confirm_owner":true}))
        .unwrap();
    assert!(removed.contains("REMOVED card \\\"Human card\\\""), "{removed}");
    assert!(removed.contains("link \\\"feeds\\\""), "{removed}");
    assert!(s.host.view().get(&card).is_none());
}
