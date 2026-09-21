use super::*;

fn place(scene: &mut Scene, label: &str, parent: Option<&str>, x: f64, y: f64) -> String {
    place_node(
        scene,
        &NodePatch {
            label: Some(label.to_string()),
            parent: parent.map(str::to_string),
            x: Some(x),
            y: Some(y),
            ..Default::default()
        },
        &Author::Agent,
    )
    .expect("place node")
}

fn machine(scene: &mut Scene, title: &str, x: f64) -> String {
    place(scene, title, None, x, 40.0)
}

fn finish_machine(scene: &Scene, id: &str) {
    set_node_diagram_kind(scene, id, "state_machine", &Author::Agent)
        .expect("set state machine kind");
}

fn role(scene: &Scene, id: &str, initial: bool, terminal: bool) {
    set_state_roles(scene, id, Some(initial), Some(terminal), &Author::Agent)
        .expect("set state roles");
}

fn problem_details(scene: &Scene) -> Vec<String> {
    layout(&read(scene).expect("read atlas"))
        .problems
        .into_iter()
        .map(|problem| problem.detail)
        .collect()
}

#[test]
fn state_roles_are_separate_lww_properties() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "one state", 40.0);
    let state = place(&mut scene, "only", Some(&machine), 100.0, 120.0);
    finish_machine(&scene, &machine);

    set_state_roles(&scene, &state, Some(true), None, &Author::Agent).expect("set initial");
    set_state_roles(&scene, &state, None, Some(true), &Author::Human).expect("set terminal");
    let atlas = read(&scene).expect("read roles");
    let projected = atlas.node(&state).expect("state");
    assert!(projected.state_initial);
    assert!(projected.state_terminal);

    set_state_roles(&scene, &state, Some(false), None, &Author::Agent).expect("clear initial");
    let atlas = read(&scene).expect("read cleared role");
    let projected = atlas.node(&state).expect("state");
    assert!(!projected.state_initial);
    assert!(projected.state_terminal);
}

#[test]
fn a_transition_reads_back_as_from_on_when_arrow() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "request", 40.0);
    let idle = place(&mut scene, "Idle", Some(&machine), 100.0, 120.0);
    let pending = place(&mut scene, "Pending", Some(&machine), 500.0, 120.0);
    finish_machine(&scene, &machine);
    role(&scene, &idle, true, false);
    role(&scene, &pending, false, false);

    transition(
        &mut scene,
        &idle,
        &pending,
        "Submit",
        Some("valid"),
        &Author::Agent,
    )
    .expect("create transition");

    let described = read(&scene).expect("read machine").describe();
    assert!(
        described.contains("- from \"Idle\" on Submit when valid -> \"Pending\""),
        "{described}"
    );
}

#[test]
fn a_cycle_is_stored_and_read_back_without_error() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "cursor", 40.0);
    let active = place(&mut scene, "active", Some(&machine), 100.0, 120.0);
    let paused = place(&mut scene, "paused", Some(&machine), 500.0, 120.0);
    finish_machine(&scene, &machine);
    role(&scene, &active, true, false);
    role(&scene, &paused, false, false);

    transition(&mut scene, &active, &paused, "pause", None, &Author::Agent)
        .expect("pause transition");
    transition(&mut scene, &paused, &active, "resume", None, &Author::Agent)
        .expect("resume transition");

    let atlas = read(&scene).expect("read cyclic machine");
    assert_eq!(atlas.edges.len(), 2);
    let described = atlas.describe();
    assert!(
        described.contains("STATE MACHINE \"cursor\" (2 states, 2 transitions, cyclic)"),
        "{described}"
    );
    assert!(
        described.contains("- from \"paused\" on resume -> \"active\""),
        "{described}"
    );
}

#[test]
fn an_unreachable_state_is_a_problem() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "explanation cursor", 40.0);
    let active = place(&mut scene, "active", Some(&machine), 100.0, 120.0);
    let review = place(&mut scene, "review", Some(&machine), 500.0, 120.0);
    finish_machine(&scene, &machine);
    role(&scene, &active, true, false);
    role(&scene, &review, false, false);

    let expected = "state machine \"explanation cursor\": state \"review\" is unreachable; no transition reaches it from \"active\"";
    assert!(problem_details(&scene).contains(&expected.to_string()));
}

