use samepage_capability::{Batch, BatchEntry, Capability, Composition, Op, Plan, Target, Value};
use samepage_diagrams::Diagrams;
use samepage_doc::host::Host;
use samepage_doc::{Actor, Caller};
use samepage_map::Map;
use serde_json::{json, Value as JsonValue};
use std::sync::{Arc, Mutex};
#[path = "../../../apps/samepage/src/diagram_sources.rs"]
mod diagram_sources;
fn host() -> Host {
    let composition = Composition::new(vec![
        samepage_map::descriptor(),
        samepage_diagrams::descriptor(),
    ])
    .unwrap();
    Host::in_memory(composition, vec![Arc::new(Map), Arc::new(Diagrams)]).unwrap()
}
fn actor() -> Actor {
    Actor::new("test-agent", Caller::Agent).labelled("Diagram agent")
}
fn machine() -> JsonValue {
    json!({"kind":"state_machine","title":"request","nodes":[{"id":"idle","label":"Idle","initial":true},{"id":"pending","label":"Pending"},{"id":"done","label":"Done","terminal":true}],"edges":[{"from":"idle","to":"pending","event":"Submit","guard":"valid"},{"from":"pending","to":"done","event":"Finish"}]})
}
fn draw(host: &Host, args: JsonValue) -> std::collections::BTreeMap<String, String> {
    host.invoke(&actor(), "diagram_draw", args).unwrap().ids
}
fn edit(host: &Host, cap: &str, id: &str, key: &str, value: impl Into<Value>) {
    host.commit_batch(
        &actor(),
        &Batch {
            base_revision: host.revision(),
            entries: vec![BatchEntry {
                capability: cap.into(),
                plan: Plan {
                    ops: vec![Op::Set {
                        target: Target::Id(id.into()),
                        props: [(key.into(), value.into())].into(),
                    }],
                    readback: String::new(),
                },
            }],
        },
    )
    .unwrap();
}
fn fingerprint(host: &Host) -> (String, Vec<u8>, u64) {
    (host.revision(), host.encode_full(), host.journal().seq)
}
fn assert_atomic(host: &Host, args: JsonValue) {
    let before = fingerprint(host);
    assert!(host.invoke(&actor(), "diagram_draw", args).is_err());
    assert_eq!(before, fingerprint(host));
}

