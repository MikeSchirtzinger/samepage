//! Spec-conformance / drift-detection tests for `ag_ui_core::event`.
//!
//! `ag-ui-core` is a hand-maintained Rust port of the AG-UI protocol, whose
//! canonical source of truth is the Zod schemas in the TypeScript SDK
//! (`sdks/typescript/packages/core/src/events.ts` + `types.ts`, upstream repo
//! `ag-ui-protocol/ag-ui`). There's no mechanical link between the two, so
//! the port can silently drift — this file is the trip wire.
//!
//! Two complementary layers:
//!
//! 1. **Fixture round-trip conformance** (`fixture_round_trips`): one
//!    canonical JSON fixture per AG-UI spec event type, checked in under
//!    `tests/fixtures/events/`. Each fixture deserializes into
//!    `ag_ui_core::event::Event` and must re-serialize back to an
//!    order-insensitive-equal JSON value. This catches field renames, type
//!    changes, and missing/extra fields — including the shapes covered by
//!    the 2026-07 catch-up (`TEXT_MESSAGE_START`, `TEXT_MESSAGE_CHUNK`,
//!    `RUN_STARTED`, `RUN_FINISHED`, and the 9 new event types) alongside the
//!    24 event types that were already present.
//!
//! 2. **EventType coverage / drift alarm** (`event_type_matches_canonical_spec_snapshot`):
//!    asserts the Rust `EventType` enum — minus the 5 intentional TimeTravel
//!    extension variants — is exactly the 33-name set vendored from the
//!    upstream TS spec at `tests/fixtures/spec/event_types.snapshot.json`.
//!    If upstream adds, removes, or renames an event, refreshing that
//!    snapshot (see `tests/fixtures/spec/REFRESH.md`) and rerunning this
//!    test is what tells the maintainer to catch up — the exact signal that
//!    was missing before this file existed.
//!
//! Neither layer executes TypeScript: the snapshot is a checked-in fact, not
//! a live fetch, so this runs in ordinary Rust CI with no extra toolchain.
//!
//! Mirrors the round-trip / JSON-literal style used in `tests/events.rs` and
//! `tests/unit.rs`.

#[cfg(test)]
mod tests {
    use ag_ui_core::event::{Event as AgUiEvent, EventType};
    use ag_ui_core::JsonValue;
    use std::collections::BTreeSet;

    /// One `(name, fixture json)` pair per canonical AG-UI event fixture.
    ///
    /// `run_finished_interrupt` is a supplementary fixture covering
    /// `RUN_FINISHED`'s second `outcome` shape (`RunFinishedOutcome::Interrupt`)
    /// — it shares an `EventType` with `run_finished`, so
    /// `fixtures_cover_every_canonical_event_type` doesn't require it
    /// separately, but `fixture_round_trips` still exercises it.
    const FIXTURES: &[(&str, &str)] = &[
        (
            "text_message_start",
            include_str!("fixtures/events/text_message_start.json"),
        ),
        (
            "text_message_content",
            include_str!("fixtures/events/text_message_content.json"),
        ),
        (
            "text_message_end",
            include_str!("fixtures/events/text_message_end.json"),
        ),
        (
            "text_message_chunk",
            include_str!("fixtures/events/text_message_chunk.json"),
        ),
        (
            "thinking_text_message_start",
            include_str!("fixtures/events/thinking_text_message_start.json"),
        ),
        (
            "thinking_text_message_content",
            include_str!("fixtures/events/thinking_text_message_content.json"),
        ),
        (
            "thinking_text_message_end",
            include_str!("fixtures/events/thinking_text_message_end.json"),
        ),
        (
            "tool_call_start",
            include_str!("fixtures/events/tool_call_start.json"),
        ),
        (
            "tool_call_args",
            include_str!("fixtures/events/tool_call_args.json"),
        ),
        (
            "tool_call_end",
            include_str!("fixtures/events/tool_call_end.json"),
        ),
        (
            "tool_call_chunk",
            include_str!("fixtures/events/tool_call_chunk.json"),
        ),
        (
            "tool_call_result",
            include_str!("fixtures/events/tool_call_result.json"),
        ),
        (
            "thinking_start",
            include_str!("fixtures/events/thinking_start.json"),
        ),
        (
            "thinking_end",
            include_str!("fixtures/events/thinking_end.json"),
        ),
        (
            "state_snapshot",
            include_str!("fixtures/events/state_snapshot.json"),
        ),
        (
            "state_delta",
            include_str!("fixtures/events/state_delta.json"),
        ),
        (
            "messages_snapshot",
            include_str!("fixtures/events/messages_snapshot.json"),
        ),
        (
            "activity_snapshot",
            include_str!("fixtures/events/activity_snapshot.json"),
        ),
        (
            "activity_delta",
            include_str!("fixtures/events/activity_delta.json"),
        ),
        ("raw", include_str!("fixtures/events/raw.json")),
        ("custom", include_str!("fixtures/events/custom.json")),
        (
            "run_started",
            include_str!("fixtures/events/run_started.json"),
        ),
        (
            "run_finished",
            include_str!("fixtures/events/run_finished.json"),
        ),
        (
            "run_finished_interrupt",
            include_str!("fixtures/events/run_finished_interrupt.json"),
        ),
        ("run_error", include_str!("fixtures/events/run_error.json")),
        (
            "step_started",
            include_str!("fixtures/events/step_started.json"),
        ),
        (
            "step_finished",
            include_str!("fixtures/events/step_finished.json"),
        ),
        (
            "reasoning_start",
            include_str!("fixtures/events/reasoning_start.json"),
        ),
        (
            "reasoning_message_start",
            include_str!("fixtures/events/reasoning_message_start.json"),
        ),
        (
            "reasoning_message_content",
            include_str!("fixtures/events/reasoning_message_content.json"),
        ),
        (
            "reasoning_message_end",
            include_str!("fixtures/events/reasoning_message_end.json"),
        ),
        (
            "reasoning_message_chunk",
            include_str!("fixtures/events/reasoning_message_chunk.json"),
        ),
        (
            "reasoning_end",
            include_str!("fixtures/events/reasoning_end.json"),
        ),
        (
            "reasoning_encrypted_value",
            include_str!("fixtures/events/reasoning_encrypted_value.json"),
        ),
    ];