#[test]
fn a_terminal_state_with_an_outgoing_transition_is_a_problem() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "explanation cursor", 40.0);
    let active = place(&mut scene, "active", Some(&machine), 100.0, 120.0);
    let completed = place(&mut scene, "completed", Some(&machine), 500.0, 120.0);
    finish_machine(&scene, &machine);
    role(&scene, &active, true, false);
    role(&scene, &completed, false, true);
    transition(
        &mut scene,
        &active,
        &completed,
        "advance",
        None,
        &Author::Agent,
    )
    .expect("advance transition");
    transition(
        &mut scene,
        &completed,
        &active,
        "restart",
        None,
        &Author::Agent,
    )
    .expect("restart transition");

    let expected = "state machine \"explanation cursor\": terminal state \"completed\" has an outgoing transition on \"restart\"; a terminal state ends the machine";
    assert!(problem_details(&scene).contains(&expected.to_string()));
}

#[test]
fn the_five_state_machine_problem_sentences_are_exact() {
    let mut scene = Scene::new();
    let cursor = machine(&mut scene, "explanation cursor", 40.0);
    let active = place(&mut scene, "active", Some(&cursor), 100.0, 120.0);
    let paused = place(&mut scene, "paused", Some(&cursor), 500.0, 120.0);
    let completed = place(&mut scene, "completed", Some(&cursor), 900.0, 120.0);
    let review = place(&mut scene, "review", Some(&cursor), 1300.0, 120.0);
    finish_machine(&scene, &cursor);
    role(&scene, &active, true, false);
    role(&scene, &paused, true, false);
    role(&scene, &completed, false, true);
    role(&scene, &review, false, false);
    transition(
        &mut scene,
        &active,
        &paused,
        "advance",
        None,
        &Author::Agent,
    )
    .expect("first advance");
    transition(
        &mut scene,
        &active,
        &active,
        "advance",
        None,
        &Author::Agent,
    )
    .expect("second advance");
    transition(
        &mut scene,
        &completed,
        &active,
        "restart",
        None,
        &Author::Agent,
    )
    .expect("restart");

    let no_initial_machine = machine(&mut scene, "empty cursor", 2000.0);
    place(&mut scene, "idle", Some(&no_initial_machine), 2060.0, 120.0);
    finish_machine(&scene, &no_initial_machine);

    let details = problem_details(&scene);
    for expected in [
        "state machine \"explanation cursor\": state \"review\" is unreachable; no transition reaches it from \"active\"",
        "state machine \"explanation cursor\": terminal state \"completed\" has an outgoing transition on \"restart\"; a terminal state ends the machine",
        "state machine \"explanation cursor\": state \"active\" has two transitions on \"advance\" and neither carries a guard; which one fires is ambiguous",
        "state machine \"empty cursor\": no state is marked initial",
        "state machine \"explanation cursor\": \"active\" and \"paused\" are both marked initial",
    ] {
        assert!(details.contains(&expected.to_string()), "{expected}: {details:?}");
    }
}

#[test]
fn a_rebound_transition_is_reported_as_the_relation_that_changed() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "explanation cursor", 40.0);
    let active = place(&mut scene, "active", Some(&machine), 100.0, 120.0);
    let paused = place(&mut scene, "paused", Some(&machine), 500.0, 120.0);
    let stopped = place(&mut scene, "stopped", Some(&machine), 900.0, 120.0);
    finish_machine(&scene, &machine);
    role(&scene, &active, true, false);
    role(&scene, &paused, false, false);
    role(&scene, &stopped, false, false);
    let edge = transition(
        &mut scene,
        &paused,
        &stopped,
        "resume",
        None,
        &Author::Agent,
    )
    .expect("resume transition");
    let before = read(&scene).expect("read before").digest();

    update_transition(
        &scene,
        &edge,
        &TransitionPatch {
            to: Some(active),
            ..Default::default()
        },
        &Author::Human,
    )
    .expect("rebind transition");

    let lines = changes(&before, &read(&scene).expect("read after").digest());
    assert_eq!(
        lines,
        vec![format!(
            "- [{edge}] the transition now reads from \"paused\" on resume -> \"active\"; it was from \"paused\" on resume -> \"stopped\""
        )]
    );
}

