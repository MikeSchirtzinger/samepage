//! Stream assembly: the AG-UI lifecycle state machine, in one place.
//!
//! Every browser app built on this workspace had its own copy of the same
//! five-case string switch: `TEXT_MESSAGE_START` opens a bubble,
//! `TEXT_MESSAGE_CONTENT` appends, `TEXT_MESSAGE_END` forgets the id. Five
//! cases, decided by string comparison in JavaScript, against a protocol that
//! defines thirty-five event types. The other thirty were not handled, they
//! were not *decided*: a content delta for an id nobody opened silently grew a
//! bubble, a chunk event was dropped on the floor, a run that ended with a
//! message still open left that message open forever.
//!
//! [`Assembler`] is that decision, made once, in Rust, over the typed
//! [`Event`] enum. Its `match` has no wildcard arm: adding a variant to
//! [`Event`] is a compile error here until someone says what it means for the
//! transcript. It compiles natively for tests and to wasm32 for the browser,
//! so the state machine the page runs is the one the test suite proves.
//!
//! ## Contract
//!
//! [`Assembler::push`] consumes one event and returns an [`Outcome`]: the
//! [`Update`]s a transcript can act on (a message started, grew, finished; a
//! tool call assembled; the transcript replaced) and the [`Anomaly`]s it
//! tolerated. It returns an [`AssemblyError`] only when accepting the event
//! would **lose content**: a delta for a message that was never started has
//! nowhere to go. Everything else that violates the spec but loses nothing (an
//! end without a start, a duplicate start, a run that ends with messages open)
//! is repaired, reported as an anomaly, and counted.
//!
//! The one deliberate departure from the strict TS-SDK verifier: a
//! `TEXT_MESSAGE_START` for an id that is already open **resets** that
//! message instead of erroring. The runtime replays exactly that on reconnect
//! (`surface.history`, then `START` + one `CONTENT` carrying the whole text so
//! far), and a reconnect projection is a full idempotent view, not an append.
//!
//! Chunk events (`TEXT_MESSAGE_CHUNK`, `TOOL_CALL_CHUNK`,
//! `REASONING_MESSAGE_CHUNK`) follow the spec's transform: a chunk with a new
//! id opens the message, a chunk without an id continues the last one, and any
//! non-chunk event finishes whatever the chunks opened.
//!
//! Events that carry no transcript meaning (`STATE_*`, `CUSTOM`, `RAW`,
//! `ACTIVITY_*`, time travel) produce no update. A client delivers those
//! to subscribers as the already-validated typed event; this module is not a
//! second copy of that routing.

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventType, RunFinishedOutcome};
use crate::types::{FunctionCall, Message, MessageId, Role, RunId, ThreadId, ToolCall, ToolCallId};
use crate::AgentState;

/// One assembled fact a transcript can act on.
///
/// Serialized with a `kind` tag in kebab-case and camelCase fields, which is
/// the shape the browser client re-emits as topics (`transcript:text-delta`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Update {
    /// A text message opened. `role` defaults to assistant on the wire.
    TextStarted {
        message_id: MessageId,
        role: Role,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A text message grew by `delta`. The accumulated text is available from
    /// [`Assembler::text`]; it is not repeated here so a long message does not
    /// cross the boundary quadratically.
    TextDelta {
        message_id: MessageId,
        delta: String,
    },
    /// A text message is complete. `text` is the whole message.
    TextFinished {
        message_id: MessageId,
        role: Role,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        text: String,
    },
    /// A tool call opened; its arguments will stream.
    ToolCallStarted {
        tool_call_id: ToolCallId,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<MessageId>,
    },
    /// A tool call's argument string grew by `delta`.
    ToolCallArgs {
        tool_call_id: ToolCallId,
        delta: String,
    },
    /// A tool call is complete, with its arguments assembled.
    ToolCallFinished {
        tool_call: ToolCall,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<MessageId>,
    },
    /// A tool produced its result.
    ToolCallResult {
        message_id: MessageId,
        tool_call_id: ToolCallId,
        content: String,
    },
    /// A reasoning message opened (`REASONING_MESSAGE_START` or the first
    /// `REASONING_MESSAGE_CHUNK` for an id).
    ReasoningStarted { message_id: MessageId },
    /// A reasoning message grew by `delta`.
    ReasoningDelta {
        message_id: MessageId,
        delta: String,
    },
    /// A reasoning message is complete.
    ReasoningFinished { message_id: MessageId, text: String },
    /// A legacy thinking phase opened (`THINKING_START`).
    ThinkingStarted {
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// The legacy thinking text grew by `delta`.
    ThinkingDelta { delta: String },
    /// A legacy thinking text is complete (`THINKING_TEXT_MESSAGE_END`), or
    /// the phase closed (`THINKING_END`) with `text` empty when no
    /// `THINKING_TEXT_MESSAGE_*` content was streamed. `title` is the phase's.
    ThinkingFinished {
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        text: String,
    },
    /// `MESSAGES_SNAPSHOT`: the transcript is replaced wholesale.
    Transcript { messages: Vec<Message> },
}

/// A spec violation the assembler repaired without losing content.
///
/// Anomalies are returned, not swallowed: a client that sees one can log it,
/// count it, or decide to resync. They are the observability of this
/// mechanism; a healthy stream produces none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Anomaly {
    /// `TEXT_MESSAGE_START` for an id already open. The message was reset;
    /// `dropped_chars` is how much accumulated text that discarded (zero for
    /// the runtime's reconnect replay, which resends the whole text next).
    TextRestarted {
        message_id: MessageId,
        dropped_chars: usize,
    },
    /// `TEXT_MESSAGE_END` for an id that was not open. Nothing to close.
    TextEndWithoutStart { message_id: MessageId },
    /// A content event carried an empty delta. The spec forbids it; nothing
    /// was appended.
    EmptyDelta { event: EventType },
    /// `TOOL_CALL_START` for an id already open. The call was reset.
    ToolCallRestarted {
        tool_call_id: ToolCallId,
        dropped_chars: usize,
    },
    /// `TOOL_CALL_END` for an id that was not open.
    ToolCallEndWithoutStart { tool_call_id: ToolCallId },
    /// `REASONING_MESSAGE_START` for an id already open. The text was reset.
    ReasoningRestarted {
        message_id: MessageId,
        dropped_chars: usize,
    },
    /// `REASONING_MESSAGE_END` for an id that was not open.
    ReasoningEndWithoutStart { message_id: MessageId },
    /// `REASONING_START` twice for one id, or `REASONING_END` for an id whose
    /// `REASONING_START` was never seen.
    ReasoningBracketMismatch {
        message_id: MessageId,
        event: EventType,
    },
    /// `THINKING_START` while a thinking phase was open, or
    /// `THINKING_TEXT_MESSAGE_START` while thinking text was open. The open
    /// phase or text was finished first.
    ThinkingRestarted { event: EventType },
    /// `THINKING_END` / `THINKING_TEXT_MESSAGE_END` with nothing open.
    ThinkingEndWithoutStart { event: EventType },
    /// `RUN_STARTED` while a run was already in progress.
    RunRestarted { previous_run_id: RunId },
    /// `RUN_FINISHED` / `RUN_ERROR` with no run in progress. This is normal
    /// for a client that connected mid-run, and for a runtime that does not
    /// bracket its turns in runs at all.
    RunEndWithoutStart { event: EventType },
    /// A run ended (or the transcript was replaced) while these were still
    /// streaming. Each was finished with the content it had; the matching
    /// `*Finished` updates precede this anomaly in the outcome.
    ClosedEarly {
        by: EventType,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        text_message_ids: Vec<MessageId>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        tool_call_ids: Vec<ToolCallId>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        reasoning_message_ids: Vec<MessageId>,
        thinking: bool,
    },
    /// `STEP_STARTED` for a step name already open.
    StepRestarted { step_name: String },
    /// `STEP_FINISHED` for a step name that was not open.
    StepFinishedWithoutStart { step_name: String },
}

