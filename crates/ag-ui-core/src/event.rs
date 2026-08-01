use convergence_attrs::substrate;

use crate::state::AgentState;
use crate::types::{Interrupt, MessageId, RunAgentInput, RunId, ThreadId, ToolCallId};
use crate::types::{Message, Role};
use crate::JsonValue;
use serde::{Deserialize, Serialize};

/// Event type discriminant for the AG-UI protocol; consumed by all event routing and serialization.
#[substrate(name = "EventType", since = "0.1.0", domain = "ui")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventType {
    /// Event indicating the start of a text message
    TextMessageStart,
    /// Event containing a piece of text message content
    TextMessageContent,
    /// Event indicating the end of a text message
    TextMessageEnd,
    /// Event containing a chunk of text message content
    TextMessageChunk,
    /// Event indicating the start of a thinking text message
    ThinkingTextMessageStart,
    /// Event indicating a piece of a thinking text message
    ThinkingTextMessageContent,
    /// Event indicating the end of a thinking text message
    ThinkingTextMessageEnd,
    /// Event indicating the start of a tool call
    ToolCallStart,
    /// Event containing tool call arguments
    ToolCallArgs,
    /// Event indicating the end of a tool call
    ToolCallEnd,
    /// Event containing a chunk of tool call content
    ToolCallChunk,
    /// Event containing the result of a tool call
    ToolCallResult,
    /// Event indicating the start of a thinking step event
    ThinkingStart,
    /// Event indicating the end of a thinking step event
    ThinkingEnd,
    /// Event containing a snapshot of the state
    StateSnapshot,
    /// Event containing a delta of the state
    StateDelta,
    /// Event containing a snapshot of the messages
    MessagesSnapshot,
    /// Event containing a raw event
    Raw,
    /// Event containing a custom event
    Custom,
    /// Event indicating that a run has started
    RunStarted,
    /// Event indicating that a run has finished
    RunFinished,
    /// Event indicating that a run has encountered an error
    RunError,
    /// Event indicating that a step has started
    StepStarted,
    /// Event indicating that a step has finished
    StepFinished,

    // ==================== Activity Events (spec catch-up 2026-07) ====================
    /// Event containing a full snapshot of a message's activity payload
    ActivitySnapshot,
    /// Event containing a JSON Patch delta against a message's activity payload
    ActivityDelta,

    // ==================== Reasoning Events (spec catch-up 2026-07) ====================
    // The TS SDK deprecates THINKING_START / THINKING_END / THINKING_TEXT_MESSAGE_*
    // in favor of these (removal planned for 1.0.0). Both families are kept side
    // by side here — see the crate-level catch-up notes for the deprecation map.
    /// Event indicating the start of a reasoning message
    ReasoningStart,
    /// Event indicating the start of a reasoning message's text content
    ReasoningMessageStart,
    /// Event containing a piece of reasoning message content
    ReasoningMessageContent,
    /// Event indicating the end of a reasoning message's text content
    ReasoningMessageEnd,
    /// Event containing a complete or partial reasoning message chunk
    ReasoningMessageChunk,
    /// Event indicating the end of a reasoning message
    ReasoningEnd,
    /// Event containing an encrypted reasoning value (opaque provider payload)
    ReasoningEncryptedValue,

    // ==================== Time Travel Events ====================
    /// Emitted at each phase boundary when a checkpoint snapshot is captured
    CheckpointCreated,
    /// Emitted when a user initiates a rewind to a prior checkpoint
    RewindInitiated,
    /// Emitted when replay execution begins from a rewound checkpoint
    ReplayStarted,
    /// Emitted after each agent/task completes during replay re-execution
    ReplayProgress,
    /// Full timeline snapshot sent on connect or reconnect for UI rendering
    TimelineSnapshot,
}

