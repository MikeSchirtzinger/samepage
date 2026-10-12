use super::*;
use crate::host::page_validation::Observation;
fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "review-app-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn state(root: &Path) -> Arc<SamePageState> {
    let (ws, _) = broadcast::channel(16);
    SamePageState::open_selected(
        root.join("document.json"),
        ws,
        Arc::new(|h| h.save().map(|_| ()).map_err(|e| e.to_string())),
        Arc::new(|_, _| {}),
        Duration::from_secs(5),
        Some(root),
        true,
        true,
    )
    .unwrap()
}
fn agent() -> SurfaceActor {
    let mut a = SurfaceActor::anonymous(ag_ui_surface::Caller::Agent);
    a.participant_id = Some("review-agent".into());
    a.label = Some("Review agent".into());
    a
}
fn place(s: &SamePageState, label: &str, x: f64, y: f64) -> String {
    let v: Value = serde_json::from_str(
        &s.invoke_as(&agent(), "card_place", json!({"label":label,"x":x,"y":y}))
            .unwrap(),
    )
    .unwrap();
    v["ids"]["card"].as_str().unwrap().into()
}
#[test]
fn page_validation_returns_post_write_errors_with_applied_ids() {
    let root = root();
    let s = state(&root);
    let first = place(&s, "first", 0., 0.);
    let second = place(&s, "second", 400., 0.);
    let error = s
        .invoke_as(&agent(), "card_place", json!({"id":second,"x":100}))
        .unwrap_err();
    let receipt: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(receipt["operation_applied"], true);
    assert_eq!(receipt["validation"]["status"], "failed");
    assert!(receipt["applied_ids"]
        .as_array()
        .unwrap()
        .contains(&json!(second)));
    assert!(error.contains(&first) && error.contains(&second));
    assert_eq!(receipt["errors"].as_array().unwrap().len(), 1);
    assert!(receipt["next"].as_str().unwrap().contains("do not repeat"));
    assert_eq!(s.host.view().records().count(), 2);
    // A write that touches neither overlapping card succeeds and carries the
    // standing overlap as a warning rather than failing.
    let unrelated = s
        .invoke_as(&agent(), "card_place", json!({"label":"far away","x":2000,"y":2000}))
        .unwrap();
    let reply: Value = serde_json::from_str(&unrelated).unwrap();
    assert_eq!(reply["warnings"][0]["code"], "card_overlap");
    assert!(reply["next"].as_str().unwrap().contains("before this write"));
    assert_eq!(s.host.view().records().count(), 3);
    s.invoke_as(&agent(), "card_place", json!({"id":second,"x":400}))
        .unwrap();
    assert_eq!(
        s.validation.report(&s.host.view()).unwrap()["status"],
        "pending"
    );
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn page_validation_late_measurement_wakes_own_writer_with_overlap() {
    let root = root();
    let s = state(&root);
    let first = place(&s, "growing", 0., 0.);
    let second = place(&s, "below", 0., 240.);
    let revision = s.host.revision();
    let before = s.host.encode_full();
    let generation = s.validation.generation();
    let waiting = s.validation.clone();
    let waiter = tokio::spawn(async move {
        waiting.wait_since(generation, 30).await;
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    s.validation
        .observe(
            "review-agent",
            Observation {
                stream: "tab-a".into(),
                revision,
                sequence: 1,
                bounds: [
                    (first, [0., 0., 232., 320.]),
                    (second, [0., 240., 232., 340.]),
                ]
                .into(),
                errors: vec![],
            },
            &s.host.view(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
    let report = s.validation.report(&s.host.view()).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(report.to_string().contains("card_overlap"));
    assert_eq!(before, s.host.encode_full());
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn browser_errors_are_fresh_evidence_and_do_not_change_document_or_read_cursor() {
    let root = root();
    let s = state(&root);
    let n = place(&s, "card", 0., 0.);
    let before = s.host.encode_full();
    let revision = s.host.revision();
    let bad: Observation = serde_json::from_value(
        json!({"stream":"tab-a","revision":"stale","sequence":1,"bounds":{},"errors":[]}),
    )
    .unwrap();
    assert!(s
        .validation
        .observe("browser", bad, &s.host.view())
        .is_err());
    let input:Observation=serde_json::from_value(json!({"stream":"tab-a","revision":revision,"sequence":2,"bounds":{n:[0,0,232,100]},"errors":[{"code":"browser_exception","message":"renderer failed"}]})).unwrap();
    s.validation
        .observe("browser", input, &s.host.view())
        .unwrap();
    assert!(s
        .validation
        .report(&s.host.view())
        .unwrap()
        .to_string()
        .contains("renderer failed"));
    assert_eq!(before, s.host.encode_full());
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn validation_wait_does_not_block_the_runtime_during_a_stalled_durable_operation() {
    let root = root();
    let s = state(&root);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let locked = s.clone();
    let lock_thread = std::thread::spawn(move || {
        let _guard = locked.durable_operation().unwrap();
        entered_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_millis(400));
    });
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let waiting = tokio::spawn(s.clone().validation_report_async());
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "durability wait blocked the async runtime"
    );
    assert!(!waiting.is_finished());
    release_tx.send(()).unwrap();
    waiting.await.unwrap().unwrap();
    lock_thread.join().unwrap();
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn same_person_tabs_keep_independent_fresh_error_evidence() {
    let root = root();
    let s = state(&root);
    let n = place(&s, "card", 0., 0.);
    let view = s.host.view();
    for (stream, sequence, errors) in [
        (
            "tab-a",
            100,
            json!([{"code":"renderer_failure","message":"tab A failed"}]),
        ),
        ("tab-b", 1, json!([])),
        (
            "tab-a",
            101,
            json!([{"code":"renderer_failure","message":"tab A still failed"}]),
        ),
    ] {
        let input=serde_json::from_value(json!({"stream":stream,"revision":view.revision(),"sequence":sequence,"bounds":{n.clone():[0,0,232,100]},"errors":errors})).unwrap();
        s.validation.observe("same-person", input, &view).unwrap();
    }
    let report = s.validation.report(&view).unwrap();
    assert_eq!(report["browser_peers"], 2);
    assert_eq!(report["status"], "failed");
    assert!(report.to_string().contains("tab A still failed"));
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