/// Accepting the event would lose content. The assembler's state is unchanged
/// when this is returned; the caller decides whether to resync (reconnect and
/// take the replay) or stop.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum AssemblyError {
    #[error("TEXT_MESSAGE_CONTENT for message {message_id} which was never started")]
    TextContentWithoutStart { message_id: MessageId },
    #[error("TEXT_MESSAGE_CHUNK carried content but no messageId and no chunked message is open")]
    TextChunkWithoutMessageId,
    #[error("TOOL_CALL_ARGS for tool call {tool_call_id} which was never started")]
    ToolCallArgsWithoutStart { tool_call_id: ToolCallId },
    #[error(
        "TOOL_CALL_CHUNK carried arguments but no toolCallId and no chunked tool call is open"
    )]
    ToolCallChunkWithoutId,
    #[error("TOOL_CALL_CHUNK opened tool call {tool_call_id} without a toolCallName")]
    ToolCallChunkWithoutName { tool_call_id: ToolCallId },
    #[error("REASONING_MESSAGE_CONTENT for message {message_id} which was never started")]
    ReasoningContentWithoutStart { message_id: MessageId },
    #[error("REASONING_MESSAGE_CHUNK carried content but no messageId and no chunked reasoning message is open")]
    ReasoningChunkWithoutMessageId,
    #[error("THINKING_TEXT_MESSAGE_CONTENT with no thinking text open")]
    ThinkingContentWithoutStart,
}

/// What one event did to the transcript.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Outcome {
    pub updates: Vec<Update>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub anomalies: Vec<Anomaly>,
}

/// A failed [`Assembler::push_json`]: the payload was not a valid AG-UI event,
/// or it was and accepting it would lose content.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("invalid AG-UI event: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Assembly(#[from] AssemblyError),
}

/// Mechanism counters, so a page can show that the assembler is doing the
/// work and how often the stream needed repair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    /// Events accepted (including those that produced no update).
    pub events: u64,
    /// Updates produced.
    pub updates: u64,
    /// Anomalies repaired.
    pub anomalies: u64,
    /// Events refused with an [`AssemblyError`].
    pub errors: u64,
}

/// Where the run lifecycle stands. Runs are tracked, not required: a runtime
/// that streams messages outside `RUN_STARTED`/`RUN_FINISHED` brackets is
/// accepted, and only end-without-start is reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "phase",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum RunPhase {
    Idle,
    Running {
        thread_id: ThreadId,
        run_id: RunId,
    },
    Finished {
        thread_id: ThreadId,
        run_id: RunId,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<RunFinishedOutcome>,
    },
    Errored {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

/// Whether an open item was declared with an explicit `*_START` event or
/// opened implicitly by a `*_CHUNK`. Only chunk-opened items are finished
/// implicitly by the next non-chunk event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Explicit,
    Chunk,
}