/// Base event for all events in the Agent User Interaction Protocol.
/// Contains common fields that are present in all event types.
///
/// # Migration Notice
///
/// When the `official-types` feature is enabled, consider migrating to the official
/// AG-UI SDK types via the compatibility layer in `crate::compat`. Helper types will
/// be maintained for 2 releases (through v0.4.0) to allow gradual migration.
///
/// ```rust,ignore
/// // Migration example (when official-types feature is enabled):
/// use ag_ui_core::event::BaseEvent;
/// use ag_ui_core::compat; // Conversion traits
///
/// let helper_event = BaseEvent { timestamp: Some(1.0), raw_event: None };
/// // With official-types feature: let official_event = helper_event.into();
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BaseEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
    #[serde(rename = "rawEvent", skip_serializing_if = "Option::is_none")]
    pub raw_event: Option<JsonValue>,
}

/// Event indicating the start of a text message.
/// This event is sent when the agent begins generating a text message.
///
/// # Spec catch-up (2026-07, non-breaking)
///
/// - `name` is a new optional field (assistant/participant display name).
/// - `role` now falls back to `Role::Assistant` when the field is omitted on
///   the wire (matches the TS SDK's `TextMessageRoleSchema.default("assistant")`).
///   Serialization is unaffected — `role` is still always emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMessageStartEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    #[serde(default = "Role::assistant")]
    pub role: Role, // "assistant"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Event containing a piece of text message content.
/// This event is sent for each chunk of content as the agent generates a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMessageContentEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    pub delta: String,
}

/// Event indicating the end of a text message.
/// This event is sent when the agent completes a text message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMessageEndEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
}

/// Event containing a chunk of text message content.
/// This event combines start, content, and potentially end information in a single event,
/// with optional fields that may or may not be present.
///
/// # Spec catch-up (2026-07, BREAKING)
///
/// - `role` changed from required `Role` to `Option<Role>` — the TS SDK's
///   `TextMessageChunkEventSchema.role` is `TextMessageRoleSchema.optional()`.
///   Any code that constructs this struct with a bare `Role` value, or reads
///   `.role` expecting `Role` rather than `Option<Role>`, must be updated.
///   See the crate-level catch-up notes for the downstream (Brevity) impact.
/// - `name` is a new optional field (non-breaking addition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMessageChunkEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Event indicating the start of a thinking text message.
/// This event is sent when the agent begins generating internal thinking content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingTextMessageStartEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
}

/// Event indicating a piece of a thinking text message.
/// This event contains chunks of the agent's internal thinking process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingTextMessageContentEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub delta: String,
}

/// Event indicating the end of a thinking text message.
/// This event is sent when the agent completes its internal thinking process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingTextMessageEndEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
}

/// Event indicating the start of a tool call.
/// This event is sent when the agent begins to call a tool with specific parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallStartEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: ToolCallId,
    #[serde(rename = "toolCallName")]
    pub tool_call_name: String,
    #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<MessageId>,
}

/// Event containing tool call arguments.
/// This event contains chunks of the arguments being passed to a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallArgsEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: ToolCallId,
    pub delta: String,
}

/// Event indicating the end of a tool call.
/// This event is sent when the agent completes sending arguments to a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallEndEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: ToolCallId,
}

/// Event containing the result of a tool call.
/// This event is sent when a tool has completed execution and returns its result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResultEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: ToolCallId,
    pub content: String,
    #[serde(default = "Role::tool")]
    pub role: Role, // "tool"
}

/// Event containing a chunk of tool call content.
/// This event combines start, args, and potentially end information in a single event,
/// with optional fields that may or may not be present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallChunkEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "toolCallId", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    #[serde(rename = "toolCallName", skip_serializing_if = "Option::is_none")]
    pub tool_call_name: Option<String>,
    #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
    pub parent_message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

/// Event indicating the start of a thinking step event.
/// This event is sent when the agent begins a deliberate thinking phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingStartEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Event indicating the end of a thinking step event.
/// This event is sent when the agent completes a thinking phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingEndEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
}

/// Event containing a snapshot of the state.
/// This event provides a complete representation of the current agent state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(deserialize = ""))]
pub struct StateSnapshotEvent<StateT: AgentState = JsonValue> {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub snapshot: StateT,
}

/// Event containing a delta of the state.
/// This event contains JSON Patch operations (RFC 6902) that describe changes to the agent state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDeltaEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub delta: Vec<JsonValue>,
}

/// Event containing a snapshot of the messages.
/// This event provides a complete list of all current conversation messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesSnapshotEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub messages: Vec<Message>,
}

