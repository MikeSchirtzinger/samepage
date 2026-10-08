use super::*;

#[test]
fn selected_architecture_populates_the_review_slot_and_omission_leaves_it_null() {
    for with_architecture in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "samepage-architecture-report-{}-{}-{}",
            std::process::id(),
            with_architecture,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (ws_tx, _) = broadcast::channel(16);
        let state = SamePageState::open_selected_architecture(
            root.join("document.json"),
            ws_tx,
            Arc::new(|h| h.save().map(|_| ()).map_err(|e| e.to_string())),
            Arc::new(|_, _| {}),
            Duration::from_secs(1),
            Some(&project),
            false,
            true,
            with_architecture,
        )
        .unwrap();
        let mut actor = SurfaceActor::anonymous(ag_ui_surface::Caller::Agent);
        actor.participant_id = Some("architecture-report-agent".into());
        actor.label = Some("Architecture report agent".into());
        state
            .invoke_as(&actor, "card_place", json!({"label":"Host"}))
            .unwrap();
        if with_architecture {
            let baseline = samepage_architecture::Graph::capture(&state.host.view())
                .revision()
                .unwrap();
            state
                .invoke_as(
                    &actor,
                    "architecture_propose",
                    json!({"title":"Retained report proposal","baseline_revision":baseline}),
                )
                .unwrap();
        }
        let result: Value =
            serde_json::from_str(&state.invoke_as(&actor, "review_read", json!({})).unwrap())
                .unwrap();
        if with_architecture {
            assert_eq!(
                result["review"]["architecture"]["comparisons"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                result["review"]["architecture"],
                samepage_architecture::inspect(&state.host.view()).unwrap()
            );
        } else {
            assert!(result["review"]["architecture"].is_null());
        }
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
