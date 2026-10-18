use crate::JsonValue;
use crate::types::ids::ToolCallId;
use serde::{Deserialize, Serialize};

/// A human-in-the-loop interrupt raised during a run.
///
/// New in the AG-UI spec's `RunFinishedOutcome` (spec catch-up, 2026-07):
/// a `RUN_FINISHED` event can now carry `outcome: { type: "interrupt",
/// interrupts: Interrupt[] }` instead of completing normally, letting a
/// server pause a run and ask the client to resolve one or more interrupts
/// before it can continue.
///
/// Mirrors the TypeScript SDK's `InterruptSchema` (`sdks/typescript/packages/core/src/types.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interrupt {
    /// Unique identifier for this interrupt.
    pub id: String,
    /// Machine-readable reason the run was interrupted.
    pub reason: String,
    /// Human-readable message describing the interrupt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The tool call this interrupt is associated with, if any.
    #[serde(rename = "toolCallId", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    /// JSON Schema describing the shape of the expected resolution payload.
    #[serde(rename = "responseSchema", skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<JsonValue>,
    /// ISO 8601 timestamp after which this interrupt is no longer resolvable.
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Arbitrary application-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

impl Interrupt {
    pub fn new(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            reason: reason.into(),
            message: None,
            tool_call_id: None,
            response_schema: None,
            expires_at: None,
            metadata: None,
        }
    }
}