/// Event containing a raw event.
/// This event type allows wrapping arbitrary events from external sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub event: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Event containing a custom event.
/// This event type allows for application-specific custom events with arbitrary data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub name: String,
    pub value: JsonValue,
}

/// Event indicating that a run has started; the lifecycle open-bracket consumed by all UI subscribers.
///
/// # Spec catch-up (2026-07, non-breaking additions)
///
/// - `parent_run_id` — the parent run this run was spawned from (e.g. a
///   sub-agent invocation), new optional field.
/// - `input` — the full `RunAgentInput` the run was started with, new
///   optional field. Uses the existing `RunAgentInput<JsonValue, JsonValue>`
///   as-is; `RunAgentInput` itself has its own separate spec drift
///   (`parentRunId`, `resume`) not addressed in this pass — see the
///   crate-level catch-up notes.
#[substrate(name = "RunStartedEvent", since = "0.1.0", domain = "ui")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStartedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "threadId")]
    pub thread_id: ThreadId,
    #[serde(rename = "runId")]
    pub run_id: RunId,
    #[serde(rename = "parentRunId", skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<RunAgentInput>,
}

/// Event indicating that a run has finished; the lifecycle close-bracket consumed by all UI subscribers.
///
/// # Spec catch-up (2026-07, non-breaking addition)
///
/// - `outcome` — new optional field distinguishing a normal completion from
///   a run paused on one or more human-in-the-loop [`Interrupt`]s. See
///   [`RunFinishedOutcome`].
#[substrate(name = "RunFinishedEvent", since = "0.1.0", domain = "ui")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFinishedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "threadId")]
    pub thread_id: ThreadId,
    #[serde(rename = "runId")]
    pub run_id: RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunFinishedOutcome>,
}

/// The outcome of a finished run: either a normal completion, or a pause on
/// one or more human-in-the-loop interrupts awaiting resolution.
///
/// New in the AG-UI spec (spec catch-up, 2026-07). Mirrors the TS SDK's
/// `RunFinishedOutcomeSchema` discriminated union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RunFinishedOutcome {
    /// The run completed normally.
    Success,
    /// The run paused, awaiting resolution of one or more interrupts.
    Interrupt {
        /// Must contain at least one interrupt — see [`RunFinishedOutcome::validate`].
        interrupts: Vec<Interrupt>,
    },
}

impl RunFinishedOutcome {
    /// Validate spec invariants not expressible in the type alone: an
    /// `Interrupt` outcome must carry at least one interrupt.
    pub fn validate(&self) -> Result<(), EventValidationError> {
        match self {
            RunFinishedOutcome::Success => Ok(()),
            RunFinishedOutcome::Interrupt { interrupts } if interrupts.is_empty() => {
                Err(EventValidationError::EmptyInterrupts)
            }
            RunFinishedOutcome::Interrupt { .. } => Ok(()),
        }
    }
}

/// Event indicating that a run has encountered an error.
/// This event is sent when an agent run fails with an error message and optional error code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunErrorEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Event indicating that a step has started.
/// This event is sent when a specific named step within a run begins execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepStartedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "stepName")]
    pub step_name: String,
}

/// Event indicating that a step has finished.
/// This event is sent when a specific named step within a run completes execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepFinishedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "stepName")]
    pub step_name: String,
}

// ==================== Activity Event Structs (spec catch-up 2026-07) ====================

/// Event containing a full snapshot of a message's activity payload.
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ActivitySnapshotEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivitySnapshotEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    #[serde(rename = "activityType")]
    pub activity_type: String,
    pub content: JsonValue,
    #[serde(default = "default_activity_replace")]
    pub replace: bool,
}

fn default_activity_replace() -> bool {
    true
}

/// Event containing a JSON Patch (RFC 6902) delta against a message's activity payload.
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ActivityDeltaEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityDeltaEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    #[serde(rename = "activityType")]
    pub activity_type: String,
    pub patch: Vec<JsonValue>,
}

