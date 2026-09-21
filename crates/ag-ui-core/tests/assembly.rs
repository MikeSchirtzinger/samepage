//! Stream assembly contract tests.
//!
//! These drive `ag_ui_core::assembly::Assembler` with wire-shaped JSON (the
//! exact `data:` payloads the runtime emits) so they double as the contract
//! the browser client relies on: which events produce which updates, which
//! spec violations are repaired and reported, and which are refused.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::Path;

use ag_ui_core::assembly::{
    Anomaly, Assembler, AssemblyError, Outcome, PushError, RunPhase, Update,
};
use ag_ui_core::event::{Event, EventType};
use ag_ui_core::types::{MessageId, Role, ToolCallId};
use serde_json::json;

const A: &str = "00000000-0000-0000-0000-00000000000a";
const B: &str = "00000000-0000-0000-0000-00000000000b";

fn id(s: &str) -> MessageId {
    s.parse().unwrap()
}

fn push(assembler: &mut Assembler, payload: serde_json::Value) -> Outcome {
    let (_, outcome) = assembler
        .push_json(&payload.to_string())
        .unwrap_or_else(|error| panic!("{payload}: {error}"));
    outcome
}

fn push_err(assembler: &mut Assembler, payload: serde_json::Value) -> AssemblyError {
    match assembler.push_json(&payload.to_string()) {
        Err(PushError::Assembly(error)) => error,
        Err(PushError::Parse(error)) => panic!("{payload} did not parse: {error}"),
        Ok((_, outcome)) => panic!("{payload} was accepted: {outcome:?}"),
    }
}

fn start(message_id: &str) -> serde_json::Value {
    json!({"type": "TEXT_MESSAGE_START", "messageId": message_id, "role": "assistant"})
}

fn content(message_id: &str, delta: &str) -> serde_json::Value {
    json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": message_id, "delta": delta})
}

fn end(message_id: &str) -> serde_json::Value {
    json!({"type": "TEXT_MESSAGE_END", "messageId": message_id})
}

#[test]
fn start_content_end_assembles_one_message() {
    let mut assembler = Assembler::new();
    assert_eq!(
        push(&mut assembler, start(A)).updates,
        vec![Update::TextStarted {
            message_id: id(A),
            role: Role::Assistant,
            name: None
        }]
    );
    assert_eq!(
        push(&mut assembler, content(A, "Hel")).updates,
        vec![Update::TextDelta {
            message_id: id(A),
            delta: "Hel".into()
        }]
    );
    push(&mut assembler, content(A, "lo"));
    assert_eq!(assembler.text(&id(A)), Some("Hello"));
    let finished = push(&mut assembler, end(A));
    assert_eq!(
        finished.updates,
        vec![Update::TextFinished {
            message_id: id(A),
            role: Role::Assistant,
            name: None,
            text: "Hello".into()
        }]
    );
    assert!(finished.anomalies.is_empty());
    assert_eq!(assembler.text(&id(A)), None);
    assert_eq!(assembler.counters().events, 4);
    assert_eq!(assembler.counters().updates, 4);
    assert_eq!(assembler.counters().anomalies, 0);
}

#[test]
fn two_messages_interleave_without_crosstalk() {
    let mut assembler = Assembler::new();
    push(&mut assembler, start(A));
    push(&mut assembler, start(B));
    push(&mut assembler, content(A, "a1"));
    push(&mut assembler, content(B, "b1"));
    push(&mut assembler, content(A, "a2"));
    assert_eq!(assembler.text(&id(A)), Some("a1a2"));
    assert_eq!(assembler.text(&id(B)), Some("b1"));
    assert_eq!(
        assembler.open_text_ids().cloned().collect::<Vec<_>>(),
        vec![id(A), id(B)]
    );
}

#[test]
fn content_for_an_unstarted_message_is_refused_and_changes_nothing() {
    let mut assembler = Assembler::new();
    push(&mut assembler, start(A));
    let error = push_err(&mut assembler, content(B, "orphan"));
    assert_eq!(
        error,
        AssemblyError::TextContentWithoutStart { message_id: id(B) }
    );
    assert_eq!(assembler.text(&id(B)), None);
    assert_eq!(assembler.text(&id(A)), Some(""));
    assert_eq!(assembler.counters().errors, 1);
    assert_eq!(assembler.counters().events, 1);
}

#[test]
fn end_without_start_is_an_anomaly_not_an_error() {
    let mut assembler = Assembler::new();
    let outcome = push(&mut assembler, end(A));
    assert!(outcome.updates.is_empty());
    assert_eq!(
        outcome.anomalies,
        vec![Anomaly::TextEndWithoutStart { message_id: id(A) }]
    );
}