    /// Brevity's TimeTravel extension events (`Event::TimeTravel`). These are
    /// an intentional, documented extension to the AG-UI protocol — not part
    /// of the upstream spec — so they're allow-listed here rather than
    /// counted as drift. See `event.rs`'s `TimeTravelEvent` doc comment.
    const TIME_TRAVEL_EXTENSIONS: &[&str] = &[
        "CHECKPOINT_CREATED",
        "REWIND_INITIATED",
        "REPLAY_STARTED",
        "REPLAY_PROGRESS",
        "TIMELINE_SNAPSHOT",
    ];

    #[derive(serde::Deserialize)]
    struct SpecSnapshot {
        event_types: Vec<String>,
    }

    fn canonical_spec_event_types() -> BTreeSet<String> {
        let snapshot: SpecSnapshot =
            serde_json::from_str(include_str!("fixtures/spec/event_types.snapshot.json"))
                .expect("vendored spec snapshot must be valid JSON — see fixtures/spec/REFRESH.md");
        snapshot.event_types.into_iter().collect()
    }

    /// `EventType`'s wire tag, e.g. `EventType::TextMessageStart` -> `"TEXT_MESSAGE_START"`.
    fn event_type_tag(event_type: EventType) -> String {
        serde_json::to_string(&event_type)
            .unwrap()
            .trim_matches('"')
            .to_string()
    }