// ==================== Reasoning Event Structs (spec catch-up 2026-07) ====================
//
// These are the AG-UI spec's replacement for THINKING_START / THINKING_END /
// THINKING_TEXT_MESSAGE_* (deprecated upstream, removal planned for 1.0.0).
// Both families are implemented side by side: the deprecated `Thinking*`
// structs above are untouched (still emitted/consumed by existing callers),
// and these `Reasoning*` structs are additive.

/// Event indicating the start of a reasoning turn (wraps one or more
/// reasoning message segments and/or encrypted reasoning values).
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningStartEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningStartEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
}

/// Event indicating the start of a reasoning message's visible text content.
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningMessageStartEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningMessageStartEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    #[serde(default = "Role::reasoning")]
    pub role: Role, // Always Role::Reasoning
}

/// Event containing a piece of reasoning message content.
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningMessageContentEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningMessageContentEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
    pub delta: String,
}

/// Event indicating the end of a reasoning message's visible text content.
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningMessageEndEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningMessageEndEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
}

/// Event containing a complete or partial reasoning message chunk, combining
/// start/content/end information in a single event (mirrors `TextMessageChunkEvent`).
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningMessageChunkEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningMessageChunkEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

/// Event indicating the end of a reasoning turn.
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningEndEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningEndEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    #[serde(rename = "messageId")]
    pub message_id: MessageId,
}

/// The kind of entity an encrypted reasoning value is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEncryptedValueSubtype {
    ToolCall,
    Message,
}

/// Event containing an opaque, provider-encrypted reasoning value (e.g. an
/// encrypted chain-of-thought signature that must be round-tripped back to
/// the provider but not rendered).
///
/// New in the AG-UI spec since this crate was last synced (2026-07 catch-up).
/// Mirrors the TS SDK's `ReasoningEncryptedValueEventSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningEncryptedValueEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    pub subtype: ReasoningEncryptedValueSubtype,
    #[serde(rename = "entityId")]
    pub entity_id: String,
    #[serde(rename = "encryptedValue")]
    pub encrypted_value: String,
}

// ==================== Time Travel Event Structs ====================

/// A checkpoint was created at a phase boundary.
///
/// Emitted each time the orchestrator captures a full state snapshot.
/// The UI adds a new node to the timeline on receipt of this event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointCreatedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    /// Unique checkpoint identifier (UUID string)
    #[serde(rename = "checkpointId")]
    pub checkpoint_id: String,
    /// Phase number at which the checkpoint was taken
    pub phase: usize,
    /// JJ operation ID anchoring this checkpoint (from jj-dev oplog)
    #[serde(rename = "opId")]
    pub op_id: String,
    /// Human-readable description of this phase boundary
    pub description: String,
    /// ISO 8601 timestamp of checkpoint creation
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

/// A user requested a rewind to a prior checkpoint.
///
/// Emitted when the rewind request is validated and accepted by the orchestrator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewindInitiatedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    /// The checkpoint being rewound to
    #[serde(rename = "checkpointId")]
    pub checkpoint_id: String,
    /// Phase being rewound to
    #[serde(rename = "targetPhase")]
    pub target_phase: usize,
    /// JJ operation ID of the target checkpoint
    #[serde(rename = "targetOpId")]
    pub target_op_id: String,
    /// Scope: `"full"`, `"phase:<N>"`, or `"agent:<name>"`.
    pub scope: String,
    /// Optional description of the modification to apply before replay
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modification: Option<String>,
}

/// A checkpoint summary for use within the timeline snapshot event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineCheckpoint {
    /// Unique checkpoint identifier
    #[serde(rename = "checkpointId")]
    pub checkpoint_id: String,
    /// Phase number
    pub phase: usize,
    /// JJ operation ID
    #[serde(rename = "opId")]
    pub op_id: String,
    /// Human-readable description
    pub description: String,
    /// ISO 8601 timestamp
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// Number of bookmarks captured at this checkpoint
    #[serde(rename = "bookmarkCount")]
    pub bookmark_count: usize,
}