#[test]
fn reconnect_replay_resets_the_open_message_to_the_replayed_text() {
    // The runtime's `sse_handler` replays an in-flight message as
    // START + one CONTENT carrying everything so far. The browser assembler
    // still holds the pre-reconnect partial text, so the START must reset.
    let mut assembler = Assembler::new();
    push(&mut assembler, start(A));
    push(&mut assembler, content(A, "Hel"));
    let replayed_start = push(&mut assembler, start(A));
    assert_eq!(
        replayed_start.anomalies,
        vec![Anomaly::TextRestarted {
            message_id: id(A),
            dropped_chars: 3
        }]
    );
    assert_eq!(
        replayed_start.updates,
        vec![Update::TextStarted {
            message_id: id(A),
            role: Role::Assistant,
            name: None
        }]
    );
    push(&mut assembler, content(A, "Hello, wor"));
    push(&mut assembler, content(A, "ld"));
    assert_eq!(assembler.text(&id(A)), Some("Hello, world"));
}

#[test]
fn empty_delta_is_reported_and_not_appended() {
    let mut assembler = Assembler::new();
    push(&mut assembler, start(A));
    let outcome = push(&mut assembler, content(A, ""));
    assert!(outcome.updates.is_empty());
    assert_eq!(
        outcome.anomalies,
        vec![Anomaly::EmptyDelta {
            event: EventType::TextMessageContent
        }]
    );
}

#[test]
fn chunks_open_grow_and_are_finished_by_the_next_non_chunk_event() {
    let mut assembler = Assembler::new();
    let first = push(
        &mut assembler,
        json!({"type": "TEXT_MESSAGE_CHUNK", "messageId": A, "role": "user", "name": "mike", "delta": "chu"}),
    );
    assert_eq!(
        first.updates,
        vec![
            Update::TextStarted {
                message_id: id(A),
                role: Role::User,
                name: Some("mike".into())
            },
            Update::TextDelta {
                message_id: id(A),
                delta: "chu".into()
            }
        ]
    );
    // No messageId: continues the last chunk-opened message.
    push(
        &mut assembler,
        json!({"type": "TEXT_MESSAGE_CHUNK", "delta": "nk"}),
    );
    assert_eq!(assembler.text(&id(A)), Some("chunk"));
    // Any non-chunk event ends the chunk run.
    let closed = push(
        &mut assembler,
        json!({"type": "CUSTOM", "name": "surface.tutor", "value": {"state": "ready"}}),
    );
    assert_eq!(
        closed.updates,
        vec![Update::TextFinished {
            message_id: id(A),
            role: Role::User,
            name: Some("mike".into()),
            text: "chunk".into()
        }]
    );
    assert!(closed.anomalies.is_empty());
}

#[test]
fn a_content_bearing_chunk_with_nothing_to_attach_to_is_refused() {
    let mut assembler = Assembler::new();
    assert_eq!(
        push_err(
            &mut assembler,
            json!({"type": "TEXT_MESSAGE_CHUNK", "delta": "x"})
        ),
        AssemblyError::TextChunkWithoutMessageId
    );
    // An empty id-less chunk carries nothing and is accepted as a no-op.
    let outcome = push(&mut assembler, json!({"type": "TEXT_MESSAGE_CHUNK"}));
    assert!(outcome.updates.is_empty());
}

#[test]
fn a_plain_content_event_for_a_chunk_opened_id_is_refused_after_the_run_closes() {
    // Spec: a chunk run ends at the first non-chunk event. A server that then
    // sends TEXT_MESSAGE_CONTENT for that id is mixing the two lifecycles;
    // accepting would drop the delta, so it is refused, and the refusal must
    // not have closed the chunk run on the way.
    let mut assembler = Assembler::new();
    push(
        &mut assembler,
        json!({"type": "TEXT_MESSAGE_CHUNK", "messageId": A, "delta": "a"}),
    );
    assert_eq!(
        push_err(&mut assembler, content(A, "b")),
        AssemblyError::TextContentWithoutStart { message_id: id(A) }
    );
    assert_eq!(assembler.text(&id(A)), Some("a"));
}