#[derive(Debug, Clone)]
struct OpenText {
    id: MessageId,
    role: Role,
    name: Option<String>,
    text: String,
    origin: Origin,
}

#[derive(Debug, Clone)]
struct OpenToolCall {
    id: ToolCallId,
    name: String,
    parent_message_id: Option<MessageId>,
    arguments: String,
    origin: Origin,
}

#[derive(Debug, Clone)]
struct OpenReasoning {
    id: MessageId,
    text: String,
    origin: Origin,
}

#[derive(Debug, Clone, Default)]
struct OpenThinking {
    title: Option<String>,
    /// `Some` once `THINKING_TEXT_MESSAGE_START` has been seen.
    text: Option<String>,
}

/// The AG-UI lifecycle state machine. See the module docs for the contract.
///
/// Open items are kept in small insertion-ordered vectors rather than maps:
/// a transcript has one, occasionally two, messages streaming at once, and
/// insertion order is what makes [`Anomaly::ClosedEarly`] deterministic.
#[derive(Debug, Clone, Default)]
pub struct Assembler {
    texts: Vec<OpenText>,
    tool_calls: Vec<OpenToolCall>,
    reasoning: Vec<OpenReasoning>,
    /// `REASONING_START` ids awaiting `REASONING_END`.
    reasoning_brackets: Vec<MessageId>,
    thinking: Option<OpenThinking>,
    run: Option<RunPhase>,
    steps: Vec<String>,
    counters: Counters,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget every open item and the run phase. Counters are kept; they
    /// describe the session, not the stream.
    pub fn reset(&mut self) {
        let counters = self.counters;
        *self = Self {
            counters,
            ..Self::default()
        };
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn run(&self) -> RunPhase {
        self.run.clone().unwrap_or(RunPhase::Idle)
    }

    /// The accumulated text of an open message, if `id` is open.
    pub fn text(&self, id: &MessageId) -> Option<&str> {
        self.texts
            .iter()
            .find(|open| open.id == *id)
            .map(|open| open.text.as_str())
    }

    /// Ids of the text messages currently streaming, oldest first.
    pub fn open_text_ids(&self) -> impl Iterator<Item = &MessageId> {
        self.texts.iter().map(|open| &open.id)
    }

    /// Ids of the tool calls currently streaming, oldest first.
    pub fn open_tool_call_ids(&self) -> impl Iterator<Item = &ToolCallId> {
        self.tool_calls.iter().map(|open| &open.id)
    }

    /// Parse one wire payload (the `data:` of an SSE message) as a typed event
    /// and push it. Returns the event alongside the outcome so a client can
    /// deliver the validated event to raw subscribers without parsing twice.
    pub fn push_json(&mut self, raw: &str) -> Result<(Event, Outcome), PushError> {
        let event: Event = serde_json::from_str(raw)?;
        let outcome = self.push(&event)?;
        Ok((event, outcome))
    }

    /// Consume one event. On `Err`, the assembler is unchanged apart from
    /// `counters().errors`: every refusal is decided by [`Self::check`]
    /// before any state moves.
    pub fn push<S: AgentState>(&mut self, event: &Event<S>) -> Result<Outcome, AssemblyError> {
        if let Err(error) = self.check(event) {
            self.counters.errors += 1;
            return Err(error);
        }
        let mut outcome = Outcome::default();
        // Chunk-opened items live only as long as the chunk run that opened
        // them. Any event that is not a chunk of the same family finishes
        // them, before the event itself is applied.
        self.finish_chunked_except(event.event_type(), &mut outcome);

        let applied = match event {
            Event::TextMessageStart(e) => {
                self.text_start(
                    e.message_id.clone(),
                    e.role.clone(),
                    e.name.clone(),
                    Origin::Explicit,
                    &mut outcome,
                );
                Ok(())
            }
            Event::TextMessageContent(e) => self.text_content(
                &e.message_id,
                &e.delta,
                EventType::TextMessageContent,
                &mut outcome,
            ),
            Event::TextMessageEnd(e) => {
                self.text_end(&e.message_id, &mut outcome);
                Ok(())
            }
            Event::TextMessageChunk(e) => {
                // `check` already refused a content-bearing chunk with nothing
                // to attach to; an id-less chunk left here carries nothing.
                match e.message_id.clone().or_else(|| self.last_chunked_text()) {
                    None => Ok(()),
                    Some(id) => {
                        if self.text_index(&id).is_none() {
                            let role = e.role.clone().unwrap_or(Role::Assistant);
                            self.text_start(
                                id.clone(),
                                role,
                                e.name.clone(),
                                Origin::Chunk,
                                &mut outcome,
                            );
                        }
                        match &e.delta {
                            Some(delta) => self.text_content(
                                &id,
                                delta,
                                EventType::TextMessageChunk,
                                &mut outcome,
                            ),
                            None => Ok(()),
                        }
                    }
                }
            }
            Event::ThinkingTextMessageStart(_) => {
                let thinking = self.thinking.get_or_insert_with(OpenThinking::default);
                if let Some(text) = thinking.text.take() {
                    outcome.anomalies.push(Anomaly::ThinkingRestarted {
                        event: EventType::ThinkingTextMessageStart,
                    });
                    outcome.updates.push(Update::ThinkingFinished {
                        title: thinking.title.clone(),
                        text,
                    });
                }
                thinking.text = Some(String::new());
                Ok(())
            }
            Event::ThinkingTextMessageContent(e) => {
                match self.thinking.as_mut().and_then(|t| t.text.as_mut()) {
                    None => Err(AssemblyError::ThinkingContentWithoutStart),
                    Some(text) => {
                        if e.delta.is_empty() {
                            outcome.anomalies.push(Anomaly::EmptyDelta {
                                event: EventType::ThinkingTextMessageContent,
                            });
                        } else {
                            text.push_str(&e.delta);
                            outcome.updates.push(Update::ThinkingDelta {
                                delta: e.delta.clone(),
                            });
                        }
                        Ok(())
                    }
                }
            }
            Event::ThinkingTextMessageEnd(_) => {
                let title = self.thinking.as_ref().and_then(|t| t.title.clone());
                match self.thinking.as_mut().and_then(|t| t.text.take()) {
                    Some(text) => outcome
                        .updates
                        .push(Update::ThinkingFinished { title, text }),
                    None => outcome.anomalies.push(Anomaly::ThinkingEndWithoutStart {
                        event: EventType::ThinkingTextMessageEnd,
                    }),
                }
                Ok(())
            }
            Event::ToolCallStart(e) => {
                self.tool_call_start(
                    e.tool_call_id.clone(),
                    e.tool_call_name.clone(),
                    e.parent_message_id.clone(),
                    Origin::Explicit,
                    &mut outcome,
                );
                Ok(())
            }
            Event::ToolCallArgs(e) => self.tool_call_args(
                &e.tool_call_id,
                &e.delta,
                EventType::ToolCallArgs,
                &mut outcome,
            ),
            Event::ToolCallEnd(e) => {
                self.tool_call_end(&e.tool_call_id, &mut outcome);
                Ok(())
            }
            Event::ToolCallChunk(e) => {
                match e
                    .tool_call_id
                    .clone()
                    .or_else(|| self.last_chunked_tool_call())
                {
                    None => Ok(()),
                    Some(id) => {
                        let opened = match (self.tool_call_index(&id), e.tool_call_name.clone()) {
                            (Some(_), _) => Ok(()),
                            (None, Some(name)) => {
                                self.tool_call_start(
                                    id.clone(),
                                    name,
                                    e.parent_message_id.clone(),
                                    Origin::Chunk,
                                    &mut outcome,
                                );
                                Ok(())
                            }
                            (None, None) => Err(AssemblyError::ToolCallChunkWithoutName {
                                tool_call_id: id.clone(),
                            }),
                        };
                        match (opened, &e.delta) {
                            (Err(error), _) => Err(error),
                            (Ok(()), Some(delta)) => self.tool_call_args(
                                &id,
                                delta,
                                EventType::ToolCallChunk,
                                &mut outcome,
                            ),
                            (Ok(()), None) => Ok(()),
                        }
                    }
                }
            }
            Event::ToolCallResult(e) => {
                outcome.updates.push(Update::ToolCallResult {
                    message_id: e.message_id.clone(),
                    tool_call_id: e.tool_call_id.clone(),
                    content: e.content.clone(),
                });
                Ok(())
            }
            Event::ThinkingStart(e) => {
                if let Some(open) = self.thinking.take() {
                    outcome.anomalies.push(Anomaly::ThinkingRestarted {
                        event: EventType::ThinkingStart,
                    });
                    outcome.updates.push(Update::ThinkingFinished {
                        title: open.title,
                        text: open.text.unwrap_or_default(),
                    });
                }
                self.thinking = Some(OpenThinking {
                    title: e.title.clone(),
                    text: None,
                });
                outcome.updates.push(Update::ThinkingStarted {
                    title: e.title.clone(),
                });
                Ok(())
            }
            Event::ThinkingEnd(_) => {
                match self.thinking.take() {
                    Some(open) => outcome.updates.push(Update::ThinkingFinished {
                        title: open.title,
                        text: open.text.unwrap_or_default(),
                    }),
                    None => outcome.anomalies.push(Anomaly::ThinkingEndWithoutStart {
                        event: EventType::ThinkingEnd,
                    }),
                }
                Ok(())
            }
            // Agent state is the client's to hold; the assembler owns the
            // transcript only. Delivered to subscribers as the typed event.
            Event::StateSnapshot(_) | Event::StateDelta(_) => Ok(()),
            Event::MessagesSnapshot(e) => {
                self.close_all(EventType::MessagesSnapshot, &mut outcome);
                outcome.updates.push(Update::Transcript {
                    messages: e.messages.clone(),
                });
                Ok(())
            }
            // Application-defined and pass-through payloads carry no lifecycle.
            Event::Raw(_) | Event::Custom(_) => Ok(()),
            Event::RunStarted(e) => {
                if let Some(RunPhase::Running { run_id, .. }) = &self.run {
                    outcome.anomalies.push(Anomaly::RunRestarted {
                        previous_run_id: run_id.clone(),
                    });
                }
                self.run = Some(RunPhase::Running {
                    thread_id: e.thread_id.clone(),
                    run_id: e.run_id.clone(),
                });
                Ok(())
            }
            Event::RunFinished(e) => {
                self.close_all(EventType::RunFinished, &mut outcome);
                if !matches!(self.run, Some(RunPhase::Running { .. })) {
                    outcome.anomalies.push(Anomaly::RunEndWithoutStart {
                        event: EventType::RunFinished,
                    });
                }
                self.run = Some(RunPhase::Finished {
                    thread_id: e.thread_id.clone(),
                    run_id: e.run_id.clone(),
                    outcome: e.outcome.clone(),
                });
                Ok(())
            }
            Event::RunError(e) => {
                self.close_all(EventType::RunError, &mut outcome);
                if !matches!(self.run, Some(RunPhase::Running { .. })) {
                    outcome.anomalies.push(Anomaly::RunEndWithoutStart {
                        event: EventType::RunError,
                    });
                }
                self.run = Some(RunPhase::Errored {
                    message: e.message.clone(),
                    code: e.code.clone(),
                });
                Ok(())
            }
            Event::StepStarted(e) => {
                if self.steps.contains(&e.step_name) {
                    outcome.anomalies.push(Anomaly::StepRestarted {
                        step_name: e.step_name.clone(),
                    });
                } else {
                    self.steps.push(e.step_name.clone());
                }
                Ok(())
            }
            Event::StepFinished(e) => {
                match self.steps.iter().position(|name| *name == e.step_name) {
                    Some(index) => {
                        self.steps.remove(index);
                    }
                    None => outcome.anomalies.push(Anomaly::StepFinishedWithoutStart {
                        step_name: e.step_name.clone(),
                    }),
                }
                Ok(())
            }
            // Activity payloads are message-attached JSON the core `Message`
            // type does not yet model; delivered as the typed event.
            Event::ActivitySnapshot(_) | Event::ActivityDelta(_) => Ok(()),
            Event::ReasoningStart(e) => {
                if self.reasoning_brackets.contains(&e.message_id) {
                    outcome.anomalies.push(Anomaly::ReasoningBracketMismatch {
                        message_id: e.message_id.clone(),
                        event: EventType::ReasoningStart,
                    });
                } else {
                    self.reasoning_brackets.push(e.message_id.clone());
                }
                Ok(())
            }
            Event::ReasoningMessageStart(e) => {
                self.reasoning_start(e.message_id.clone(), Origin::Explicit, &mut outcome);
                Ok(())
            }
            Event::ReasoningMessageContent(e) => self.reasoning_content(
                &e.message_id,
                &e.delta,
                EventType::ReasoningMessageContent,
                &mut outcome,
            ),
            Event::ReasoningMessageEnd(e) => {
                self.reasoning_end(&e.message_id, &mut outcome);
                Ok(())
            }
            Event::ReasoningMessageChunk(e) => {
                match e
                    .message_id
                    .clone()
                    .or_else(|| self.last_chunked_reasoning())
                {
                    None => Ok(()),
                    Some(id) => {
                        if self.reasoning_index(&id).is_none() {
                            self.reasoning_start(id.clone(), Origin::Chunk, &mut outcome);
                        }
                        match &e.delta {
                            Some(delta) => self.reasoning_content(
                                &id,
                                delta,
                                EventType::ReasoningMessageChunk,
                                &mut outcome,
                            ),
                            None => Ok(()),
                        }
                    }
                }
            }
            Event::ReasoningEnd(e) => {
                // A reasoning message left open inside its bracket is finished
                // by the bracket closing; the content is kept.
                if self.reasoning_index(&e.message_id).is_some() {
                    self.reasoning_end(&e.message_id, &mut outcome);
                }
                match self
                    .reasoning_brackets
                    .iter()
                    .position(|id| *id == e.message_id)
                {
                    Some(index) => {
                        self.reasoning_brackets.remove(index);
                    }
                    None => outcome.anomalies.push(Anomaly::ReasoningBracketMismatch {
                        message_id: e.message_id.clone(),
                        event: EventType::ReasoningEnd,
                    }),
                }
                Ok(())
            }
            // Opaque provider material to round-trip, never to render.
            Event::ReasoningEncryptedValue(_) => Ok(()),
            // Orchestration timeline; no transcript meaning.
            Event::TimeTravel(_) => Ok(()),
        };

        match applied {
            Ok(()) => Ok(self.record(outcome)),
            // Unreachable by construction (`check` runs first); kept typed
            // rather than asserted so a future arm cannot turn it into a panic.
            Err(error) => {
                self.counters.errors += 1;
                Err(error)
            }
        }
    }

    /// Every refusal, decided against unmodified state. An id opened by a
    /// chunk run counts as open only for another chunk of the same family:
    /// the non-chunk event that arrives next finishes the run first, so a
    /// `TEXT_MESSAGE_CONTENT` for a chunk-opened id has nowhere to go.
    fn check<S: AgentState>(&self, event: &Event<S>) -> Result<(), AssemblyError> {
        let carries = |delta: &Option<String>| delta.as_deref().is_some_and(|d| !d.is_empty());
        match event {
            Event::TextMessageContent(e)
                if !self.text_open_for(&e.message_id, EventType::TextMessageContent) =>
            {
                Err(AssemblyError::TextContentWithoutStart {
                    message_id: e.message_id.clone(),
                })
            }
            Event::TextMessageChunk(e)
                if e.message_id.is_none()
                    && self.last_chunked_text().is_none()
                    && carries(&e.delta) =>
            {
                Err(AssemblyError::TextChunkWithoutMessageId)
            }
            Event::ToolCallArgs(e)
                if !self.tool_call_open_for(&e.tool_call_id, EventType::ToolCallArgs) =>
            {
                Err(AssemblyError::ToolCallArgsWithoutStart {
                    tool_call_id: e.tool_call_id.clone(),
                })
            }
            Event::ToolCallChunk(e) => match e
                .tool_call_id
                .clone()
                .or_else(|| self.last_chunked_tool_call())
            {
                None if carries(&e.delta) => Err(AssemblyError::ToolCallChunkWithoutId),
                Some(id) if self.tool_call_index(&id).is_none() && e.tool_call_name.is_none() => {
                    Err(AssemblyError::ToolCallChunkWithoutName { tool_call_id: id })
                }
                _ => Ok(()),
            },
            Event::ReasoningMessageContent(e)
                if !self.reasoning_open_for(&e.message_id, EventType::ReasoningMessageContent) =>
            {
                Err(AssemblyError::ReasoningContentWithoutStart {
                    message_id: e.message_id.clone(),
                })
            }
            Event::ReasoningMessageChunk(e)
                if e.message_id.is_none()
                    && self.last_chunked_reasoning().is_none()
                    && carries(&e.delta) =>
            {
                Err(AssemblyError::ReasoningChunkWithoutMessageId)
            }
            Event::ThinkingTextMessageContent(_)
                if self
                    .thinking
                    .as_ref()
                    .and_then(|t| t.text.as_ref())
                    .is_none() =>
            {
                Err(AssemblyError::ThinkingContentWithoutStart)
            }
            Event::TextMessageStart(_)
            | Event::TextMessageContent(_)
            | Event::TextMessageEnd(_)
            | Event::TextMessageChunk(_)
            | Event::ThinkingTextMessageStart(_)
            | Event::ThinkingTextMessageContent(_)
            | Event::ThinkingTextMessageEnd(_)
            | Event::ToolCallStart(_)
            | Event::ToolCallArgs(_)
            | Event::ToolCallEnd(_)
            | Event::ToolCallResult(_)
            | Event::ThinkingStart(_)
            | Event::ThinkingEnd(_)
            | Event::StateSnapshot(_)
            | Event::StateDelta(_)
            | Event::MessagesSnapshot(_)
            | Event::Raw(_)
            | Event::Custom(_)
            | Event::RunStarted(_)
            | Event::RunFinished(_)
            | Event::RunError(_)
            | Event::StepStarted(_)
            | Event::StepFinished(_)
            | Event::ActivitySnapshot(_)
            | Event::ActivityDelta(_)
            | Event::ReasoningStart(_)
            | Event::ReasoningMessageStart(_)
            | Event::ReasoningMessageContent(_)
            | Event::ReasoningMessageEnd(_)
            | Event::ReasoningMessageChunk(_)
            | Event::ReasoningEnd(_)
            | Event::ReasoningEncryptedValue(_)
            | Event::TimeTravel(_) => Ok(()),
        }
    }

    fn text_open_for(&self, id: &MessageId, incoming: EventType) -> bool {
        self.texts.iter().any(|open| {
            open.id == *id
                && (open.origin == Origin::Explicit || incoming == EventType::TextMessageChunk)
        })
    }

    fn tool_call_open_for(&self, id: &ToolCallId, incoming: EventType) -> bool {
        self.tool_calls.iter().any(|open| {
            open.id == *id
                && (open.origin == Origin::Explicit || incoming == EventType::ToolCallChunk)
        })
    }

    fn reasoning_open_for(&self, id: &MessageId, incoming: EventType) -> bool {
        self.reasoning.iter().any(|open| {
            open.id == *id
                && (open.origin == Origin::Explicit || incoming == EventType::ReasoningMessageChunk)
        })
    }

    // ── bookkeeping ────────────────────────────────────────────────────────

    fn record(&mut self, outcome: Outcome) -> Outcome {
        self.counters.events += 1;
        self.counters.updates += outcome.updates.len() as u64;
        self.counters.anomalies += outcome.anomalies.len() as u64;
        outcome
    }

    // ── text messages ──────────────────────────────────────────────────────

    fn text_index(&self, id: &MessageId) -> Option<usize> {
        self.texts.iter().position(|open| open.id == *id)
    }

    fn last_chunked_text(&self) -> Option<MessageId> {
        self.texts
            .iter()
            .rev()
            .find(|open| open.origin == Origin::Chunk)
            .map(|open| open.id.clone())
    }

    fn text_start(
        &mut self,
        id: MessageId,
        role: Role,
        name: Option<String>,
        origin: Origin,
        outcome: &mut Outcome,
    ) {
        if let Some(index) = self.text_index(&id) {
            let dropped_chars = self.texts[index].text.chars().count();
            outcome.anomalies.push(Anomaly::TextRestarted {
                message_id: id.clone(),
                dropped_chars,
            });
            self.texts.remove(index);
        }
        outcome.updates.push(Update::TextStarted {
            message_id: id.clone(),
            role: role.clone(),
            name: name.clone(),
        });
        self.texts.push(OpenText {
            id,
            role,
            name,
            text: String::new(),
            origin,
        });
    }

    fn text_content(
        &mut self,
        id: &MessageId,
        delta: &str,
        event: EventType,
        outcome: &mut Outcome,
    ) -> Result<(), AssemblyError> {
        let Some(index) = self.text_index(id) else {
            return Err(AssemblyError::TextContentWithoutStart {
                message_id: id.clone(),
            });
        };
        if delta.is_empty() {
            outcome.anomalies.push(Anomaly::EmptyDelta { event });
            return Ok(());
        }
        self.texts[index].text.push_str(delta);
        outcome.updates.push(Update::TextDelta {
            message_id: id.clone(),
            delta: delta.to_owned(),
        });
        Ok(())
    }

    fn text_end(&mut self, id: &MessageId, outcome: &mut Outcome) {
        match self.text_index(id) {
            Some(index) => {
                let open = self.texts.remove(index);
                outcome.updates.push(Update::TextFinished {
                    message_id: open.id,
                    role: open.role,
                    name: open.name,
                    text: open.text,
                });
            }
            None => outcome.anomalies.push(Anomaly::TextEndWithoutStart {
                message_id: id.clone(),
            }),
        }
    }

    // ── tool calls ─────────────────────────────────────────────────────────

    fn tool_call_index(&self, id: &ToolCallId) -> Option<usize> {
        self.tool_calls.iter().position(|open| open.id == *id)
    }

    fn last_chunked_tool_call(&self) -> Option<ToolCallId> {
        self.tool_calls
            .iter()
            .rev()
            .find(|open| open.origin == Origin::Chunk)
            .map(|open| open.id.clone())
    }

    fn tool_call_start(
        &mut self,
        id: ToolCallId,
        name: String,
        parent_message_id: Option<MessageId>,
        origin: Origin,
        outcome: &mut Outcome,
    ) {
        if let Some(index) = self.tool_call_index(&id) {
            let dropped_chars = self.tool_calls[index].arguments.chars().count();
            outcome.anomalies.push(Anomaly::ToolCallRestarted {
                tool_call_id: id.clone(),
                dropped_chars,
            });
            self.tool_calls.remove(index);
        }
        outcome.updates.push(Update::ToolCallStarted {
            tool_call_id: id.clone(),
            name: name.clone(),
            parent_message_id: parent_message_id.clone(),
        });
        self.tool_calls.push(OpenToolCall {
            id,
            name,
            parent_message_id,
            arguments: String::new(),
            origin,
        });
    }

    fn tool_call_args(
        &mut self,
        id: &ToolCallId,
        delta: &str,
        event: EventType,
        outcome: &mut Outcome,
    ) -> Result<(), AssemblyError> {
        let Some(index) = self.tool_call_index(id) else {
            return Err(AssemblyError::ToolCallArgsWithoutStart {
                tool_call_id: id.clone(),
            });
        };
        if delta.is_empty() {
            outcome.anomalies.push(Anomaly::EmptyDelta { event });
            return Ok(());
        }
        self.tool_calls[index].arguments.push_str(delta);
        outcome.updates.push(Update::ToolCallArgs {
            tool_call_id: id.clone(),
            delta: delta.to_owned(),
        });
        Ok(())
    }

    fn tool_call_end(&mut self, id: &ToolCallId, outcome: &mut Outcome) {
        match self.tool_call_index(id) {
            Some(index) => {
                let open = self.tool_calls.remove(index);
                outcome.updates.push(Update::ToolCallFinished {
                    tool_call: ToolCall::new(
                        open.id,
                        FunctionCall {
                            name: open.name,
                            arguments: open.arguments,
                        },
                    ),
                    parent_message_id: open.parent_message_id,
                });
            }
            None => outcome.anomalies.push(Anomaly::ToolCallEndWithoutStart {
                tool_call_id: id.clone(),
            }),
        }
    }

    // ── reasoning messages ─────────────────────────────────────────────────

    fn reasoning_index(&self, id: &MessageId) -> Option<usize> {
        self.reasoning.iter().position(|open| open.id == *id)
    }

    fn last_chunked_reasoning(&self) -> Option<MessageId> {
        self.reasoning
            .iter()
            .rev()
            .find(|open| open.origin == Origin::Chunk)
            .map(|open| open.id.clone())
    }

    fn reasoning_start(&mut self, id: MessageId, origin: Origin, outcome: &mut Outcome) {
        if let Some(index) = self.reasoning_index(&id) {
            let dropped_chars = self.reasoning[index].text.chars().count();
            outcome.anomalies.push(Anomaly::ReasoningRestarted {
                message_id: id.clone(),
                dropped_chars,
            });
            self.reasoning.remove(index);
        }
        outcome.updates.push(Update::ReasoningStarted {
            message_id: id.clone(),
        });
        self.reasoning.push(OpenReasoning {
            id,
            text: String::new(),
            origin,
        });
    }

    fn reasoning_content(
        &mut self,
        id: &MessageId,
        delta: &str,
        event: EventType,
        outcome: &mut Outcome,
    ) -> Result<(), AssemblyError> {
        let Some(index) = self.reasoning_index(id) else {
            return Err(AssemblyError::ReasoningContentWithoutStart {
                message_id: id.clone(),
            });
        };
        if delta.is_empty() {
            outcome.anomalies.push(Anomaly::EmptyDelta { event });
            return Ok(());
        }
        self.reasoning[index].text.push_str(delta);
        outcome.updates.push(Update::ReasoningDelta {
            message_id: id.clone(),
            delta: delta.to_owned(),
        });
        Ok(())
    }

    fn reasoning_end(&mut self, id: &MessageId, outcome: &mut Outcome) {
        match self.reasoning_index(id) {
            Some(index) => {
                let open = self.reasoning.remove(index);
                outcome.updates.push(Update::ReasoningFinished {
                    message_id: open.id,
                    text: open.text,
                });
            }
            None => outcome.anomalies.push(Anomaly::ReasoningEndWithoutStart {
                message_id: id.clone(),
            }),
        }
    }

    // ── implicit finishing ─────────────────────────────────────────────────

    /// Finish every chunk-opened item whose family is not `incoming`. The
    /// spec's chunk transform ends a chunk run at the first event that is not
    /// another chunk of the same kind.
    fn finish_chunked_except(&mut self, incoming: EventType, outcome: &mut Outcome) {
        if incoming != EventType::TextMessageChunk {
            let ids: Vec<MessageId> = self
                .texts
                .iter()
                .filter(|open| open.origin == Origin::Chunk)
                .map(|open| open.id.clone())
                .collect();
            for id in ids {
                self.text_end(&id, outcome);
            }
        }
        if incoming != EventType::ToolCallChunk {
            let ids: Vec<ToolCallId> = self
                .tool_calls
                .iter()
                .filter(|open| open.origin == Origin::Chunk)
                .map(|open| open.id.clone())
                .collect();
            for id in ids {
                self.tool_call_end(&id, outcome);
            }
        }
        if incoming != EventType::ReasoningMessageChunk {
            let ids: Vec<MessageId> = self
                .reasoning
                .iter()
                .filter(|open| open.origin == Origin::Chunk)
                .map(|open| open.id.clone())
                .collect();
            for id in ids {
                self.reasoning_end(&id, outcome);
            }
        }
    }

    /// Finish everything still streaming because `by` ended the run or
    /// replaced the transcript. Reports one [`Anomaly::ClosedEarly`] naming
    /// what was open, after the `*Finished` updates that carry their content.
    fn close_all(&mut self, by: EventType, outcome: &mut Outcome) {
        let text_message_ids: Vec<MessageId> =
            self.texts.iter().map(|open| open.id.clone()).collect();
        let tool_call_ids: Vec<ToolCallId> =
            self.tool_calls.iter().map(|open| open.id.clone()).collect();
        let reasoning_message_ids: Vec<MessageId> =
            self.reasoning.iter().map(|open| open.id.clone()).collect();
        let thinking = self.thinking.is_some();
        if text_message_ids.is_empty()
            && tool_call_ids.is_empty()
            && reasoning_message_ids.is_empty()
            && !thinking
        {
            self.reasoning_brackets.clear();
            self.steps.clear();
            return;
        }
        for id in &text_message_ids {
            self.text_end(id, outcome);
        }
        for id in &tool_call_ids {
            self.tool_call_end(id, outcome);
        }
        for id in &reasoning_message_ids {
            self.reasoning_end(id, outcome);
        }
        if let Some(open) = self.thinking.take() {
            outcome.updates.push(Update::ThinkingFinished {
                title: open.title,
                text: open.text.unwrap_or_default(),
            });
        }
        self.reasoning_brackets.clear();
        self.steps.clear();
        outcome.anomalies.push(Anomaly::ClosedEarly {
            by,
            text_message_ids,
            tool_call_ids,
            reasoning_message_ids,
            thinking,
        });
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;
    use crate::event::{
        BaseEvent, TextMessageChunkEvent, TextMessageContentEvent, TextMessageEndEvent,
        TextMessageStartEvent,
    };
    use crate::JsonValue;

    type Ev = Event<JsonValue>;

    fn start(id: &MessageId) -> Ev {
        Ev::TextMessageStart(TextMessageStartEvent::new(id.clone()))
    }

    fn content(id: &MessageId, delta: &str) -> Ev {
        Ev::TextMessageContent(TextMessageContentEvent {
            base: BaseEvent::default(),
            message_id: id.clone(),
            delta: delta.to_owned(),
        })
    }

    fn end(id: &MessageId) -> Ev {
        Ev::TextMessageEnd(TextMessageEndEvent {
            base: BaseEvent::default(),
            message_id: id.clone(),
        })
    }

    #[test]
    fn a_refused_event_leaves_state_and_chunk_runs_untouched() {
        // A chunk run is open; a content delta for an unknown id arrives. The
        // refusal must not have silently finished the chunk run on the way.
        let mut assembler = Assembler::new();
        let chunk_id = MessageId::random();
        assembler
            .push(&Ev::TextMessageChunk(TextMessageChunkEvent {
                base: BaseEvent::default(),
                message_id: Some(chunk_id.clone()),
                role: None,
                delta: Some("partial".into()),
                name: None,
            }))
            .unwrap();
        let orphan = MessageId::random();
        let error = assembler.push(&content(&orphan, "lost?")).unwrap_err();
        assert_eq!(
            error,
            AssemblyError::TextContentWithoutStart { message_id: orphan }
        );
        assert_eq!(assembler.text(&chunk_id), Some("partial"));
        assert_eq!(assembler.counters().errors, 1);
        assert_eq!(assembler.counters().events, 1);
    }

    #[test]
    fn reset_keeps_counters_and_forgets_open_items() {
        let mut assembler = Assembler::new();
        let id = MessageId::random();
        assembler.push(&start(&id)).unwrap();
        assembler.push(&content(&id, "x")).unwrap();
        assembler.reset();
        assert_eq!(assembler.text(&id), None);
        assert_eq!(assembler.counters().events, 2);
        let outcome = assembler.push(&end(&id)).unwrap();
        assert_eq!(
            outcome.anomalies,
            vec![Anomaly::TextEndWithoutStart { message_id: id }]
        );
    }
}