/// Replay execution has begun from the rewound checkpoint.
///
/// Emitted after the JJ state restore completes and agent re-execution starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayStartedEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    /// The checkpoint being replayed from
    #[serde(rename = "checkpointId")]
    pub checkpoint_id: String,
    /// Phases that will be re-executed
    #[serde(rename = "affectedPhases")]
    pub affected_phases: Vec<usize>,
    /// Agent names whose work will be re-run
    #[serde(rename = "affectedAgents")]
    pub affected_agents: Vec<String>,
    /// Agent names whose work is preserved
    #[serde(rename = "preservedAgents")]
    pub preserved_agents: Vec<String>,
    /// Description of the modification being applied
    pub modification: String,
    /// Estimated re-execution cost in USD (None if unavailable)
    #[serde(rename = "estimatedCostUsd", skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

/// Per-phase progress update during replay re-execution.
///
/// Emitted after each agent/task completes within a replaying phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayProgressEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    /// Phase currently executing
    pub phase: usize,
    /// Number of agents/tasks completed so far in this phase
    pub completed: usize,
    /// Total agents/tasks in this phase
    pub total: usize,
    /// Name of the agent that just completed (optional)
    #[serde(rename = "agentName", skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

/// Full timeline snapshot for initial UI render or reconnect.
///
/// Sent when a client first connects to the SSE stream so the frontend can
/// render all existing checkpoints without replaying event history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineSnapshotEvent {
    #[serde(flatten)]
    pub base: BaseEvent,
    /// All checkpoints in this orchestration run, ordered by phase
    pub checkpoints: Vec<TimelineCheckpoint>,
    /// Current phase number
    #[serde(rename = "currentPhase")]
    pub current_phase: usize,
    /// Whether a rewind/replay is currently in progress
    #[serde(rename = "replayInProgress")]
    pub replay_in_progress: bool,
    /// ID of the checkpoint being replayed (if replay_in_progress is true)
    #[serde(
        rename = "activeReplayCheckpointId",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_replay_checkpoint_id: Option<String>,
}

// ==================== TimeTravelEvent Inner Enum ====================

/// Brevity-specific time-travel events.
///
/// These are an internal extension to the AG-UI protocol grouped under
/// `Event::TimeTravel(TimeTravelEvent)` so that future TimeTravel work touches
/// one enum rather than two. They are still serializable on the wire — the
/// `From<Event> for OfficialEvent` impl wraps each variant as a `Custom` event
/// (see `compat::events`) so AG-UI SDK consumers receive them transparently.
///
/// All time-travel events remain fully constructable, matchable, and serializable —
/// they simply live one level deeper under `Event::TimeTravel`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TimeTravelEvent {
    /// Emitted at each orchestration phase boundary when a checkpoint is captured.
    CheckpointCreated(CheckpointCreatedEvent),
    /// Emitted when a user's rewind request has been validated and accepted.
    RewindInitiated(RewindInitiatedEvent),
    /// Emitted when replay execution starts from the rewound checkpoint.
    ReplayStarted(ReplayStartedEvent),
    /// Emitted after each agent/task completes during replay re-execution.
    ReplayProgress(ReplayProgressEvent),
    /// Full timeline snapshot sent on initial connection or reconnect.
    TimelineSnapshot(TimelineSnapshotEvent),
}

impl TimeTravelEvent {
    /// Get the `EventType` discriminant for this time travel event.
    pub fn event_type(&self) -> EventType {
        match self {
            TimeTravelEvent::CheckpointCreated(_) => EventType::CheckpointCreated,
            TimeTravelEvent::RewindInitiated(_) => EventType::RewindInitiated,
            TimeTravelEvent::ReplayStarted(_) => EventType::ReplayStarted,
            TimeTravelEvent::ReplayProgress(_) => EventType::ReplayProgress,
            TimeTravelEvent::TimelineSnapshot(_) => EventType::TimelineSnapshot,
        }
    }

    /// Get the base-event timestamp, if set.
    pub fn timestamp(&self) -> Option<f64> {
        match self {
            TimeTravelEvent::CheckpointCreated(e) => e.base.timestamp,
            TimeTravelEvent::RewindInitiated(e) => e.base.timestamp,
            TimeTravelEvent::ReplayStarted(e) => e.base.timestamp,
            TimeTravelEvent::ReplayProgress(e) => e.base.timestamp,
            TimeTravelEvent::TimelineSnapshot(e) => e.base.timestamp,
        }
    }
}

