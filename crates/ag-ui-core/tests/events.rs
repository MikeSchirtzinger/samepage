//! Event-shape tests for the 2026-07 spec catch-up.
//!
//! Covers:
//! - the 9 event types newly added to close the gap against the current
//!   AG-UI spec (2 Activity events + 7 Reasoning events)
//! - the 4 in-place field-shape updates on existing events, including the
//!   one breaking change (`TextMessageChunkEvent.role` required -> optional)
//! - backward compatibility: legacy wire payloads (pre-catch-up shape) must
//!   still deserialize against the updated structs.
//!
//! Mirrors the round-trip / JSON-literal style used in `tests/unit.rs`.

#[cfg(test)]
mod tests {
    use ag_ui_core::event::{
        ActivityDeltaEvent, ActivitySnapshotEvent, BaseEvent, Event as AgUiEvent, EventType,
        ReasoningEncryptedValueEvent, ReasoningEncryptedValueSubtype, ReasoningEndEvent,
        ReasoningMessageChunkEvent, ReasoningMessageContentEvent, ReasoningMessageEndEvent,
        ReasoningMessageStartEvent, ReasoningStartEvent, RunFinishedEvent, RunFinishedOutcome,
        RunStartedEvent, TextMessageChunkEvent, TextMessageStartEvent,
    };
    use ag_ui_core::types::{Interrupt, MessageId, Role, RunId, ThreadId};
    use ag_ui_core::JsonValue;
    use serde_json::json;

    // ---------------------------------------------------------------
    // Missing events (additive): construct, round-trip, check `type` tag
    // and `event_type()`.
    // ---------------------------------------------------------------

