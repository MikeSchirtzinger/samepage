use serde::{Deserialize, Serialize};
/// Evidence is separate from the spoken or written cue so an explanation
/// cannot satisfy its contract with fluent prose over an unrelated picture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationEvidence {
    pub target_id: String,
    pub detail: String,
}

/// One visual operation bound to a stable Atlas target.
///
/// `cue_span` is a character range in the beat cue. The event-clock renderer
/// executes actions when the beat begins. A later browser speech clock may use
/// this range to align the same action with spoken progress without changing
/// the flow state or the visual vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationVisualAction {
    pub kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_span: Option<[u32; 2]>,
}

/// One named way out of a beat.
///
/// Phrases and target ids are resolver hints, not a second hidden answer
/// checker. The agent combines the learner's utterance with live selection,
/// chooses one transition, and the shared core validates the exact id and
/// optimistic revision before advancing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationTransition {
    pub id: String,
    pub label: String,
    /// Render this transition as a learner-facing control whose click is the
    /// answer. Agent-resolved grading branches omit it and continue to use a
    /// free response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default)]
    pub target_ids: Vec<String>,
    #[serde(default)]
    pub phrases: Vec<String>,
}

/// How the human or agent may advance one beat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationAdvance {
    pub mode: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub transitions: Vec<ExplanationTransition>,
}

/// One finite explanatory unit: why it exists, what is said, what must be
/// observable, what the page does, and which transitions are valid next.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationBeat {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub cue: String,
    #[serde(default)]
    pub evidence: Vec<ExplanationEvidence>,
    #[serde(default)]
    pub actions: Vec<ExplanationVisualAction>,
    pub advance: ExplanationAdvance,
}

/// An authored, topic-independent explanation graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationFlowDefinition {
    pub schema: String,
    pub id: String,
    pub title: String,
    pub goal: String,
    /// Semantic subjects this exploration is about, not architectural parents.
    #[serde(default)]
    pub about: Vec<String>,
    /// Explicit visual objects owned by this exploration, separate from the blueprint.
    #[serde(default)]
    pub scene_ids: Vec<String>,
    pub start: String,
    pub beats: Vec<ExplanationBeat>,
}

/// The independently mutable cursor over an explanation definition.
///
/// It is stored as one JSON scalar in the CRDT. Concurrent peers therefore
/// resolve to one coherent state rather than a current beat from one writer
/// and a transition record from another.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationFlowState {
    pub current_beat: String,
    pub status: String,
    pub revision: u32,
    /// Revision at which the current beat was entered.
    ///
    /// Pause, resume, and stop change the cursor revision but preserve this
    /// value, so browser visual actions do not replay for a control event.
    /// Advance and restart set it to the new revision.
    pub entered_revision: u32,
    #[serde(default)]
    pub last_transition: String,
    #[serde(default)]
    pub last_response: String,
    #[serde(default)]
    pub last_selected_target_ids: Vec<String>,
    #[serde(default)]
    pub touched_by: String,
}

/// The projected shared explanation object.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExplanationFlow {
    pub object_id: String,
    pub definition: ExplanationFlowDefinition,
    pub state: ExplanationFlowState,
    pub created_by: String,
}