/// Union of all possible events in the Agent User Interaction Protocol; the top-level UI wire type.
///
/// This enum represents the full set of events that can be exchanged
/// between the agent and the client.
///
/// # Migration Notice
///
/// When the `official-types` feature is enabled, consider migrating to the official
/// AG-UI SDK `Event` type via the compatibility layer. Helper types will be maintained
/// for 2 releases (through v0.4.0) to allow gradual migration.
///
/// See `crate::compat::events` for bidirectional conversion support.
#[substrate(name = "Event", since = "0.1.0", domain = "ui")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "SCREAMING_SNAKE_CASE",
    bound(deserialize = "")
)]
pub enum Event<StateT: AgentState = JsonValue> {
    /// Signals the start of a text message from an agent.
    /// Contains the message ID and role information.
    TextMessageStart(TextMessageStartEvent),

    /// Represents a chunk of content being added to an in-progress text message.
    /// Contains the message ID and the text delta to append.
    TextMessageContent(TextMessageContentEvent),

    /// Signals the completion of a text message.
    /// Contains the message ID of the completed message.
    TextMessageEnd(TextMessageEndEvent),

    /// Represents a complete or partial message chunk in a single event.
    /// May contain optional message ID, role, and delta information.
    TextMessageChunk(TextMessageChunkEvent),

    /// Signals the start of a thinking text message.
    /// Used for internal agent thought processes that should be displayed to the user.
    ThinkingTextMessageStart(ThinkingTextMessageStartEvent),

    /// Represents content being added to an in-progress thinking text message.
    /// Contains the delta text to append.
    ThinkingTextMessageContent(ThinkingTextMessageContentEvent),

    /// Signals the completion of a thinking text message.
    ThinkingTextMessageEnd(ThinkingTextMessageEndEvent),

    /// Signals the start of a tool call by the agent.
    /// Contains the tool call ID, name, and optional parent message ID.
    ToolCallStart(ToolCallStartEvent),

    /// Represents arguments being added to an in-progress tool call.
    /// Contains the tool call ID and argument data delta.
    ToolCallArgs(ToolCallArgsEvent),

    /// Signals the completion of a tool call.
    /// Contains the tool call ID of the completed call.
    ToolCallEnd(ToolCallEndEvent),

    /// Represents a complete or partial tool call in a single event.
    /// May contain optional tool call ID, name, parent message ID, and delta.
    ToolCallChunk(ToolCallChunkEvent),

    /// Represents the result of a completed tool call.
    /// Contains the message ID, tool call ID, content, and optional role.
    ToolCallResult(ToolCallResultEvent),

    /// Signals the start of a thinking process.
    /// Contains an optional title describing the thinking process.
    ThinkingStart(ThinkingStartEvent),

    /// Signals the end of a thinking process.
    ThinkingEnd(ThinkingEndEvent),

    /// Provides a complete snapshot of the current state.
    /// Contains the full state as a JSON value.
    StateSnapshot(StateSnapshotEvent<StateT>),

    /// Provides incremental changes to the state.
    /// Contains a vector of delta operations to apply to the state.
    StateDelta(StateDeltaEvent),

    /// Provides a complete snapshot of all messages.
    /// Contains a vector of all current messages.
    MessagesSnapshot(MessagesSnapshotEvent),

    /// Wraps a raw event from an external source.
    /// Contains the original event as a JSON value and an optional source identifier.
    Raw(RawEvent),

    /// Represents a custom event type not covered by the standard events.
    /// Contains a name identifying the custom event type and an associated value.
    Custom(CustomEvent),

    /// Signals the start of an agent run.
    /// Contains thread ID and run ID to identify the run.
    RunStarted(RunStartedEvent),

    /// Signals the completion of an agent run.
    /// Contains thread ID, run ID, and optional result data.
    RunFinished(RunFinishedEvent),

    /// Signals an error that occurred during an agent run.
    /// Contains error message and optional error code.
    RunError(RunErrorEvent),

    /// Signals the start of a step within an agent run.
    /// Contains the name of the step being started.
    StepStarted(StepStartedEvent),

    /// Signals the completion of a step within an agent run.
    /// Contains the name of the completed step.
    StepFinished(StepFinishedEvent),