    #[test]
    fn test_activity_snapshot_roundtrip() {
        let event = AgUiEvent::<JsonValue>::ActivitySnapshot(ActivitySnapshotEvent {
            base: BaseEvent::default(),
            message_id: MessageId::random(),
            activity_type: "code-diff".to_string(),
            content: json!({"path": "src/main.rs", "diff": "+1 -0"}),
            replace: true,
        });
        assert_eq!(event.event_type(), EventType::ActivitySnapshot);

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains(r#""type":"ACTIVITY_SNAPSHOT""#));
        assert!(json_str.contains(r#""activityType":"code-diff""#));

        let round_tripped: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn test_activity_snapshot_replace_defaults_true_when_omitted() {
        // Spec: `replace: z.boolean().optional().default(true)` — a producer
        // that omits `replace` on the wire must still deserialize, with the
        // field defaulting to `true`.
        let json_str = format!(
            r#"{{"type":"ACTIVITY_SNAPSHOT","messageId":"{}","activityType":"code-diff","content":{{}}}}"#,
            MessageId::random()
        );
        let event: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        match event {
            AgUiEvent::ActivitySnapshot(e) => assert!(e.replace),
            other => panic!("expected ActivitySnapshot, got {other:?}"),
        }
    }

    #[test]
    fn test_activity_delta_roundtrip() {
        let event = AgUiEvent::<JsonValue>::ActivityDelta(ActivityDeltaEvent {
            base: BaseEvent::default(),
            message_id: MessageId::random(),
            activity_type: "code-diff".to_string(),
            patch: vec![json!({"op": "replace", "path": "/diff", "value": "+2 -1"})],
        });
        assert_eq!(event.event_type(), EventType::ActivityDelta);

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains(r#""type":"ACTIVITY_DELTA""#));

        let round_tripped: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        assert_eq!(event, round_tripped);
    }

    #[test]
    fn test_reasoning_start_and_end_roundtrip() {
        let message_id = MessageId::random();

        let start = AgUiEvent::<JsonValue>::ReasoningStart(ReasoningStartEvent {
            base: BaseEvent::default(),
            message_id: message_id.clone(),
        });
        assert_eq!(start.event_type(), EventType::ReasoningStart);
        let start_json = serde_json::to_string(&start).unwrap();
        assert!(start_json.contains(r#""type":"REASONING_START""#));
        assert_eq!(start, serde_json::from_str(&start_json).unwrap());

        let end = AgUiEvent::<JsonValue>::ReasoningEnd(ReasoningEndEvent {
            base: BaseEvent::default(),
            message_id,
        });
        assert_eq!(end.event_type(), EventType::ReasoningEnd);
        let end_json = serde_json::to_string(&end).unwrap();
        assert!(end_json.contains(r#""type":"REASONING_END""#));
        assert_eq!(end, serde_json::from_str(&end_json).unwrap());
    }

    #[test]
    fn test_reasoning_message_start_role_is_reasoning() {
        let event = ReasoningMessageStartEvent {
            base: BaseEvent::default(),
            message_id: MessageId::random(),
            role: Role::Reasoning,
        };
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains(r#""role":"reasoning""#));

        let wrapped = AgUiEvent::<JsonValue>::ReasoningMessageStart(event);
        assert_eq!(wrapped.event_type(), EventType::ReasoningMessageStart);
    }

    #[test]
    fn test_reasoning_message_start_role_defaults_when_omitted() {
        // Mirrors TextMessageStartEvent's default-on-omit behavior.
        let json_str = format!(
            r#"{{"type":"REASONING_MESSAGE_START","messageId":"{}"}}"#,
            MessageId::random()
        );
        let event: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        match event {
            AgUiEvent::ReasoningMessageStart(e) => assert_eq!(e.role, Role::Reasoning),
            other => panic!("expected ReasoningMessageStart, got {other:?}"),
        }
    }

    #[test]
    fn test_reasoning_message_content_and_end_roundtrip() {
        let message_id = MessageId::random();

        let content =
            AgUiEvent::<JsonValue>::ReasoningMessageContent(ReasoningMessageContentEvent {
                base: BaseEvent::default(),
                message_id: message_id.clone(),
                delta: "Let me think about this...".to_string(),
            });
        assert_eq!(content.event_type(), EventType::ReasoningMessageContent);
        let json_str = serde_json::to_string(&content).unwrap();
        assert_eq!(content, serde_json::from_str(&json_str).unwrap());

        let end = AgUiEvent::<JsonValue>::ReasoningMessageEnd(ReasoningMessageEndEvent {
            base: BaseEvent::default(),
            message_id,
        });
        assert_eq!(end.event_type(), EventType::ReasoningMessageEnd);
    }

    #[test]
    fn test_reasoning_message_chunk_all_fields_optional() {
        // Mirrors TextMessageChunkEvent: every field but `type` is optional.
        let event: AgUiEvent =
            serde_json::from_str(r#"{"type":"REASONING_MESSAGE_CHUNK"}"#).unwrap();
        match event {
            AgUiEvent::ReasoningMessageChunk(e) => {
                assert!(e.message_id.is_none());
                assert!(e.delta.is_none());
            }
            other => panic!("expected ReasoningMessageChunk, got {other:?}"),
        }

        let full = AgUiEvent::<JsonValue>::ReasoningMessageChunk(ReasoningMessageChunkEvent {
            base: BaseEvent::default(),
            message_id: Some(MessageId::random()),
            delta: Some("partial".to_string()),
        });
        let json_str = serde_json::to_string(&full).unwrap();
        assert_eq!(full, serde_json::from_str(&json_str).unwrap());
    }

    #[test]
    fn test_reasoning_encrypted_value_roundtrip_and_subtypes() {
        for (subtype, expected) in [
            (
                ReasoningEncryptedValueSubtype::ToolCall,
                r#""subtype":"tool-call""#,
            ),
            (
                ReasoningEncryptedValueSubtype::Message,
                r#""subtype":"message""#,
            ),
        ] {
            let event =
                AgUiEvent::<JsonValue>::ReasoningEncryptedValue(ReasoningEncryptedValueEvent {
                    base: BaseEvent::default(),
                    subtype,
                    entity_id: "entity-123".to_string(),
                    encrypted_value: "opaque-blob".to_string(),
                });
            assert_eq!(event.event_type(), EventType::ReasoningEncryptedValue);
            let json_str = serde_json::to_string(&event).unwrap();
            assert!(
                json_str.contains(expected),
                "{json_str} should contain {expected}"
            );
            assert!(json_str.contains(r#""type":"REASONING_ENCRYPTED_VALUE""#));
            assert_eq!(event, serde_json::from_str(&json_str).unwrap());
        }
    }

    // ---------------------------------------------------------------
    // Changed events (in-place field-shape updates)
    // ---------------------------------------------------------------

    #[test]
    fn test_text_message_start_name_is_new_and_optional() {
        let with_name = TextMessageStartEvent::new(MessageId::random()).with_name("Ada");
        let json_str = serde_json::to_string(&with_name).unwrap();
        assert!(json_str.contains(r#""name":"Ada""#));

        let without_name = TextMessageStartEvent::new(MessageId::random());
        let json_str = serde_json::to_string(&without_name).unwrap();
        assert!(
            !json_str.contains("\"name\""),
            "name must be omitted when unset: {json_str}"
        );
    }

    #[test]
    fn test_text_message_start_role_defaults_to_assistant_when_omitted() {
        // Legacy producers, and the TS SDK's `.default("assistant")`, may
        // omit `role` on the wire.
        let json_str = format!(
            r#"{{"type":"TEXT_MESSAGE_START","messageId":"{}"}}"#,
            MessageId::random()
        );
        let event: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        match event {
            AgUiEvent::TextMessageStart(e) => assert_eq!(e.role, Role::Assistant),
            other => panic!("expected TextMessageStart, got {other:?}"),
        }
    }

    #[test]
    fn test_text_message_chunk_role_is_now_optional_breaking_change() {
        // BREAKING: `role` was `Role` (required), is now `Option<Role>`.
        // A chunk with no role at all must still deserialize.
        let json_str = r#"{"type":"TEXT_MESSAGE_CHUNK"}"#;
        let event: AgUiEvent = serde_json::from_str(json_str).unwrap();
        match event {
            AgUiEvent::TextMessageChunk(e) => {
                assert_eq!(e.role, None);
                assert_eq!(e.name, None);
            }
            other => panic!("expected TextMessageChunk, got {other:?}"),
        }

        // A chunk that does carry a role/name still round-trips.
        let full = AgUiEvent::<JsonValue>::TextMessageChunk(TextMessageChunkEvent {
            base: BaseEvent::default(),
            message_id: Some(MessageId::random()),
            role: Some(Role::Assistant),
            delta: Some("hi".to_string()),
            name: Some("Ada".to_string()),
        });
        let json_str = serde_json::to_string(&full).unwrap();
        assert_eq!(full, serde_json::from_str(&json_str).unwrap());
    }

    #[test]
    fn test_run_started_legacy_payload_still_deserializes() {
        // Pre-catch-up wire shape: no parentRunId, no input.
        let json_str = format!(
            r#"{{"type":"RUN_STARTED","threadId":"{}","runId":"{}"}}"#,
            ThreadId::random(),
            RunId::random()
        );
        let event: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        match event {
            AgUiEvent::RunStarted(e) => {
                assert!(e.parent_run_id.is_none());
                assert!(e.input.is_none());
            }
            other => panic!("expected RunStarted, got {other:?}"),
        }
    }

    #[test]
    fn test_run_started_with_parent_run_id_roundtrip() {
        let event = AgUiEvent::<JsonValue>::RunStarted(RunStartedEvent {
            base: BaseEvent::default(),
            thread_id: ThreadId::random(),
            run_id: RunId::random(),
            parent_run_id: Some(RunId::random()),
            input: None,
        });
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains("parentRunId"));
        assert_eq!(event, serde_json::from_str(&json_str).unwrap());
    }

    #[test]
    fn test_run_finished_legacy_payload_still_deserializes() {
        // Pre-catch-up wire shape: no outcome.
        let json_str = format!(
            r#"{{"type":"RUN_FINISHED","threadId":"{}","runId":"{}"}}"#,
            ThreadId::random(),
            RunId::random()
        );
        let event: AgUiEvent = serde_json::from_str(&json_str).unwrap();
        match event {
            AgUiEvent::RunFinished(e) => assert!(e.outcome.is_none()),
            other => panic!("expected RunFinished, got {other:?}"),
        }
    }

    #[test]
    fn test_run_finished_outcome_success_roundtrip() {
        let event = AgUiEvent::<JsonValue>::RunFinished(RunFinishedEvent {
            base: BaseEvent::default(),
            thread_id: ThreadId::random(),
            run_id: RunId::random(),
            result: None,
            outcome: Some(RunFinishedOutcome::Success),
        });
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains(r#""outcome":{"type":"success"}"#));
        assert_eq!(event, serde_json::from_str(&json_str).unwrap());
    }

    #[test]
    fn test_run_finished_outcome_interrupt_roundtrip_and_validation() {
        let interrupt = Interrupt::new("int-1", "needs_approval");
        let outcome = RunFinishedOutcome::Interrupt {
            interrupts: vec![interrupt],
        };
        assert!(outcome.validate().is_ok());

        let event = AgUiEvent::<JsonValue>::RunFinished(RunFinishedEvent {
            base: BaseEvent::default(),
            thread_id: ThreadId::random(),
            run_id: RunId::random(),
            result: None,
            outcome: Some(outcome),
        });
        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains(r#""type":"interrupt""#));
        assert_eq!(event, serde_json::from_str(&json_str).unwrap());

        // Spec invariant (min 1 interrupt) isn't expressible in the type
        // alone — verify the runtime check catches it.
        let empty = RunFinishedOutcome::Interrupt { interrupts: vec![] };
        assert!(empty.validate().is_err());
    }

    #[test]
    fn test_interrupt_optional_fields_roundtrip() {
        let interrupt = Interrupt {
            id: "int-42".to_string(),
            reason: "requires_human_review".to_string(),
            message: Some("Please confirm this action.".to_string()),
            tool_call_id: None,
            response_schema: Some(json!({"type": "boolean"})),
            expires_at: Some("2026-08-01T00:00:00Z".to_string()),
            metadata: Some(json!({"priority": "high"})),
        };
        let json_str = serde_json::to_string(&interrupt).unwrap();
        let round_tripped: Interrupt = serde_json::from_str(&json_str).unwrap();
        assert_eq!(interrupt, round_tripped);
    }

    // ---------------------------------------------------------------
    // Coverage: every new EventType round-trips through the tag string.
    // ---------------------------------------------------------------

    #[test]
    fn test_new_event_type_tags() {
        let cases = [
            (EventType::ActivitySnapshot, "\"ACTIVITY_SNAPSHOT\""),
            (EventType::ActivityDelta, "\"ACTIVITY_DELTA\""),
            (EventType::ReasoningStart, "\"REASONING_START\""),
            (
                EventType::ReasoningMessageStart,
                "\"REASONING_MESSAGE_START\"",
            ),
            (
                EventType::ReasoningMessageContent,
                "\"REASONING_MESSAGE_CONTENT\"",
            ),
            (EventType::ReasoningMessageEnd, "\"REASONING_MESSAGE_END\""),
            (
                EventType::ReasoningMessageChunk,
                "\"REASONING_MESSAGE_CHUNK\"",
            ),
            (EventType::ReasoningEnd, "\"REASONING_END\""),
            (
                EventType::ReasoningEncryptedValue,
                "\"REASONING_ENCRYPTED_VALUE\"",
            ),
        ];
        for (event_type, expected_tag) in cases {
            let json_str = serde_json::to_string(&event_type).unwrap();
            assert_eq!(json_str, expected_tag);
        }
    }
}
