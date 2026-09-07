//! Host-owned semantic targets and ephemeral reciprocal attention.
//!
//! This module deliberately separates three things archived applications used
//! to collapse into DOM events:
//!
//! - an extension-owned semantic namespace (`atlas`, `board`, …);
//! - a stable target id inside that namespace;
//! - short-lived awareness of a participant attending to that target.
//!
//! Awareness is context only. Nothing in this module participates in
//! [`crate::Caller`] authorization, action audiences, data access, or peer
//! trust. It is also transport-neutral: the service publishes typed snapshots
//! through an injected sink; SSE and HTTP are adapters installed by
//! [`crate::App`], not the presence contract.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::{
    ActionRouteDef, ClientModule, Effect, RouteDef, SnapshotPngFuture, StateBacking, StateSnapshot,
    Surface, SurfaceState, SurfaceStore, ToolDef,
};

/// The one runtime event carrying the complete ephemeral awareness projection.
pub const EVENT_NAME: &str = "surface.semantic-targets";

/// A pointer/highlight is intentionally brief.
pub const ACTIVE_LEASE_MS: u64 = 30_000;
/// A connected participant remains merely present after active attention ends.
pub const PRESENCE_LEASE_MS: u64 = 45_000;

const MAX_ID_CHARS: usize = 160;
const MAX_LABEL_CHARS: usize = 120;
const MAX_MESSAGE_CHARS: usize = 240;

/// A stable id whose namespace is the extension that owns its meaning.
///
/// The pair, never `target_id` alone, is the public identity. This is what
/// prevents an `id="42"` in Atlas from resolving against a Board entry with
/// the same local id.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticTargetRef {
    pub extension_id: String,
    pub target_id: String,
}

impl SemanticTargetRef {
    pub fn new(extension_id: impl Into<String>, target_id: impl Into<String>) -> Self {
        Self {
            extension_id: extension_id.into(),
            target_id: target_id.into(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        validate_id("extension id", &self.extension_id)?;
        validate_id("target id", &self.target_id)
    }
}

/// Agent-readable semantics for one registered target.
///
/// There are deliberately no selectors or coordinates. `spatial` says only
/// whether a browser may sensibly derive geometry from its locally registered
/// element; it never puts that geometry into the agent contract.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticTarget {
    pub target: SemanticTargetRef,
    pub label: String,
    pub description: String,
    pub spatial: bool,
}

impl SemanticTarget {
    pub fn new(
        target: SemanticTargetRef,
        label: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            target,
            label: label.into(),
            description: description.into(),
            spatial: false,
        }
    }

    pub fn spatial(mut self, spatial: bool) -> Self {
        self.spatial = spatial;
        self
    }
}

/// Typed anchor handed to the shared conversation composer.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerAnchor {
    pub target: SemanticTargetRef,
    pub label: String,
}

impl From<&SemanticTarget> for ComposerAnchor {
    fn from(target: &SemanticTarget) -> Self {
        Self {
            target: target.target.clone(),
            label: target.label.clone(),
        }
    }
}

/// Human and agent are values of the same participant shape.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantKind {
    Human,
    Agent,
}

/// Identity is descriptive context, not authenticated authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Participant {
    pub id: String,
    pub label: String,
    pub kind: ParticipantKind,
}

impl Participant {
    pub fn human(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: ParticipantKind::Human,
        }
    }

    pub fn agent(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: ParticipantKind::Agent,
        }
    }

    fn validate(&self) -> Result<(), String> {
        validate_id("participant id", &self.id)?;
        let count = self.label.chars().count();
        if self.label.trim().is_empty() {
            return Err("participant label must not be empty".to_string());
        }
        if count > MAX_LABEL_CHARS {
            return Err(format!(
                "participant label is longer than {MAX_LABEL_CHARS} characters"
            ));
        }
        Ok(())
    }
}

/// Why a participant is attending to a target.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionMode {
    Focus,
    Selection,
    Pointer,
    Reveal,
}

/// One active semantic gesture. Geometry remains browser-owned.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    pub mode: AttentionMode,
    pub target: SemanticTarget,
    pub anchor: ComposerAnchor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One participant as projected to browsers and agents.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipantAwareness {
    pub participant: Participant,
    /// `"active"` and `"present"` are intentionally distinct.
    pub status: &'static str,
    pub present: bool,
    pub active: bool,
    pub active_for_ms: u64,
    pub present_for_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
}