    // ==================== Activity Events (spec catch-up 2026-07) ====================
    /// A full snapshot of a message's activity payload.
    ActivitySnapshot(ActivitySnapshotEvent),
    /// A JSON Patch delta against a message's activity payload.
    ActivityDelta(ActivityDeltaEvent),

    // ==================== Reasoning Events (spec catch-up 2026-07) ====================
    /// Signals the start of a reasoning turn.
    ReasoningStart(ReasoningStartEvent),
    /// Signals the start of a reasoning message's visible text content.
    ReasoningMessageStart(ReasoningMessageStartEvent),
    /// A piece of reasoning message content.
    ReasoningMessageContent(ReasoningMessageContentEvent),
    /// Signals the end of a reasoning message's visible text content.
    ReasoningMessageEnd(ReasoningMessageEndEvent),
    /// A complete or partial reasoning message chunk.
    ReasoningMessageChunk(ReasoningMessageChunkEvent),
    /// Signals the end of a reasoning turn.
    ReasoningEnd(ReasoningEndEvent),
    /// An opaque, provider-encrypted reasoning value.
    ReasoningEncryptedValue(ReasoningEncryptedValueEvent),

    // ==================== Time Travel Events ====================
    /// Wrapper for Brevity-specific time-travel events.
    ///
    /// Grouped here for blast-radius isolation: future TimeTravel changes touch
    /// the [`TimeTravelEvent`] enum, not [`Event`]. On the wire, each inner
    /// variant is converted to an `OfficialEvent::Custom` event (see
    /// `compat::events`) so AG-UI SDK consumers can consume them transparently.
    TimeTravel(TimeTravelEvent),
}

impl<StateT: AgentState> Event<StateT> {
    /// Get the event type
    pub fn event_type(&self) -> EventType {
        match self {
            Event::TextMessageStart(_) => EventType::TextMessageStart,
            Event::TextMessageContent(_) => EventType::TextMessageContent,
            Event::TextMessageEnd(_) => EventType::TextMessageEnd,
            Event::TextMessageChunk(_) => EventType::TextMessageChunk,
            Event::ThinkingTextMessageStart(_) => EventType::ThinkingTextMessageStart,
            Event::ThinkingTextMessageContent(_) => EventType::ThinkingTextMessageContent,
            Event::ThinkingTextMessageEnd(_) => EventType::ThinkingTextMessageEnd,
            Event::ToolCallStart(_) => EventType::ToolCallStart,
            Event::ToolCallArgs(_) => EventType::ToolCallArgs,
            Event::ToolCallEnd(_) => EventType::ToolCallEnd,
            Event::ToolCallChunk(_) => EventType::ToolCallChunk,
            Event::ToolCallResult(_) => EventType::ToolCallResult,
            Event::ThinkingStart(_) => EventType::ThinkingStart,
            Event::ThinkingEnd(_) => EventType::ThinkingEnd,
            Event::StateSnapshot(_) => EventType::StateSnapshot,
            Event::StateDelta(_) => EventType::StateDelta,
            Event::MessagesSnapshot(_) => EventType::MessagesSnapshot,
            Event::Raw(_) => EventType::Raw,
            Event::Custom(_) => EventType::Custom,
            Event::RunStarted(_) => EventType::RunStarted,
            Event::RunFinished(_) => EventType::RunFinished,
            Event::RunError(_) => EventType::RunError,
            Event::StepStarted(_) => EventType::StepStarted,
            Event::StepFinished(_) => EventType::StepFinished,
            Event::ActivitySnapshot(_) => EventType::ActivitySnapshot,
            Event::ActivityDelta(_) => EventType::ActivityDelta,
            Event::ReasoningStart(_) => EventType::ReasoningStart,
            Event::ReasoningMessageStart(_) => EventType::ReasoningMessageStart,
            Event::ReasoningMessageContent(_) => EventType::ReasoningMessageContent,
            Event::ReasoningMessageEnd(_) => EventType::ReasoningMessageEnd,
            Event::ReasoningMessageChunk(_) => EventType::ReasoningMessageChunk,
            Event::ReasoningEnd(_) => EventType::ReasoningEnd,
            Event::ReasoningEncryptedValue(_) => EventType::ReasoningEncryptedValue,
            Event::TimeTravel(inner) => inner.event_type(),
        }
    }