    /// Every `EventType` variant's wire tag, exhaustively.
    ///
    /// The `match` below is the enforcement mechanism, not documentation: if
    /// `EventType` gains, loses, or renames a variant without this function
    /// being updated to match, the crate fails to compile. That's what makes
    /// `event_type_matches_canonical_spec_snapshot` a reliable drift alarm
    /// rather than a check that can silently go stale on its own.
    fn all_event_type_tags() -> Vec<String> {
        use EventType::*;
        let variants = [
            TextMessageStart,
            TextMessageContent,
            TextMessageEnd,
            TextMessageChunk,
            ThinkingTextMessageStart,
            ThinkingTextMessageContent,
            ThinkingTextMessageEnd,
            ToolCallStart,
            ToolCallArgs,
            ToolCallEnd,
            ToolCallChunk,
            ToolCallResult,
            ThinkingStart,
            ThinkingEnd,
            StateSnapshot,
            StateDelta,
            MessagesSnapshot,
            Raw,
            Custom,
            RunStarted,
            RunFinished,
            RunError,
            StepStarted,
            StepFinished,
            ActivitySnapshot,
            ActivityDelta,
            ReasoningStart,
            ReasoningMessageStart,
            ReasoningMessageContent,
            ReasoningMessageEnd,
            ReasoningMessageChunk,
            ReasoningEnd,
            ReasoningEncryptedValue,
            CheckpointCreated,
            RewindInitiated,
            ReplayStarted,
            ReplayProgress,
            TimelineSnapshot,
        ];
        for variant in variants {
            match variant {
                TextMessageStart
                | TextMessageContent
                | TextMessageEnd
                | TextMessageChunk
                | ThinkingTextMessageStart
                | ThinkingTextMessageContent
                | ThinkingTextMessageEnd
                | ToolCallStart
                | ToolCallArgs
                | ToolCallEnd
                | ToolCallChunk
                | ToolCallResult
                | ThinkingStart
                | ThinkingEnd
                | StateSnapshot
                | StateDelta
                | MessagesSnapshot
                | Raw
                | Custom
                | RunStarted
                | RunFinished
                | RunError
                | StepStarted
                | StepFinished
                | ActivitySnapshot
                | ActivityDelta
                | ReasoningStart
                | ReasoningMessageStart
                | ReasoningMessageContent
                | ReasoningMessageEnd
                | ReasoningMessageChunk
                | ReasoningEnd
                | ReasoningEncryptedValue
                | CheckpointCreated
                | RewindInitiated
                | ReplayStarted
                | ReplayProgress
                | TimelineSnapshot => {}
            }
        }
        variants.into_iter().map(event_type_tag).collect()
    }

    // ---------------------------------------------------------------
    // Layer 1: fixture round-trip conformance.
    // ---------------------------------------------------------------

    #[test]
    fn fixture_round_trips() {
        for (name, raw) in FIXTURES {
            let original: JsonValue = serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("fixture `{name}` is not valid JSON: {e}"));
            let event: AgUiEvent = serde_json::from_str(raw).unwrap_or_else(|e| {
                panic!("fixture `{name}` failed to deserialize into ag_ui_core::event::Event: {e}")
            });
            let round_tripped = serde_json::to_value(&event)
                .unwrap_or_else(|e| panic!("fixture `{name}` failed to re-serialize: {e}"));
            assert_eq!(
                original, round_tripped,
                "fixture `{name}` did not round-trip losslessly \
                 (deserialize -> serialize changed the JSON shape)"
            );
        }
    }

    #[test]
    fn fixtures_cover_every_canonical_event_type() {
        let canonical = canonical_spec_event_types();
        let covered: BTreeSet<String> = FIXTURES
            .iter()
            .map(|(name, raw)| {
                let event: AgUiEvent = serde_json::from_str(raw)
                    .unwrap_or_else(|e| panic!("fixture `{name}` failed to parse: {e}"));
                event_type_tag(event.event_type())
            })
            .collect();
        let missing: Vec<_> = canonical.difference(&covered).collect();
        assert!(
            missing.is_empty(),
            "no fixture covers these canonical AG-UI event types: {missing:?} \
             — add a JSON fixture under tests/fixtures/events/"
        );
    }

    // ---------------------------------------------------------------
    // Layer 2: EventType coverage / drift alarm.
    // ---------------------------------------------------------------

    #[test]
    fn event_type_matches_canonical_spec_snapshot() {
        let canonical = canonical_spec_event_types();
        let rust_spec_types: BTreeSet<String> = all_event_type_tags()
            .into_iter()
            .filter(|tag| !TIME_TRAVEL_EXTENSIONS.contains(&tag.as_str()))
            .collect();

        let missing_from_rust: Vec<_> = canonical.difference(&rust_spec_types).collect();
        let extra_in_rust: Vec<_> = rust_spec_types.difference(&canonical).collect();

        assert!(
            missing_from_rust.is_empty() && extra_in_rust.is_empty(),
            "EventType has drifted from the vendored upstream spec snapshot \
             (tests/fixtures/spec/event_types.snapshot.json) — \
             missing from Rust: {missing_from_rust:?}, \
             present in Rust but not in the spec snapshot: {extra_in_rust:?}. \
             If this is real upstream drift, do a catch-up pass; if the \
             snapshot is just stale, see tests/fixtures/spec/REFRESH.md."
        );
    }
}