/// Complete replace-style projection of ephemeral awareness.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionSnapshot {
    pub schema_version: u32,
    /// A machine-readable reminder that this state cannot authorize anything.
    pub authority: &'static str,
    pub durable: bool,
    pub participants: Vec<ParticipantAwareness>,
}

type Resolver = dyn Fn(&SemanticTargetRef) -> Result<Option<SemanticTarget>, String> + Send + Sync;
type Publish = dyn Fn(AttentionSnapshot) + Send + Sync;
type Clock = dyn Fn() -> u64 + Send + Sync;

#[derive(Clone)]
struct LeasedAttention {
    participant: Participant,
    attention: Option<Attention>,
    active_until: u64,
    present_until: u64,
    sequence: u64,
}

/// In-memory registry and awareness service shared by every enabled namespace.
pub struct SemanticTargetService {
    namespaces: BTreeSet<String>,
    resolver: Arc<Resolver>,
    publish: Arc<Publish>,
    clock: Arc<Clock>,
    sequence: AtomicU64,
    awareness: Mutex<BTreeMap<(ParticipantKind, String), LeasedAttention>>,
}

impl std::fmt::Debug for SemanticTargetService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticTargetService")
            .field("namespaces", &self.namespaces)
            .field("awareness", &self.awareness.lock().len())
            .finish_non_exhaustive()
    }
}