    /// Get the timestamp if available
    pub fn timestamp(&self) -> Option<f64> {
        match self {
            Event::TextMessageStart(e) => e.base.timestamp,
            Event::TextMessageContent(e) => e.base.timestamp,
            Event::TextMessageEnd(e) => e.base.timestamp,
            Event::TextMessageChunk(e) => e.base.timestamp,
            Event::ThinkingTextMessageStart(e) => e.base.timestamp,
            Event::ThinkingTextMessageContent(e) => e.base.timestamp,
            Event::ThinkingTextMessageEnd(e) => e.base.timestamp,
            Event::ToolCallStart(e) => e.base.timestamp,
            Event::ToolCallArgs(e) => e.base.timestamp,
            Event::ToolCallEnd(e) => e.base.timestamp,
            Event::ToolCallChunk(e) => e.base.timestamp,
            Event::ToolCallResult(e) => e.base.timestamp,
            Event::ThinkingStart(e) => e.base.timestamp,
            Event::ThinkingEnd(e) => e.base.timestamp,
            Event::StateSnapshot(e) => e.base.timestamp,
            Event::StateDelta(e) => e.base.timestamp,
            Event::MessagesSnapshot(e) => e.base.timestamp,
            Event::Raw(e) => e.base.timestamp,
            Event::Custom(e) => e.base.timestamp,
            Event::RunStarted(e) => e.base.timestamp,
            Event::RunFinished(e) => e.base.timestamp,
            Event::RunError(e) => e.base.timestamp,
            Event::StepStarted(e) => e.base.timestamp,
            Event::StepFinished(e) => e.base.timestamp,
            Event::ActivitySnapshot(e) => e.base.timestamp,
            Event::ActivityDelta(e) => e.base.timestamp,
            Event::ReasoningStart(e) => e.base.timestamp,
            Event::ReasoningMessageStart(e) => e.base.timestamp,
            Event::ReasoningMessageContent(e) => e.base.timestamp,
            Event::ReasoningMessageEnd(e) => e.base.timestamp,
            Event::ReasoningMessageChunk(e) => e.base.timestamp,
            Event::ReasoningEnd(e) => e.base.timestamp,
            Event::ReasoningEncryptedValue(e) => e.base.timestamp,
            Event::TimeTravel(inner) => inner.timestamp(),
        }
    }
}

/// Validation error types for events in the Agent User Interaction Protocol.
/// These errors represent validation failures when creating or processing events.
#[derive(Debug, thiserror::Error)]
pub enum EventValidationError {
    #[error("Delta must not be an empty string")]
    EmptyDelta,
    #[error("Invalid event format: {0}")]
    InvalidFormat(String),
    #[error("Interrupt outcome must contain at least one interrupt")]
    EmptyInterrupts,
}

/// Validate text message content event
impl TextMessageContentEvent {
    pub fn validate(&self) -> Result<(), EventValidationError> {
        if self.delta.is_empty() {
            return Err(EventValidationError::EmptyDelta);
        }
        Ok(())
    }
}

/// Builder pattern for creating events
impl TextMessageStartEvent {
    pub fn new(message_id: impl Into<MessageId>) -> Self {
        Self {
            base: BaseEvent {
                timestamp: None,
                raw_event: None,
            },
            message_id: message_id.into(),
            role: Role::Assistant,
            name: None,
        }
    }

    pub fn with_timestamp(mut self, timestamp: f64) -> Self {
        self.base.timestamp = Some(timestamp);
        self
    }

    pub fn with_raw_event(mut self, raw_event: JsonValue) -> Self {
        self.base.raw_event = Some(raw_event);
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

impl TextMessageContentEvent {
    pub fn new(
        message_id: impl Into<MessageId>,
        delta: String,
    ) -> Result<Self, EventValidationError> {
        let event = Self {
            base: BaseEvent {
                timestamp: None,
                raw_event: None,
            },
            message_id: message_id.into(),
            delta,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn with_timestamp(mut self, timestamp: f64) -> Self {
        self.base.timestamp = Some(timestamp);
        self
    }
}