#[test]
fn tool_call_arguments_assemble_into_a_tool_call() {
    let mut assembler = Assembler::new();
    push(
        &mut assembler,
        json!({"type": "TOOL_CALL_START", "toolCallId": "call_1", "toolCallName": "search", "parentMessageId": A}),
    );
    push(
        &mut assembler,
        json!({"type": "TOOL_CALL_ARGS", "toolCallId": "call_1", "delta": "{\"q\":"}),
    );
    push(
        &mut assembler,
        json!({"type": "TOOL_CALL_ARGS", "toolCallId": "call_1", "delta": "\"rust\"}"}),
    );
    let finished = push(
        &mut assembler,
        json!({"type": "TOOL_CALL_END", "toolCallId": "call_1"}),
    );
    match &finished.updates[..] {
        [Update::ToolCallFinished {
            tool_call,
            parent_message_id,
        }] => {
            assert_eq!(&*tool_call.id, "call_1");
            assert_eq!(tool_call.function.name, "search");
            assert_eq!(tool_call.function.arguments, r#"{"q":"rust"}"#);
            assert_eq!(parent_message_id, &Some(id(A)));
        }
        other => panic!("unexpected updates: {other:?}"),
    }
    let result = push(
        &mut assembler,
        json!({"type": "TOOL_CALL_RESULT", "messageId": B, "toolCallId": "call_1", "content": "42", "role": "tool"}),
    );
    assert_eq!(
        result.updates,
        vec![Update::ToolCallResult {
            message_id: id(B),
            tool_call_id: ToolCallId::new("call_1"),
            content: "42".into()
        }]
    );
}

#[test]
fn tool_call_chunks_need_a_name_to_open_and_an_id_to_continue() {
    let mut assembler = Assembler::new();
    assert_eq!(
        push_err(
            &mut assembler,
            json!({"type": "TOOL_CALL_CHUNK", "toolCallId": "call_9", "delta": "{"})
        ),
        AssemblyError::ToolCallChunkWithoutName {
            tool_call_id: ToolCallId::new("call_9")
        }
    );
    assert_eq!(
        push_err(
            &mut assembler,
            json!({"type": "TOOL_CALL_CHUNK", "delta": "{"})
        ),
        AssemblyError::ToolCallChunkWithoutId
    );
    push(
        &mut assembler,
        json!({"type": "TOOL_CALL_CHUNK", "toolCallId": "call_9", "toolCallName": "grep", "delta": "{\"p\":"}),
    );
    push(
        &mut assembler,
        json!({"type": "TOOL_CALL_CHUNK", "delta": "1}"}),
    );
    let closed = push(
        &mut assembler,
        json!({"type": "STEP_STARTED", "stepName": "next"}),
    );
    match &closed.updates[..] {
        [Update::ToolCallFinished { tool_call, .. }] => {
            assert_eq!(tool_call.function.arguments, r#"{"p":1}"#)
        }
        other => panic!("unexpected updates: {other:?}"),
    }
}

#[test]
fn reasoning_messages_assemble_and_brackets_are_tracked() {
    let mut assembler = Assembler::new();
    push(
        &mut assembler,
        json!({"type": "REASONING_START", "messageId": A}),
    );
    push(
        &mut assembler,
        json!({"type": "REASONING_MESSAGE_START", "messageId": A, "role": "reasoning"}),
    );
    push(
        &mut assembler,
        json!({"type": "REASONING_MESSAGE_CONTENT", "messageId": A, "delta": "think"}),
    );
    let finished = push(
        &mut assembler,
        json!({"type": "REASONING_MESSAGE_END", "messageId": A}),
    );
    assert_eq!(
        finished.updates,
        vec![Update::ReasoningFinished {
            message_id: id(A),
            text: "think".into()
        }]
    );
    let bracket = push(
        &mut assembler,
        json!({"type": "REASONING_END", "messageId": A}),
    );
    assert!(bracket.anomalies.is_empty());
    // END for a bracket never opened is reported, not refused.
    let stray = push(
        &mut assembler,
        json!({"type": "REASONING_END", "messageId": B}),
    );
    assert_eq!(
        stray.anomalies,
        vec![Anomaly::ReasoningBracketMismatch {
            message_id: id(B),
            event: EventType::ReasoningEnd
        }]
    );
    // Content for an unstarted reasoning message would be lost: refused.
    assert_eq!(
        push_err(
            &mut assembler,
            json!({"type": "REASONING_MESSAGE_CONTENT", "messageId": B, "delta": "x"})
        ),
        AssemblyError::ReasoningContentWithoutStart { message_id: id(B) }
    );
}

#[test]
fn legacy_thinking_family_assembles_text() {
    let mut assembler = Assembler::new();
    assert_eq!(
        push_err(
            &mut assembler,
            json!({"type": "THINKING_TEXT_MESSAGE_CONTENT", "delta": "x"})
        ),
        AssemblyError::ThinkingContentWithoutStart
    );
    push(
        &mut assembler,
        json!({"type": "THINKING_START", "title": "plan"}),
    );
    push(
        &mut assembler,
        json!({"type": "THINKING_TEXT_MESSAGE_START"}),
    );
    push(
        &mut assembler,
        json!({"type": "THINKING_TEXT_MESSAGE_CONTENT", "delta": "step 1"}),
    );
    let text_done = push(&mut assembler, json!({"type": "THINKING_TEXT_MESSAGE_END"}));
    assert_eq!(
        text_done.updates,
        vec![Update::ThinkingFinished {
            title: Some("plan".into()),
            text: "step 1".into()
        }]
    );
    let phase_done = push(&mut assembler, json!({"type": "THINKING_END"}));
    assert_eq!(
        phase_done.updates,
        vec![Update::ThinkingFinished {
            title: Some("plan".into()),
            text: String::new()
        }]
    );
    let stray = push(&mut assembler, json!({"type": "THINKING_END"}));
    assert_eq!(
        stray.anomalies,
        vec![Anomaly::ThinkingEndWithoutStart {
            event: EventType::ThinkingEnd
        }]
    );
}

#[test]
fn a_run_ending_with_a_message_open_finishes_it_and_says_so() {
    let mut assembler = Assembler::new();
    push(
        &mut assembler,
        json!({"type": "RUN_STARTED", "threadId": A, "runId": B}),
    );
    assert_eq!(
        assembler.run(),
        RunPhase::Running {
            thread_id: A.parse().unwrap(),
            run_id: B.parse().unwrap()
        }
    );
    push(&mut assembler, start(A));
    push(&mut assembler, content(A, "unfinished"));
    let outcome = push(
        &mut assembler,
        json!({"type": "RUN_FINISHED", "threadId": A, "runId": B}),
    );
    assert_eq!(
        outcome.updates,
        vec![Update::TextFinished {
            message_id: id(A),
            role: Role::Assistant,
            name: None,
            text: "unfinished".into()
        }]
    );
    assert_eq!(
        outcome.anomalies,
        vec![Anomaly::ClosedEarly {
            by: EventType::RunFinished,
            text_message_ids: vec![id(A)],
            tool_call_ids: vec![],
            reasoning_message_ids: vec![],
            thinking: false
        }]
    );
    assert!(matches!(assembler.run(), RunPhase::Finished { .. }));
}

#[test]
fn messages_outside_a_run_are_accepted_and_only_end_without_start_is_noted() {
    // The runtime streams TEXT_MESSAGE_* without RUN_* brackets. That is
    // accepted silently; a RUN_FINISHED with no RUN_STARTED is only noted.
    let mut assembler = Assembler::new();
    push(&mut assembler, start(A));
    push(&mut assembler, content(A, "x"));
    let done = push(&mut assembler, end(A));
    assert!(done.anomalies.is_empty());
    let finished = push(
        &mut assembler,
        json!({"type": "RUN_FINISHED", "threadId": A, "runId": B}),
    );
    assert_eq!(
        finished.anomalies,
        vec![Anomaly::RunEndWithoutStart {
            event: EventType::RunFinished
        }]
    );
}

#[test]
fn a_messages_snapshot_replaces_the_transcript_and_closes_what_was_open() {
    let mut assembler = Assembler::new();
    push(&mut assembler, start(A));
    let outcome = push(
        &mut assembler,
        json!({"type": "MESSAGES_SNAPSHOT", "messages": [
            {"id": A, "role": "user", "content": "hi"},
            {"id": B, "role": "assistant", "content": "hello"}
        ]}),
    );
    assert_eq!(outcome.updates.len(), 2);
    assert!(matches!(outcome.updates[0], Update::TextFinished { .. }));
    match &outcome.updates[1] {
        Update::Transcript { messages } => assert_eq!(messages.len(), 2),
        other => panic!("unexpected update: {other:?}"),
    }
    assert!(matches!(
        outcome.anomalies[..],
        [Anomaly::ClosedEarly {
            by: EventType::MessagesSnapshot,
            ..
        }]
    ));
    assert_eq!(assembler.open_text_ids().count(), 0);
}

#[test]
fn events_without_transcript_meaning_produce_nothing_and_are_counted() {
    let mut assembler = Assembler::new();
    for payload in [
        json!({"type": "STATE_SNAPSHOT", "snapshot": {"k": 1}}),
        json!({"type": "STATE_DELTA", "delta": [{"op": "add", "path": "/k", "value": 2}]}),
        json!({"type": "RAW", "event": {"anything": true}}),
        json!({"type": "CUSTOM", "name": "surface.ask", "value": {"text": "q"}}),
        json!({"type": "ACTIVITY_SNAPSHOT", "messageId": A, "activityType": "diff", "content": {}}),
        json!({"type": "REASONING_ENCRYPTED_VALUE", "subtype": "message", "entityId": "e", "encryptedValue": "v"}),
    ] {
        let outcome = push(&mut assembler, payload);
        assert!(outcome.updates.is_empty());
        assert!(outcome.anomalies.is_empty());
    }
    assert_eq!(assembler.counters().events, 6);
    assert_eq!(assembler.counters().updates, 0);
}

#[test]
fn malformed_payloads_are_parse_errors_not_assembly_errors() {
    let mut assembler = Assembler::new();
    assert!(matches!(
        assembler.push_json("not json"),
        Err(PushError::Parse(_))
    ));
    assert!(matches!(
        assembler.push_json(r#"{"type":"NOT_AN_EVENT"}"#),
        Err(PushError::Parse(_))
    ));
    assert_eq!(assembler.counters().events, 0);
    assert_eq!(assembler.counters().errors, 0);
}

#[test]
fn every_spec_fixture_is_decided() {
    // One fixture per event type lives under tests/fixtures/events. Each is
    // pushed into a fresh assembler: the content-bearing ones with no start
    // are refused (their content would be lost); everything else is
    // accepted. A new fixture that is neither accepted nor in the refused
    // set fails this test until someone decides it.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events");
    let mut refused = BTreeSet::new();
    let mut accepted = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut assembler = Assembler::new();
        match assembler.push_json(&raw) {
            Ok(_) => accepted += 1,
            Err(PushError::Assembly(_)) => {
                refused.insert(name);
            }
            Err(PushError::Parse(error)) => panic!("{name} did not parse as an Event: {error}"),
        }
    }
    assert_eq!(
        refused,
        BTreeSet::from([
            "reasoning_message_content.json".to_string(),
            "text_message_content.json".to_string(),
            "thinking_text_message_content.json".to_string(),
            "tool_call_args.json".to_string(),
        ])
    );
    assert!(accepted >= 25, "only {accepted} fixtures accepted");
}

#[test]
fn every_upstream_event_type_has_a_decided_fixture() {
    // The compiler enforces that the assembler's match has no wildcard arm.
    // This ties the fixture sweep above to the vendored upstream spec set:
    // every event type upstream knows has a fixture here, so
    // `every_spec_fixture_is_decided` really does cover the spec.
    let snapshot: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/spec/event_types.snapshot.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/events");
    let missing: Vec<String> = snapshot["event_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_ascii_lowercase())
        .filter(|name| !dir.join(format!("{name}.json")).is_file())
        .collect();
    assert!(
        missing.is_empty(),
        "spec event types without a fixture: {missing:?}"
    );
    let _: fn(&Event) -> EventType = Event::event_type;
}

#[test]
fn wire_shape_of_updates_and_anomalies_is_kebab_kind_with_camel_fields() {
    // The browser client re-emits `kind` as a topic and reads the fields by
    // these names. Changing them is a client change.
    let update = Update::TextDelta {
        message_id: id(A),
        delta: "x".into(),
    };
    assert_eq!(
        serde_json::to_value(&update).unwrap(),
        json!({"kind": "text-delta", "messageId": A, "delta": "x"})
    );
    let finished = Update::TextFinished {
        message_id: id(A),
        role: Role::Assistant,
        name: None,
        text: "done".into(),
    };
    assert_eq!(
        serde_json::to_value(&finished).unwrap(),
        json!({"kind": "text-finished", "messageId": A, "role": "assistant", "text": "done"})
    );
    let anomaly = Anomaly::TextRestarted {
        message_id: id(A),
        dropped_chars: 3,
    };
    assert_eq!(
        serde_json::to_value(&anomaly).unwrap(),
        json!({"kind": "text-restarted", "messageId": A, "droppedChars": 3})
    );
    let error = AssemblyError::TextContentWithoutStart { message_id: id(A) };
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"kind": "text-content-without-start", "messageId": A})
    );
    assert_eq!(
        error.to_string(),
        format!("TEXT_MESSAGE_CONTENT for message {A} which was never started")
    );
}