#[test]
fn one_diagram_call_lands_nodes_links_and_no_drawn_shapes() {
    let host = host();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observer = seen.clone();
    host.on_commit(move |c| observer.lock().unwrap().push(c.clone()));
    let ids = draw(&host, machine());
    let view = host.view();
    assert_eq!(view.records().filter(|r| r.kind == "node").count(), 4);
    assert_eq!(view.records().filter(|r| r.kind == "edge").count(), 2);
    assert_eq!(view.records().count(), 6);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap()[0].created.len(), 6);
    assert_eq!(
        samepage_map::prop_str(view.get(&ids["node-0"]).unwrap(), "parent"),
        Some(ids["container-0"].as_str())
    );
    assert!(view
        .records()
        .all(|r| samepage_map::prop_str(r, "created_by") == Some("Diagram agent")));
}
#[test]
fn diagram_validation_finishes_before_the_first_node_lands() {
    let host = host();
    for bad in [
        json!({"nodes":[{"id":"a"},{"id":"a"}]}),
        json!({"nodes":[{"id":"a"}],"edges":[{"from":"a","to":"missing"}]}),
        json!({"nodes":[{"id":"a","shape":"triangle"}]}),
        json!({"kind":"hierarchy","nodes":[{"id":"a"}],"edges":[{"from":"a","to":"a"}]}),
        json!({"kind":"state_machine","nodes":[{"id":"a"}],"edges":[{"from":"a","to":"a","event":""}]}),
    ] {
        assert_atomic(&host, bad);
    }
}
#[test]
fn self_loop_and_parallel_transitions_are_fresh_edges_ordinary_links_keep_their_policy() {
    let host = host();
    let mut spec = machine();
    spec["edges"] = json!([{"from":"idle","to":"idle","event":"Retry"},{"from":"idle","to":"pending","event":"Submit"},{"from":"idle","to":"pending","event":"Resume"}]);
    let ids = draw(&host, spec);
    assert_ne!(ids["edge-1"], ids["edge-2"]);
    let view = Diagrams.project(&host.view()).unwrap();
    assert_eq!(view["transitions"].as_array().unwrap().len(), 3);
    let paths = view["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["points"].to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(paths.len(), 3);
    assert!(host
        .invoke(
            &actor(),
            "card_link",
            json!({"from":ids["node-0"],"to":ids["node-0"]})
        )
        .is_err());
    let a = host
        .invoke(
            &actor(),
            "card_link",
            json!({"from":ids["node-0"],"to":ids["node-1"],"label":"ordinary"}),
        )
        .unwrap();
    assert!(a
        .ids
        .values()
        .all(|id| id != &ids["edge-1"] && id != &ids["edge-2"]));
    let count = host.view().records().count();
    host.invoke(
        &actor(),
        "card_link",
        json!({"from":ids["node-0"],"to":ids["node-1"],"label":"changed"}),
    )
    .unwrap();
    assert_eq!(count, host.view().records().count());
}
#[test]
fn a_transition_reads_back_as_from_on_when_arrow() {
    let host = host();
    draw(&host, machine());
    assert!(samepage_diagrams::describe(&host.view())
        .unwrap()
        .contains("- from \"Idle\" on Submit when valid -> \"Pending\""));
}
#[test]
fn state_roles_are_separate_lww_properties() {
    let host = host();
    let ids = draw(&host, machine());
    edit(&host, "diagrams", &ids["node-0"], "state_terminal", true);
    edit(&host, "diagrams", &ids["node-0"], "state_initial", false);
    let r = host.view();
    let node = r.get(&ids["node-0"]).unwrap();
    assert_eq!(node.props["state_initial"], false.into());
    assert_eq!(node.props["state_terminal"], true.into());
}
#[test]
fn a_cycle_is_stored_and_read_back_without_error() {
    let host = host();
    let mut spec = machine();
    spec["edges"] = json!([{"from":"idle","to":"pending","event":"pause"},{"from":"pending","to":"idle","event":"resume"}]);
    draw(&host, spec);
    let text = samepage_diagrams::describe(&host.view()).unwrap();
    assert!(text.contains("3 states, 2 transitions, cyclic"));
    assert!(text.contains("from \"Pending\" on resume -> \"Idle\""));
}
#[test]
fn the_five_state_machine_problem_sentences_are_exact() {
    let host = host();
    draw(
        &host,
        json!({"kind":"state_machine","title":"explanation cursor","nodes":[{"id":"active","initial":true},{"id":"paused","initial":true},{"id":"completed","terminal":true},{"id":"review"}],"edges":[{"from":"active","to":"paused","event":"advance"},{"from":"active","to":"active","event":"advance"},{"from":"completed","to":"active","event":"restart"}]}),
    );
    draw(
        &host,
        json!({"kind":"state_machine","title":"empty cursor","nodes":[{"id":"idle"}]}),
    );
    let p = Diagrams.project(&host.view()).unwrap();
    let details = p["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["detail"].as_str().unwrap())
        .collect::<Vec<_>>();
    for expected in [
        "state machine \"explanation cursor\": state \"review\" is unreachable; no transition reaches it from \"active\"",
        "state machine \"explanation cursor\": terminal state \"completed\" has an outgoing transition on \"restart\"; a terminal state ends the machine",
        "state machine \"explanation cursor\": state \"active\" has two transitions on \"advance\" and neither carries a guard; which one fires is ambiguous",
        "state machine \"empty cursor\": no state is marked initial",
        "state machine \"explanation cursor\": \"active\" and \"paused\" are both marked initial",
    ]{assert!(details.contains(&expected),"{details:?}");}
}
#[test]
fn a_rebound_transition_is_reported_as_the_relation_that_changed() {
    let host = host();
    let ids = draw(&host, machine());
    let before = Diagrams.digest(&host.view()).unwrap();
    edit(&host, "map", &ids["edge-0"], "to", ids["node-0"].clone());
    let changes = Diagrams
        .changes(&before, &Diagrams.digest(&host.view()).unwrap(), "agent")
        .unwrap();
    assert_eq!(changes,vec![format!("- [{}] the transition now reads from \"Idle\" on Submit when valid -> \"Idle\"; it was from \"Idle\" on Submit when valid -> \"Pending\"",ids["edge-0"])]);
}
#[test]
fn a_guard_edit_reports_both_transition_sentences() {
    let host = host();
    let ids = draw(&host, machine());
    let before = Diagrams.digest(&host.view()).unwrap();
    edit(&host, "diagrams", &ids["edge-0"], "guard", "ready");
    assert_eq!(Diagrams.changes(&before,&Diagrams.digest(&host.view()).unwrap(),"agent").unwrap(),vec![format!("- [{}] the transition now reads from \"Idle\" on Submit when ready -> \"Pending\"; it was from \"Idle\" on Submit when valid -> \"Pending\"",ids["edge-0"])]);
}
#[test]
fn atlas_diagram_stores_the_styles_it_used_to_discard() {
    let host = host();
    let ids=host.invoke(&actor(),"atlas_diagram",json!({"nodes":[{"id":"start","shape":"ellipse","size":"hero"},{"id":"branch","shape":"diamond","color":"amber"},{"id":"risk","color":"#f87171","filled":true},{"id":"plain"}],"edges":[{"from":"start","to":"branch"}]})).unwrap().ids;
    let p = Map.project(&host.view()).unwrap();
    let card = |i| {
        p["cards"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == ids[&format!("node-{i}")])
            .unwrap()
    };
    assert_eq!(card(0)["shape"], "ellipse");
    assert_eq!(card(0)["size"], "hero");
    assert_eq!(card(1)["shape"], "diamond");
    assert_eq!(card(1)["color"], "amber");
    assert_eq!(card(2)["color"], "red");
    assert_eq!(card(2)["emphasis"], "strong");
    assert_eq!(card(3)["color"], "");
}
#[test]
fn a_three_level_hierarchy_lays_out_with_no_container_overlap_and_every_child_inside_its_parent() {
    let host = host();
    draw(
        &host,
        json!({"kind":"hierarchy","containers":[{"id":"c","label":"C","parent":"b"},{"id":"b","label":"B","parent":"a"},{"id":"a","label":"A"},{"id":"d","label":"D"}],"nodes":[{"id":"one","group":"a"},{"id":"two","group":"b"},{"id":"three","group":"c"},{"id":"four","group":"d"}]}),
    );
    let p = Map.project(&host.view()).unwrap();
    let cards = p["cards"].as_array().unwrap();
    let n = |r: &JsonValue, k: &str| r[k].as_f64().unwrap();
    for child in cards.iter().filter(|c| c["parent"] != "") {
        let parent = cards.iter().find(|p| p["id"] == child["parent"]).unwrap();
        assert!(
            n(child, "x") >= n(parent, "x")
                && n(child, "y") >= n(parent, "y")
                && n(child, "x") + n(child, "width") <= n(parent, "x") + n(parent, "width")
                && n(child, "y") + n(child, "height") <= n(parent, "y") + n(parent, "height")
        );
    }
    let roots = cards
        .iter()
        .filter(|c| c["parent"] == "")
        .collect::<Vec<_>>();
    assert_eq!(roots.len(), 2);
    assert!(
        n(roots[0], "x") + n(roots[0], "width") <= n(roots[1], "x")
            || n(roots[1], "x") + n(roots[1], "width") <= n(roots[0], "x")
            || n(roots[0], "y") + n(roots[0], "height") <= n(roots[1], "y")
            || n(roots[1], "y") + n(roots[1], "height") <= n(roots[0], "y")
    );
}
fn source_host() -> Host {
    host()
        .with_verifier(
            "diagrams",
            Arc::new(
                diagram_sources::DiagramSources::open(std::path::Path::new(env!(
                    "CARGO_MANIFEST_DIR"
                )))
                .unwrap(),
            ),
        )
        .unwrap()
}
#[test]
fn atlas_diagram_verifies_and_packs_source_backed_nodes_in_the_original_batch() {
    let host = source_host();
    let ids = draw(
        &host,
        json!({"nodes":[{"id":"lib","label":"Planner","note":"Real source","path":"src/lib.rs","lines":"1-20"},{"id":"layout","path":"src/layout.rs","lines":"1-10"}],"edges":[{"from":"lib","to":"layout"}]}),
    );
    assert_eq!(host.journal().seq, 1);
    let view = host.view();
    assert_eq!(
        samepage_map::prop_str(view.get(&ids["node-0"]).unwrap(), "path"),
        Some("src/lib.rs")
    );
}
#[test]
fn a_batch_with_one_bad_source_reference_draws_nothing() {
    let host = source_host();
    assert_atomic(
        &host,
        json!({"nodes":[{"id":"good","path":"src/lib.rs","lines":"1-10"},{"id":"bad","path":"src/lib.rs","lines":"999999"}]}),
    );
    assert_atomic(
        &host,
        json!({"nodes":[{"id":"good","path":"src/lib.rs","lines":"1-10"},{"id":"bad","path":"absent.rs","lines":"1"}]}),
    );
}
#[test]
fn source_facts_are_required_and_lines_only_edits_verify_the_effective_binding() {
    assert_atomic(
        &host(),
        json!({"nodes":[{"id":"a","path":"src/lib.rs","lines":"1"}]}),
    );
    let host = source_host();
    let ids = draw(
        &host,
        json!({"nodes":[{"id":"a","path":"src/lib.rs","lines":"1"}]}),
    );
    let before = fingerprint(&host);
    let batch = Batch {
        base_revision: host.revision(),
        entries: vec![BatchEntry {
            capability: "diagrams".into(),
            plan: Plan {
                ops: vec![Op::Set {
                    target: Target::Id(ids["node-0"].clone()),
                    props: [("lines".into(), "999999".into())].into(),
                }],
                readback: String::new(),
            },
        }],
    };
    assert!(host.commit_batch(&actor(), &batch).is_err());
    assert_eq!(before, fingerprint(&host));
}
#[test]
fn malformed_typed_transition_cannot_bypass_map_self_link_refusal() {
    let host = host();
    let ids = draw(&host, machine());
    let before = fingerprint(&host);
    let batch = Batch {
        base_revision: host.revision(),
        entries: vec![
            BatchEntry {
                capability: "map".into(),
                plan: Plan {
                    ops: vec![Op::Create {
                        reference: "bad".into(),
                        kind: "edge".into(),
                        props: [
                            ("from".into(), ids["node-0"].clone().into()),
                            ("to".into(), ids["node-0"].clone().into()),
                        ]
                        .into(),
                    }],
                    readback: String::new(),
                },
            },
            BatchEntry {
                capability: "diagrams".into(),
                plan: Plan {
                    ops: vec![Op::Set {
                        target: Target::Ref("bad".into()),
                        props: [("guard".into(), "".into())].into(),
                    }],
                    readback: String::new(),
                },
            },
        ],
    };
    assert!(host.commit_batch(&actor(), &batch).is_err());
    assert_eq!(before, fingerprint(&host));
}
#[test]
fn explicit_clear_is_atomic_and_default_draw_preserves_existing_cards() {
    let host = source_host();
    draw(&host, json!({"nodes":[{"id":"old"}]}));
    draw(&host, json!({"nodes":[{"id":"also"}]}));
    assert_eq!(host.view().records().count(), 2);
    assert_atomic(
        &host,
        json!({"clear":true,"nodes":[{"id":"bad","path":"missing.rs","lines":"1"}]}),
    );
    draw(&host, json!({"clear":true,"nodes":[{"id":"new"}]}));
    assert_eq!(host.view().records().count(), 1);
    assert_eq!(
        samepage_map::prop_str(host.view().records().next().unwrap(), "label"),
        Some("new")
    );
}
#[test]
fn bounded_source_ranges_in_large_files_remain_supported() {
    let root = std::env::temp_dir().join(format!("diagram-large-source-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("large.rs"), "// line\n".repeat(350_000)).unwrap();
    let sources = diagram_sources::DiagramSources::open(&root).unwrap();
    sources.check("large.rs", "1-2").unwrap();
    sources.check("large.rs", "349999-350000").unwrap();
    assert!(sources.check("large.rs", "350001").is_err());
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn source_paths_refuse_escape_private_and_symlink_targets() {
    let root = std::env::temp_dir().join(format!("diagram-source-policy-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".local")).unwrap();
    std::fs::write(root.join(".local/private.rs"), "private").unwrap();
    std::fs::write(root.join(".env"), "private").unwrap();
    std::fs::write(root.join("safe.rs"), "text\n").unwrap();
    let sources = diagram_sources::DiagramSources::open(&root).unwrap();
    for path in ["../outside.rs", "/etc/passwd", ".local/private.rs", ".env"] {
        assert!(sources.check(path, "1").is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join(".local/private.rs"), root.join("alias.rs")).unwrap();
        assert!(sources.check("alias.rs", "1").is_err());
    }
    sources.check("safe.rs", "1").unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn changing_an_edge_free_machine_cannot_orphan_unchanged_state_roles() {
    let host = host();
    let ids = draw(
        &host,
        json!({"kind":"state_machine","title":"Single state","nodes":[{"id":"only","initial":true}]}),
    );
    for kind in ["hierarchy", ""] {
        let before = fingerprint(&host);
        let batch = Batch {
            base_revision: host.revision(),
            entries: vec![BatchEntry {
                capability: "diagrams".into(),
                plan: Plan {
                    ops: vec![Op::Set {
                        target: Target::Id(ids["container-0"].clone()),
                        props: [("diagram_kind".into(), kind.into())].into(),
                    }],
                    readback: String::new(),
                },
            }],
        };
        assert!(host.commit_batch(&actor(), &batch).is_err());
        assert_eq!(before, fingerprint(&host));
    }
}

#[test]
fn explicit_clear_restarts_default_geometry_at_the_origin() {
    let host = host();
    draw(&host, json!({"y":19000,"nodes":[{"id":"old"}]}));
    let ids = draw(&host, json!({"clear":true,"nodes":[{"id":"new"}]}));
    assert_eq!(
        samepage_map::prop_num(host.view().get(&ids["node-0"]).unwrap(), "y"),
        Some(48.0)
    );
}

#[test]
fn portable_routes_follow_local_measured_bounds_without_mutating_the_view() {
    let host = host();
    let ids = draw(&host, machine());
    let view = host.view();
    let before = fingerprint(&host);
    let map = Map.project(&view).unwrap();
    let mut bounds = serde_json::Map::new();
    for c in map["cards"].as_array().unwrap() {
        let x = c["x"].as_f64().unwrap();
        let y = c["y"].as_f64().unwrap();
        let w = c["width"].as_f64().unwrap();
        let h = c["height"].as_f64().unwrap();
        bounds.insert(
            c["id"].as_str().unwrap().into(),
            json!([x, y, x + w, y + h + 90.0]),
        );
    }
    let result: JsonValue = serde_json::from_str(&samepage_diagrams::project_with_bounds(
        &serde_json::to_string(&view).unwrap(),
        &JsonValue::Object(bounds.clone()).to_string(),
    ))
    .unwrap();
    assert!(result.get("error").is_none(), "{result}");
    let edge = result["ok"]["transitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == ids["edge-0"])
        .unwrap();
    let point = edge["points"][0].as_array().unwrap();
    let b = bounds[&ids["node-0"]].as_array().unwrap();
    let x = point[0].as_f64().unwrap();
    let y = point[1].as_f64().unwrap();
    assert!(
        x == b[0].as_f64().unwrap()
            || x == b[2].as_f64().unwrap()
            || y == b[1].as_f64().unwrap()
            || y == b[3].as_f64().unwrap()
    );
    assert_eq!(before, fingerprint(&host));
    let invalid: JsonValue = serde_json::from_str(&samepage_diagrams::project_with_bounds(
        &serde_json::to_string(&view).unwrap(),
        "{}",
    ))
    .unwrap();
    assert!(invalid.get("error").is_some());
}
