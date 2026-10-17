use samepage_capability::{
    Batch, BatchEntry, Call, Capability, Composition, Op, Plan, Target, Value as PlanValue,
};
use samepage_doc::{Actor, Caller, Replica, host::Host};
use samepage_explain::*;
use samepage_map::{Map, prop_str};
use samepage_review::Review;
use samepage_sketch::Sketch;
use serde_json::{Value, json};
use std::sync::Arc;
fn descriptors() -> Vec<samepage_capability::Descriptor> {
    vec![
        samepage_map::descriptor(),
        samepage_review::descriptor(),
        samepage_sketch::descriptor(),
        descriptor(),
    ]
}
fn capabilities() -> Vec<Arc<dyn Capability>> {
    vec![
        Arc::new(Map),
        Arc::new(Review),
        Arc::new(Sketch),
        Arc::new(Explain),
    ]
}
fn host() -> Host {
    Host::in_memory(Composition::new(descriptors()).unwrap(), capabilities())
        .unwrap()
        .with_plan_context("review", &["shape"])
        .unwrap()
}
fn human() -> Actor {
    Actor::new("person", Caller::Human).labelled("Human")
}
fn agent() -> Actor {
    Actor::new("explain-worker", Caller::Agent).labelled("Explain agent")
}
fn invoke(h: &Host, name: &str, a: Value) -> samepage_doc::host::Invoked {
    h.invoke(&agent(), name, a).unwrap()
}
fn card(h: &Host, label: &str) -> String {
    invoke(h, "card_place", json!({"label":label})).ids["card"].clone()
}
fn targets(h: &Host) -> (String, String) {
    (card(h, "Inside evidence"), card(h, "Beyond evidence"))
}
include!("fixture.rs.inc");
fn define(h: &Host, d: &ExplanationFlowDefinition) -> ExplanationFlow {
    invoke(h, "walkthrough_define", json!({"flow":d}));
    flows(&h.view())
        .unwrap()
        .into_iter()
        .find(|f| f.definition.id == d.id)
        .unwrap()
}
fn state(h: &Host, id: &str) -> ExplanationFlowState {
    flows(&h.view())
        .unwrap()
        .into_iter()
        .find(|f| f.definition.id == id)
        .unwrap()
        .state
}
fn peer(h: &Host) -> Replica {
    let mut p = Replica::new();
    p.set_composition(descriptors()).unwrap();
    for f in h.hello() {
        p.receive(&f).unwrap();
    }
    p.take_outbound();
    p
}
fn send(h: &Host, p: &mut Replica) {
    for f in p.take_outbound() {
        h.receive_frame(&human(), &f).unwrap();
    }
}
fn plan(p: &mut Replica, a: Value) -> samepage_doc::replica::Applied {
    let batch = Explain
        .plan_batch(
            &Call {
                action: "walkthrough_draw".into(),
                args: a,
            },
            &p.view(),
        )
        .unwrap();
    p.apply_batch(&batch).unwrap()
}
#[test]
fn drawing_inside_an_exploration_retains_scope_and_cursor() {
    let h = host();
    let (a, b) = targets(&h);
    let mut d = explanation_fixture(&b, &b);
    d.about = vec![a];
    d.scene_ids = vec![b];
    define(&h, &d);
    let old = state(&h, &d.id);
    let result = invoke(
        &h,
        "walkthrough_draw",
        json!({"flow_id":d.id,"shapes":[{"form":"rect"}],"nodes":[{"label":"local thought"}]}),
    );
    assert_eq!(result.commit.unwrap().created.len(), 2);
    assert_eq!(state(&h, &d.id), old);
    let f = flows(&h.view()).unwrap().remove(0);
    assert_eq!(f.definition.scene_ids.len(), 3);
    for id in result.ids.values() {
        assert_eq!(
            prop_str(h.view().get(id).unwrap(), "exploration_id"),
            Some(d.id.as_str())
        );
    }
    let mut left = peer(&h);
    let mut right = peer(&h);
    let l = plan(
        &mut left,
        json!({"flow_id":d.id,"shapes":[{"form":"rect"}]}),
    )
    .ids["shape-0"]
        .clone();
    let r = plan(
        &mut right,
        json!({"flow_id":d.id,"nodes":[{"label":"peer thought"}]}),
    )
    .ids["exploration-card-0"]
        .clone();
    send(&h, &mut left);
    h.invoke(
        &human(),
        "walkthrough_control",
        json!({"flow_id":d.id,"expected_revision":1,"command":"pause"}),
    )
    .unwrap();
    send(&h, &mut right);
    let f = flows(&h.view()).unwrap().remove(0);
    assert!(f.definition.scene_ids.contains(&l) && f.definition.scene_ids.contains(&r));
    assert_eq!(f.state.status, "paused");
    assert_eq!(f.state.entered_revision, 1);
}
#[test]
fn scoped_explanations_coexist_and_reject_cross_room_ownership() {
    let h = host();
    let (a, b) = targets(&h);
    let mut first = explanation_fixture(&b, &b);
    first.about = vec![a.clone()];
    first.scene_ids = vec![b.clone()];
    let f = define(&h, &first);
    let mut second = explanation_fixture(&a, &b);
    second.id = "second-question".into();
    second.about = vec![a.clone()];
    let other = define(&h, &second);
    h.invoke(
        &human(),
        "walkthrough_control",
        json!({"flow_id":first.id,"expected_revision":1,"command":"pause"}),
    )
    .unwrap();
    assert_eq!(state(&h, &second.id).status, "active");
    let before = h.view();
    for scope in [
        vec![b.clone(), b.clone()],
        vec![b],
        vec![a],
        vec!["missing".into()],
    ] {
        second.scene_ids = scope;
        assert!(
            h.invoke(&agent(), "walkthrough_define", json!({"flow":second}))
                .is_err()
        );
        assert_eq!(h.view(), before);
    }
    first.beats[0].cue = "A revised explanation.".into();
    let revised = define(&h, &first);
    assert_eq!(revised.object_id, f.object_id);
    assert_eq!(revised.state.revision, 3);
    assert_eq!(flows(&h.view()).unwrap().len(), 2);
    assert_eq!(state(&h, &other.definition.id).revision, 1);
    assert!(
        h.invoke(
            &human(),
            "walkthrough_draw",
            json!({"flow_id":first.id,"nodes":[{"id":second.about[0],"label":"outside edit"}]})
        )
        .is_err()
    );
    assert_eq!(state(&h, &first.id).revision, 3);
}
#[test]
fn explanation_path_trace_requires_a_connector_and_preserves_marks_on_revision() {
    let h = host();
    let (a, b) = targets(&h);
    let connector = invoke(
        &h,
        "sketch",
        json!({"shapes":[{"form":"arrow","from":a,"to":b}]}),
    )
    .ids["shape-0"]
        .clone();
    let mut d = explanation_fixture(&a, &b);
    d.beats[0].actions[0].kind = "trace-path".into();
    let before = h.view();
    assert!(
        h.invoke(&agent(), "walkthrough_define", json!({"flow":d}))
            .unwrap_err()
            .to_string()
            .contains("arrow or line")
    );
    assert_eq!(h.view(), before);
    d.beats[0].actions[0].target_id = connector.clone();
    let f = define(&h, &d);
    let mark = h
        .invoke(
            &human(),
            "mark_add",
            json!({"target":connector,"glyph":"!","text":"Explain the return path"}),
        )
        .unwrap()
        .ids["mark"]
        .clone();
    d.beats[0].cue = "The return path also merges a shared update.".into();
    let revised = define(&h, &d);
    assert_eq!(f.object_id, revised.object_id);
    assert_eq!(revised.state.revision, 2);
    let before = h.view();
    assert!(
        h.invoke(
            &human(),
            "walkthrough_control",
            json!({"flow_id":d.id,"expected_revision":1,"command":"pause"})
        )
        .is_err()
    );
    assert_eq!(h.view(), before);
    assert_eq!(
        prop_str(h.view().get(&mark).unwrap(), "target"),
        Some(connector.as_str())
    );
    assert!(
        read(&h.view(), None).unwrap()["readback"]
            .as_str()
            .unwrap()
            .contains("trace-path")
    );
    let edge = invoke(&h, "card_link", json!({"from":a,"to":b})).ids["link"].clone();
    d.id = "map-link-trace".into();
    d.beats[0].actions[0].target_id = edge;
    define(&h, &d);
}
#[test]
fn one_generic_explanation_flow_advances_with_utterance_and_selection_evidence() {
    let h = host();
    let (a, b) = targets(&h);
    let d = explanation_fixture(&a, &b);
    define(&h, &d);
    h.invoke(&human(),"walkthrough_advance",json!({"flow_id":d.id,"expected_revision":1,"transition_id":"inside","response":"yes","selected_target_ids":[a.clone()]})).unwrap();
    let s = state(&h, &d.id);
    assert_eq!(s.current_beat, "contrast");
    assert_eq!(s.status, "completed");
    assert_eq!(s.last_response, "yes");
    assert_eq!(s.last_selected_target_ids, vec![a]);
    assert_eq!(s.touched_by, "Human");
    assert!(
        read(&h.view(), None).unwrap()["readback"]
            .as_str()
            .unwrap()
            .contains("response=\"yes\"")
    );
}
#[test]
fn invalid_explanation_graphs_and_stale_transitions_are_atomic() {
    let h = host();
    let (a, b) = targets(&h);
    let d = explanation_fixture(&a, &b);
    let mut invalid = d.clone();
    invalid.beats[0].advance.transitions[0].control = Some("link".into());
    let before = h.view();
    assert!(
        h.invoke(&agent(), "walkthrough_define", json!({"flow":invalid}))
            .is_err()
    );
    assert_eq!(h.view(), before);
    let mut missing = d.clone();
    missing.beats[0].evidence[0].target_id = "not-on-page".into();
    assert!(
        h.invoke(&agent(), "walkthrough_define", json!({"flow":missing}))
            .is_err()
    );
    assert_eq!(h.view(), before);
    let mut cycle = d.clone();
    cycle.beats[1].advance = ExplanationAdvance {
        mode: "continue".into(),
        prompt: "Again".into(),
        transitions: vec![ExplanationTransition {
            id: "again".into(),
            label: "Again".into(),
            control: None,
            next: Some("observe".into()),
            target_ids: vec![],
            phrases: vec![],
        }],
    };
    assert!(
        h.invoke(&agent(), "walkthrough_define", json!({"flow":cycle}))
            .unwrap_err()
            .to_string()
            .contains("must be finite")
    );
    assert_eq!(h.view(), before);
    define(&h, &d);
    let before = h.view();
    let seq = h.journal().seq;
    for transition in [
        json!({"expected_revision":9,"transition_id":"inside","selected_target_ids":[a]}),
        json!({"expected_revision":1,"transition_id":"unknown"}),
        json!({"expected_revision":1,"transition_id":"inside","selected_target_ids":[b]}),
    ] {
        let mut args = transition;
        args["flow_id"] = json!(d.id);
        assert!(h.invoke(&human(), "walkthrough_advance", args).is_err());
        assert_eq!(h.view(), before);
        assert_eq!(h.journal().seq, seq);
    }
}
#[test]
fn explanation_control_and_replica_restore_preserve_one_coherent_cursor() {
    let h = host();
    let (a, b) = targets(&h);
    let d = explanation_fixture(&a, &b);
    define(&h, &d);
    for (revision, command, status, entered) in [
        (1, "pause", "paused", 1),
        (2, "resume", "active", 1),
        (3, "restart", "active", 4),
        (4, "stop", "stopped", 4),
    ] {
        h.invoke(
            &human(),
            "walkthrough_control",
            json!({"flow_id":d.id,"expected_revision":revision,"command":command}),
        )
        .unwrap();
        let s = state(&h, &d.id);
        assert_eq!(s.status, status);
        assert_eq!(s.entered_revision, entered);
    }
    let replica = peer(&h);
    assert_eq!(flows(&replica.view()).unwrap(), flows(&h.view()).unwrap());
    let path = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap())
        .join(format!("restore-{}.json", std::process::id()));
    let disk = Host::open(
        &path,
        Composition::new(descriptors()).unwrap(),
        capabilities(),
    )
    .unwrap();
    let (a, b) = targets(&disk);
    let d = explanation_fixture(&a, &b);
    define(&disk, &d);
    invoke(
        &disk,
        "walkthrough_control",
        json!({"flow_id":d.id,"expected_revision":1,"command":"pause"}),
    );
    disk.save().unwrap();
    let restored = Host::open(
        &path,
        Composition::new(descriptors()).unwrap(),
        capabilities(),
    )
    .unwrap();
    assert_eq!(
        flows(&disk.view()).unwrap(),
        flows(&restored.view()).unwrap()
    );
    std::fs::remove_file(path).unwrap();
}
#[test]
fn unsupported_replay_motion_is_named_and_does_not_disable_the_flow() {
    let h = host();
    let (a, b) = targets(&h);
    let mut d = explanation_fixture(&a, &b);
    d.beats[0].actions[0].kind = "replay-motion".into();
    define(&h, &d);
    let projection = read(&h.view(), None).unwrap();
    assert_eq!(projection["unsupported"][0]["kind"], "replay-motion");
    assert!(
        projection["readback"]
            .as_str()
            .unwrap()
            .contains("UNSUPPORTED: replay-motion")
    );
    invoke(
        &h,
        "walkthrough_advance",
        json!({"flow_id":d.id,"expected_revision":1,"transition_id":"inside"}),
    );
    assert_eq!(state(&h, &d.id).status, "completed");
}
#[test]
fn raw_cursor_and_definition_forgery_are_refused_on_batch_and_replica_routes() {
    let h = host();
    let (a, b) = targets(&h);
    let d = explanation_fixture(&a, &b);
    let f = define(&h, &d);
    let before = h.view();
    let mut forged = f.state.clone();
    forged.revision = 9;
    forged.touched_by.clear();
    let plan = Plan {
        ops: vec![Op::Set {
            target: Target::Id(f.object_id.clone()),
            props: [(
                "state".into(),
                PlanValue::from(serde_json::to_string(&forged).unwrap()),
            )]
            .into(),
        }],
        readback: String::new(),
    };
    assert!(
        h.commit_batch(
            &human(),
            &Batch {
                base_revision: before.revision().into(),
                entries: vec![BatchEntry {
                    capability: ID.into(),
                    plan: plan.clone()
                }]
            }
        )
        .is_err()
    );
    assert_eq!(h.view(), before);
    let mut p = peer(&h);
    p.apply(ID, plan).unwrap();
    for frame in p.take_outbound() {
        assert!(h.receive_frame(&human(), &frame).is_err());
    }
    assert_eq!(h.view(), before);
    let mut p = peer(&h);
    let batch = Explain
        .plan_batch(
            &Call {
                action: "walkthrough_define".into(),
                args: json!({"flow":d}),
            },
            &p.view(),
        )
        .unwrap();
    p.apply_batch(&batch).unwrap();
    for frame in p.take_outbound() {
        assert!(h.receive_frame(&human(), &frame).is_err());
    }
    assert_eq!(h.view(), before);
}
#[test]
fn stale_replica_transition_cannot_overwrite_a_newer_cursor() {
    let h = host();
    let (a, b) = targets(&h);
    let d = explanation_fixture(&a, &b);
    define(&h, &d);
    let mut p = peer(&h);
    let batch = Explain
        .plan_batch(
            &Call {
                action: "walkthrough_advance".into(),
                args: json!({"flow_id":d.id,"expected_revision":1,"transition_id":"inside"}),
            },
            &p.view(),
        )
        .unwrap();
    p.apply_batch(&batch).unwrap();
    invoke(
        &h,
        "walkthrough_control",
        json!({"flow_id":d.id,"expected_revision":1,"command":"pause"}),
    );
    let before = h.view();
    for frame in p.take_outbound() {
        let reply = h.receive_frame(&human(), &frame);
        if reply.is_ok() {
            assert!(
                !h.journal().recent.back().unwrap().visible_change,
                "a stale cursor may only be accepted when its CRDT write loses and is inert"
            );
        }
    }
    assert_eq!(
        h.view().records().cloned().collect::<Vec<_>>(),
        before.records().cloned().collect::<Vec<_>>()
    );
    assert_eq!(state(&h, &d.id).status, "paused");
}
#[test]
fn aliases_are_hidden_and_journaled_under_preferred_names() {
    let h = host();
    let (a, b) = targets(&h);
    let d = explanation_fixture(&a, &b);
    invoke(&h, "atlas_explanation_define", json!({"flow":d}));
    assert_eq!(
        h.journal().recent.back().unwrap().action.as_deref(),
        Some("walkthrough_define")
    );
    assert!(
        descriptor()
            .actions
            .iter()
            .all(|a| !a.name.starts_with("atlas_"))
    );
}

#[test]
fn scoped_cards_and_bound_arrows_resolve_in_one_batch() {
    let h = host();
    let (a, b) = targets(&h);
    let mut d = explanation_fixture(&b, &b);
    d.about = vec![a];
    d.scene_ids = vec![b];
    define(&h, &d);
    let seq = h.journal().seq;
    let result = invoke(
        &h,
        "walkthrough_draw",
        json!({"flow_id":d.id,"nodes":[{"ref":"thought","label":"A scoped thought"}],"shapes":[{"form":"arrow","from":"thought","points":"0,0 100,0"}]}),
    );
    assert_eq!(h.journal().seq, seq + 1);
    assert_eq!(
        prop_str(h.view().get(&result.ids["shape-0"]).unwrap(), "from"),
        Some(result.ids["exploration-card-0"].as_str())
    );
    assert_eq!(state(&h, &d.id).revision, 1);
}