#[test]
fn a_moved_state_uses_machine_relation_wording() {
    let mut scene = Scene::new();
    let explanation = machine(&mut scene, "explanation cursor", 40.0);
    let active = place(&mut scene, "active", Some(&explanation), 100.0, 120.0);
    let paused = place(&mut scene, "paused", Some(&explanation), 500.0, 120.0);
    let stopped = place(&mut scene, "stopped", Some(&explanation), 900.0, 120.0);
    finish_machine(&scene, &explanation);
    role(&scene, &active, true, false);
    role(&scene, &paused, false, false);
    role(&scene, &stopped, false, false);
    transition(&mut scene, &paused, &active, "resume", None, &Author::Agent)
        .expect("resume transition");
    transition(&mut scene, &paused, &stopped, "stop", None, &Author::Agent)
        .expect("stop transition");
    transition(&mut scene, &active, &paused, "pause", None, &Author::Agent)
        .expect("pause transition");

    let review = machine(&mut scene, "review cursor", 1400.0);
    let reviewing = place(&mut scene, "reviewing", Some(&review), 1460.0, 120.0);
    finish_machine(&scene, &review);
    role(&scene, &reviewing, true, false);
    let before = read(&scene).expect("read before move").digest();

    place_node(
        &mut scene,
        &NodePatch {
            id: Some(paused.clone()),
            parent: Some(review),
            ..Default::default()
        },
        &Author::Human,
    )
    .expect("move state");

    let lines = changes(&before, &read(&scene).expect("read after move").digest());
    let expected = format!(
        "- [{paused}] \"paused\" was moved into the machine \"review cursor\" by human; it was inside \"explanation cursor\" before, and its 3 transitions now cross between machines"
    );
    assert!(lines.contains(&expected), "{lines:?}");
    assert!(
        problem_details(&scene)
            .iter()
            .any(|detail| detail.contains("crosses into state machine")),
        "a crossing transition must be a problem"
    );
}

#[test]
fn a_guard_edit_reports_both_transition_sentences() {
    let mut scene = Scene::new();
    let machine = machine(&mut scene, "cursor", 40.0);
    let active = place(&mut scene, "active", Some(&machine), 100.0, 120.0);
    finish_machine(&scene, &machine);
    role(&scene, &active, true, false);
    let edge = transition(
        &mut scene,
        &active,
        &active,
        "advance",
        None,
        &Author::Agent,
    )
    .expect("advance transition");
    let before = read(&scene).expect("read before edit").digest();

    update_transition(
        &scene,
        &edge,
        &TransitionPatch {
            guard: Some("the next beat is not terminal".to_string()),
            ..Default::default()
        },
        &Author::Human,
    )
    .expect("edit guard");

    assert_eq!(
        changes(&before, &read(&scene).expect("read after edit").digest()),
        vec![format!(
            "- [{edge}] the transition now reads from \"active\" on advance when the next beat is not terminal -> \"active\"; it was from \"active\" on advance -> \"active\""
        )]
    );
}

#[test]
fn the_explanation_cursor_fixture_round_trips_through_the_core() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fixtures/typed-diagrams/explanation-cursor.json"
    );
    let json = std::fs::read_to_string(path).expect("read fixture file");
    let first = StateMachineSpec::parse(&json).expect("parse fixture");
    let first_description = first.describe().expect("describe fixture");
    assert!(
        first_description
            .contains("STATE MACHINE \"explanation cursor\" (4 states, 10 transitions, cyclic)"),
        "{first_description}"
    );
    assert!(
        first_description.contains(
            "- from \"active\" on advance when the next beat is not terminal -> \"active\""
        ),
        "{first_description}"
    );
    assert!(
        !first_description.contains("PROBLEMS"),
        "{first_description}"
    );

    let exported = first.export().expect("export fixture");
    let second = StateMachineSpec::parse(&exported).expect("parse exported fixture");
    let second_description = second.describe().expect("describe exported fixture");
    assert_eq!(first_description, second_description);
}