impl SemanticTargetService {
    fn new(
        namespaces: impl IntoIterator<Item = String>,
        resolver: Arc<Resolver>,
        publish: Arc<Publish>,
    ) -> Result<Self, String> {
        let started = Instant::now();
        Self::new_with_clock(
            namespaces,
            resolver,
            publish,
            Arc::new(move || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
        )
    }

    fn new_with_clock(
        namespaces: impl IntoIterator<Item = String>,
        resolver: Arc<Resolver>,
        publish: Arc<Publish>,
        clock: Arc<Clock>,
    ) -> Result<Self, String> {
        let mut registered = BTreeSet::new();
        for namespace in namespaces {
            validate_id("semantic target namespace", &namespace)?;
            if !registered.insert(namespace.clone()) {
                return Err(format!(
                    "semantic target namespace {namespace:?} is registered more than once"
                ));
            }
        }
        Ok(Self {
            namespaces: registered,
            resolver,
            publish,
            clock,
            sequence: AtomicU64::new(0),
            awareness: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn namespaces(&self) -> Vec<String> {
        self.namespaces.iter().cloned().collect()
    }

    /// Resolve one exact `(extension id, target id)` pair to semantics.
    pub fn resolve(&self, target: &SemanticTargetRef) -> Result<SemanticTarget, String> {
        target.validate()?;
        if !self.namespaces.contains(&target.extension_id) {
            return Err(format!(
                "semantic target namespace {:?} is not registered",
                target.extension_id
            ));
        }
        let resolved = (self.resolver)(target)?.ok_or_else(|| {
            format!(
                "semantic target {}:{} does not exist",
                target.extension_id, target.target_id
            )
        })?;
        if resolved.target != *target {
            return Err(format!(
                "semantic target resolver for {:?} returned a different id",
                target.extension_id
            ));
        }
        if resolved.label.trim().is_empty() || resolved.description.trim().is_empty() {
            return Err(format!(
                "semantic target {}:{} resolved without label and description",
                target.extension_id, target.target_id
            ));
        }
        Ok(resolved)
    }

    /// Refresh mere connectivity without claiming active attention.
    pub fn present(&self, participant: Participant) -> Result<AttentionSnapshot, String> {
        participant.validate()?;
        let now = (self.clock)();
        let key = (participant.kind, participant.id.clone());
        let mut awareness = self.awareness.lock();
        let existing = awareness.remove(&key);
        awareness.insert(
            key,
            LeasedAttention {
                participant,
                attention: existing.as_ref().and_then(|record| {
                    (record.active_until > now)
                        .then_some(record.attention.clone())
                        .flatten()
                }),
                active_until: existing
                    .as_ref()
                    .map(|record| record.active_until)
                    .filter(|until| *until > now)
                    .unwrap_or(now),
                present_until: now.saturating_add(PRESENCE_LEASE_MS),
                sequence: existing.as_ref().map(|record| record.sequence).unwrap_or(0),
            },
        );
        drop(awareness);
        Ok(self.publish_current())
    }

    /// Focus/select/point/reveal a semantic target for one leased interval.
    pub fn attend(
        &self,
        participant: Participant,
        mode: AttentionMode,
        target_ref: SemanticTargetRef,
        message: Option<String>,
    ) -> Result<SemanticTarget, String> {
        participant.validate()?;
        let message = normalize_message(message)?;
        let target = self.resolve(&target_ref)?;
        let now = (self.clock)();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        self.awareness.lock().insert(
            (participant.kind, participant.id.clone()),
            LeasedAttention {
                participant,
                attention: Some(Attention {
                    mode,
                    anchor: ComposerAnchor::from(&target),
                    target: target.clone(),
                    message,
                }),
                active_until: now.saturating_add(ACTIVE_LEASE_MS),
                present_until: now.saturating_add(PRESENCE_LEASE_MS),
                sequence,
            },
        );
        self.publish_current();
        Ok(target)
    }

    /// Clear active attention while retaining a short present lease.
    pub fn clear(&self, participant: Participant) -> Result<AttentionSnapshot, String> {
        participant.validate()?;
        let now = (self.clock)();
        let key = (participant.kind, participant.id.clone());
        self.awareness.lock().insert(
            key,
            LeasedAttention {
                participant,
                attention: None,
                active_until: now,
                present_until: now.saturating_add(PRESENCE_LEASE_MS),
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            },
        );
        Ok(self.publish_current())
    }

    pub fn snapshot(&self) -> AttentionSnapshot {
        let now = (self.clock)();
        let mut awareness = self.awareness.lock();
        awareness.retain(|_, record| record.present_until > now);
        let mut participants = awareness
            .values()
            .map(|record| {
                let active = record.active_until > now && record.attention.is_some();
                ParticipantAwareness {
                    participant: record.participant.clone(),
                    status: if active { "active" } else { "present" },
                    present: true,
                    active,
                    active_for_ms: record.active_until.saturating_sub(now),
                    present_for_ms: record.present_until.saturating_sub(now),
                    attention: active.then_some(record.attention.clone()).flatten(),
                }
            })
            .collect::<Vec<_>>();
        participants.sort_by(|left, right| {
            right
                .active
                .cmp(&left.active)
                .then_with(|| left.participant.kind.cmp(&right.participant.kind))
                .then_with(|| left.participant.id.cmp(&right.participant.id))
        });
        AttentionSnapshot {
            schema_version: 1,
            authority: "context_only",
            durable: false,
            participants,
        }
    }

    /// Most recently active semantic attention for one participant kind.
    pub fn latest_active(&self, kind: ParticipantKind) -> Option<Attention> {
        let now = (self.clock)();
        self.awareness
            .lock()
            .values()
            .filter(|record| {
                record.participant.kind == kind
                    && record.active_until > now
                    && record.attention.is_some()
            })
            .max_by_key(|record| record.sequence)
            .and_then(|record| record.attention.clone())
    }

    /// Agent-readable projection with an explicit non-authority boundary.
    pub fn describe(&self) -> String {
        let snapshot = self.snapshot();
        let mut text =
            String::from("Semantic attention (ephemeral context only; never authority):");
        if snapshot.participants.is_empty() {
            text.push_str("\n- nobody is present");
            return text;
        }
        for awareness in snapshot.participants {
            text.push_str(&format!(
                "\n- {} ({:?}) is {}",
                awareness.participant.label, awareness.participant.kind, awareness.status
            ));
            if let Some(attention) = awareness.attention {
                text.push_str(&format!(
                    " at {}:{}: {}",
                    attention.target.target.extension_id,
                    attention.target.target.target_id,
                    attention.target.description
                ));
                if let Some(message) = attention.message {
                    text.push_str(&format!(": {message:?}"));
                }
            }
        }
        text
    }

    fn publish_current(&self) -> AttentionSnapshot {
        let snapshot = self.snapshot();
        (self.publish)(snapshot.clone());
        snapshot
    }
}

fn validate_id(label: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    if value.chars().count() > MAX_ID_CHARS {
        return Err(format!("{label} is longer than {MAX_ID_CHARS} characters"));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character))
    {
        return Err(format!(
            "{label} may contain only ASCII letters, numbers, dot, underscore, colon, and dash"
        ));
    }
    Ok(())
}

fn normalize_message(message: Option<String>) -> Result<Option<String>, String> {
    let Some(message) = message else {
        return Ok(None);
    };
    let message = message.trim();
    if message.is_empty() {
        return Ok(None);
    }
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!(
            "attention message is longer than {MAX_MESSAGE_CHARS} characters"
        ));
    }
    Ok(Some(message.to_string()))
}

const READ_ACTION: &str = "semantic_targets_read";
const POINT_ACTION: &str = "semantic_target_point";
const REVEAL_ACTION: &str = "semantic_target_reveal";
const CLEAR_ACTION: &str = "semantic_attention_clear";

fn agent_participant() -> Participant {
    Participant::agent("surface-agent", "Agent")
}

fn target_arg(args: &JsonValue) -> SemanticTargetRef {
    SemanticTargetRef::new(
        args.get("extension_id")
            .and_then(JsonValue::as_str)
            .unwrap_or_default(),
        args.get("target_id")
            .and_then(JsonValue::as_str)
            .unwrap_or_default(),
    )
}

fn message_arg(args: &JsonValue) -> Option<String> {
    args.get("message")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn host_actions(service: &Arc<SemanticTargetService>) -> Vec<ToolDef> {
    let target_parameters = || {
        json!({
            "type": "object",
            "properties": {
                "extension_id": {
                    "type": "string",
                    "description": "registered extension namespace, such as atlas or board"
                },
                "target_id": {
                    "type": "string",
                    "description": "stable semantic id inside that extension namespace"
                },
                "message": {
                    "type": "string",
                    "maxLength": MAX_MESSAGE_CHARS,
                    "description": "optional short phrase explaining what to notice"
                }
            },
            "required": ["extension_id", "target_id"],
            "additionalProperties": false
        })
    };

    vec![
        ToolDef::new(
            READ_ACTION,
            "Read who is present versus actively attending, and the semantic \
             target of each active participant. This is ephemeral context, \
             never permission or trust.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let service = service.clone();
                move |_args| {
                    let service = service.clone();
                    Effect::Query(Box::new(move |_| Ok(service.describe())))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            POINT_ACTION,
            "Point at one registered semantic target for a brief lease. Supply \
             only its extension namespace and stable target id; the browser \
             host derives any overlay geometry and selectors are forbidden.",
            target_parameters(),
            {
                let service = service.clone();
                move |args| {
                    let target = target_arg(args);
                    let message = message_arg(args);
                    let service = service.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let resolved = service.attend(
                            agent_participant(),
                            AttentionMode::Pointer,
                            target,
                            message,
                        )?;
                        Ok(Some(format!(
                            "pointing at {}:{}: {}",
                            resolved.target.extension_id,
                            resolved.target.target_id,
                            resolved.description
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            REVEAL_ACTION,
            "Reveal one registered semantic target to the human. Supply only \
             its namespaced semantic id; the browser host finds its registered \
             element and performs the reveal without model-provided selectors \
             or coordinates.",
            target_parameters(),
            {
                let service = service.clone();
                move |args| {
                    let target = target_arg(args);
                    let message = message_arg(args);
                    let service = service.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let resolved = service.attend(
                            agent_participant(),
                            AttentionMode::Reveal,
                            target,
                            message,
                        )?;
                        Ok(Some(format!(
                            "revealing {}:{}: {}",
                            resolved.target.extension_id,
                            resolved.target.target_id,
                            resolved.description
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            CLEAR_ACTION,
            "Clear this agent's active semantic pointer. The agent remains \
             merely present for its short ephemeral lease.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let service = service.clone();
                move |_args| {
                    let service = service.clone();
                    Effect::Mutate(Box::new(move |_| {
                        service.clear(agent_participant())?;
                        Ok(Some("cleared the agent's semantic pointer".to_string()))
                    }))
                }
            },
        )
        .agent_only(),
    ]
}

struct HostedState {
    inner: Arc<dyn Surface>,
    service: Arc<SemanticTargetService>,
}

impl SurfaceState for HostedState {
    fn backing(&self) -> StateBacking {
        self.inner.state().backing()
    }

    fn describe(&self) -> Result<String, String> {
        self.inner.state().describe()
    }

    fn snapshot_png(&self) -> SnapshotPngFuture<'_> {
        self.inner.state().snapshot_png()
    }

    fn snapshot(&self) -> Result<StateSnapshot, String> {
        self.inner.state().snapshot()
    }

    fn activity_state_revision(&self) -> Result<Vec<crate::ActivityStateRevision>, String> {
        self.inner.state().activity_state_revision()
    }

    fn activity_feed(&self) -> Option<&crate::ActivityFeed> {
        self.inner.state().activity_feed()
    }

    fn resolve(&self, id: &str) -> Result<Option<String>, String> {
        self.inner.state().resolve(id)
    }

    fn semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        self.inner.state().semantic_target(target)
    }

    fn semantic_targets(&self) -> Option<&SemanticTargetService> {
        Some(self.service.as_ref())
    }

    fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
        self.inner.state().ws_hello()
    }

    fn ws_receive(&self, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.inner.state().ws_receive(data)
    }

    fn reconnect_events(&self) -> Vec<(String, JsonValue)> {
        let mut events = self.inner.state().reconnect_events();
        events.push((
            EVENT_NAME.to_string(),
            serde_json::to_value(self.service.snapshot())
                .unwrap_or_else(|error| json!({ "error": error.to_string() })),
        ));
        events
    }
}

struct HostedSurface {
    inner: Arc<dyn Surface>,
    state: HostedState,
    tools: Vec<ToolDef>,
}

impl Surface for HostedSurface {
    fn state(&self) -> &dyn SurfaceState {
        &self.state
    }

    fn tools(&self) -> &[ToolDef] {
        &self.tools
    }

    fn client_modules(&self) -> Vec<ClientModule> {
        self.inner.client_modules()
    }

    fn store(&self) -> Option<&dyn SurfaceStore> {
        self.inner.store()
    }

    fn routes(&self) -> Vec<RouteDef> {
        self.inner.routes()
    }

    fn action_routes(&self) -> Vec<ActionRouteDef> {
        self.inner.action_routes()
    }

    fn focus_events(&self) -> &[&str] {
        self.inner.focus_events()
    }

    fn note_caller(&self, actor: &crate::Actor) {
        self.inner.note_caller(actor);
    }

    fn resolve_focus(&self, event: &str, id: &str) -> Result<Option<String>, String> {
        self.inner.resolve_focus(event, id)
    }

    fn semantic_target_namespaces(&self) -> Vec<String> {
        self.inner.semantic_target_namespaces()
    }

    fn resolve_semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        self.inner.resolve_semantic_target(target)
    }
}

type SemanticTargetInstallation = (Arc<dyn Surface>, Option<Arc<SemanticTargetService>>);

/// Install the host service before action schemas, MCP, and browser manifests
/// are compiled. A surface that registers no namespaces is returned unchanged
/// and does not advertise unusable attention actions.
pub(crate) fn install(
    surface: Arc<dyn Surface>,
    publish: impl Fn(AttentionSnapshot) + Send + Sync + 'static,
) -> Result<SemanticTargetInstallation, String> {
    let namespaces = surface.semantic_target_namespaces();
    if namespaces.is_empty() {
        // Presence is session state, not target state: a surface with nothing
        // to point at still has people on it. The service runs so `/mcp`
        // attachments and browser check-ins register, but the surface is not
        // wrapped — with no namespaces there is nothing for the pointing
        // actions to do, so agents are not shown them.
        let resolver_surface = surface.clone();
        let service = Arc::new(SemanticTargetService::new(
            Vec::new(),
            Arc::new(move |target| resolver_surface.resolve_semantic_target(target)),
            Arc::new(publish),
        )?);
        return Ok((surface, Some(service)));
    }

    let resolver_surface = surface.clone();
    let service = Arc::new(SemanticTargetService::new(
        namespaces,
        Arc::new(move |target| resolver_surface.resolve_semantic_target(target)),
        Arc::new(publish),
    )?);

    let reserved = [READ_ACTION, POINT_ACTION, REVEAL_ACTION, CLEAR_ACTION]
        .into_iter()
        .collect::<HashSet<_>>();
    if let Some(action) = surface
        .tools()
        .iter()
        .find(|action| reserved.contains(action.name.as_str()))
    {
        return Err(format!(
            "surface action {:?} collides with a semantic-target host action",
            action.name
        ));
    }
    let mut tools = surface.tools().to_vec();
    tools.extend(host_actions(&service));
    let hosted: Arc<dyn Surface> = Arc::new(HostedSurface {
        state: HostedState {
            inner: surface.clone(),
            service: service.clone(),
        },
        inner: surface,
        tools,
    });
    Ok((hosted, Some(service)))
}

pub(crate) const BROWSER_JS: &str = include_str!("semantic-targets.js");
pub(crate) const BROWSER_CSS: &str = include_str!("semantic-targets.css");

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn fixture_service(
        now: Arc<AtomicU64>,
        published: Arc<Mutex<Vec<AttentionSnapshot>>>,
    ) -> SemanticTargetService {
        SemanticTargetService::new_with_clock(
            ["atlas".to_string(), "board".to_string()],
            Arc::new(|target| {
                Ok(Some(SemanticTarget::new(
                    target.clone(),
                    format!("{} label", target.extension_id),
                    format!("{} meaning for {}", target.extension_id, target.target_id),
                )))
            }),
            Arc::new(move |snapshot| published.lock().push(snapshot)),
            Arc::new(move || now.load(Ordering::Relaxed)),
        )
        .expect("valid fixture service")
    }

    #[test]
    fn namespace_is_part_of_target_identity() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let atlas = service
            .resolve(&SemanticTargetRef::new("atlas", "same"))
            .expect("atlas target");
        let board = service
            .resolve(&SemanticTargetRef::new("board", "same"))
            .expect("board target");
        assert_ne!(atlas.description, board.description);
        assert_eq!(atlas.target.target_id, board.target.target_id);
    }

    #[test]
    fn active_expires_to_present_before_presence_expires() {
        let now = Arc::new(AtomicU64::new(10));
        let service = fixture_service(now.clone(), Arc::new(Mutex::new(Vec::new())));
        service
            .attend(
                Participant::human("human-1", "Human"),
                AttentionMode::Selection,
                SemanticTargetRef::new("atlas", "node-1"),
                None,
            )
            .expect("focus accepted");
        let active = service.snapshot();
        assert!(active.participants[0].active);
        assert_eq!(active.participants[0].status, "active");

        now.store(10 + ACTIVE_LEASE_MS + 1, Ordering::Relaxed);
        let present = service.snapshot();
        assert!(!present.participants[0].active);
        assert!(present.participants[0].present);
        assert_eq!(present.participants[0].status, "present");
        assert!(present.participants[0].attention.is_none());

        now.store(10 + PRESENCE_LEASE_MS + 1, Ordering::Relaxed);
        assert!(service.snapshot().participants.is_empty());
    }

    #[test]
    fn human_and_agent_have_the_same_serialized_shape() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        service
            .present(Participant::human("h", "Human"))
            .expect("human present");
        service
            .present(Participant::agent("a", "Agent"))
            .expect("agent present");
        let value = serde_json::to_value(service.snapshot()).expect("snapshot serializes");
        let participants = value["participants"].as_array().expect("participants");
        assert_eq!(
            participants[0]
                .as_object()
                .expect("participant object")
                .keys()
                .collect::<Vec<_>>(),
            participants[1]
                .as_object()
                .expect("participant object")
                .keys()
                .collect::<Vec<_>>()
        );
        assert_eq!(value["authority"], "context_only");
        assert_eq!(value["durable"], false);
    }

    #[test]
    fn host_action_schemas_never_accept_geometry_or_selectors() {
        let service = Arc::new(fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        ));
        let schemas = serde_json::to_string(
            &host_actions(&service)
                .into_iter()
                .map(|action| action.parameters)
                .collect::<Vec<_>>(),
        )
        .expect("schemas serialize");
        for forbidden in ["selector", "\"x\"", "\"y\"", "coordinate"] {
            assert!(
                !schemas.contains(forbidden),
                "host schema leaked forbidden geometry term {forbidden}"
            );
        }
    }

    #[test]
    fn browser_registry_uses_element_handles_not_global_dom_lookup() {
        for forbidden in [
            "getElementById",
            "querySelector",
            "CustomEvent",
            "dispatchEvent",
        ] {
            assert!(
                !BROWSER_JS.contains(forbidden),
                "semantic-target browser host contains forbidden global coupling {forbidden}"
            );
        }
        assert!(BROWSER_JS.contains("getBoundingClientRect"));
        assert!(BROWSER_JS.contains("scrollIntoView"));
    }
}