/// The fixture's `path` and `lines` must point at the code each note claims.
///
/// `verify_source` only bounds-checks a range, so a citation that survives the
/// writer can still address unrelated code once the cited file grows above it.
/// That happened twice on this board: `core/src/lib.rs` gained work above the
/// explanation flow and every fixture range silently slid onto something else,
/// found only by reading the lines back by hand. This test is that read, done
/// by the machine: each node's range has to still contain the token that names
/// the construct its note describes.
#[test]
fn the_fixture_citations_point_at_the_code_they_describe() {
    // One anchor per fixture node, chosen to be the text a reader would look
    // for to decide the citation is honest. A range that no longer contains
    // its anchor is stale even when it still resolves to real lines.
    const ANCHORS: &[(&str, &[&str])] = &[
        (
            "active",
            &["fn explanation_status_for_beat", "\"active\".to_string()"],
        ),
        (
            "paused",
            &[
                "\"pause\" if state.status == \"active\"",
                "\"resume\" if state.status == \"paused\"",
            ],
        ),
        (
            "completed",
            &["status: next.map_or_else", "\"completed\".to_string()"],
        ),
        (
            "stopped",
            &[
                "\"stop\" if matches!(state.status.as_str(), \"active\" | \"paused\")",
                "state.status = \"stopped\".to_string();",
            ],
        ),
    ];

    let fixture_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fixtures/typed-diagrams/explanation-cursor.json"
    );
    let repository_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    let fixture: JsonValue =
        serde_json::from_str(&std::fs::read_to_string(fixture_path).expect("read fixture file"))
            .expect("parse fixture as json");
    let nodes = fixture["nodes"].as_array().expect("fixture has nodes");

    let mut checked = Vec::new();
    for node in nodes {
        let id = node["id"].as_str().expect("node id");
        let (Some(path), Some(lines)) = (node["path"].as_str(), node["lines"].as_str()) else {
            continue;
        };
        let anchors = ANCHORS
            .iter()
            .find(|(anchor_id, _)| *anchor_id == id)
            .unwrap_or_else(|| {
                panic!("fixture node {id:?} cites {path}:{lines} but this test has no anchor for it; add one so the citation cannot go stale unnoticed")
            })
            .1;

        let source =
            std::fs::read_to_string(format!("{repository_root}/{path}")).unwrap_or_else(|error| {
                panic!("node {id:?} cites {path}, which cannot be read: {error}")
            });
        let source: Vec<&str> = source.lines().collect();

        // `lines` is `N` or `N-M`, one-indexed and inclusive, the same shape
        // the repository service accepts.
        let (first, last) = match lines.split_once('-') {
            Some((first, last)) => (first, last),
            None => (lines, lines),
        };
        let first: usize = first
            .parse()
            .unwrap_or_else(|_| panic!("node {id:?} has unparseable lines {lines:?}"));
        let last: usize = last
            .parse()
            .unwrap_or_else(|_| panic!("node {id:?} has unparseable lines {lines:?}"));
        assert!(
            first >= 1 && last >= first && last <= source.len(),
            "node {id:?} cites {path}:{lines}, which is outside that file's {} lines",
            source.len()
        );
        let cited = source[first - 1..last].join("\n");

        for anchor in anchors {
            assert!(
                cited.contains(anchor),
                "node {id:?} cites {path}:{lines}, which no longer contains {anchor:?}. \
                 The cited range has moved; find the code again and update the fixture. \
                 The range currently reads:\n{cited}"
            );
        }
        checked.push(id);
    }

    // A node quietly losing its citation would otherwise pass by doing nothing.
    for (id, _) in ANCHORS {
        assert!(
            checked.contains(id),
            "this test has an anchor for node {id:?}, but the fixture no longer cites a path and lines for it"
        );
    }
}
