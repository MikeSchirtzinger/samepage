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
    ActionRouteDef, Actor, Caller, ClientModule, Effect, RouteDef, SnapshotPngFuture, StateBacking,
    StateSnapshot, Surface, SurfaceState, SurfaceStore, ToolDef,
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
/// How many targets one gesture may carry.
///
/// A marquee over a whole board is a real gesture, so this is generous rather
/// than tight; it exists to stop an unbounded body from becoming an unbounded
/// read-back sentence.
pub const MAX_ATTENTION_TARGETS: usize = 200;
/// How many names a read-back sentence prints before it summarises the tail.
const NAMED_IN_READ_BACK: usize = 8;

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
///
/// It carries the whole gesture, not its first member. It used to carry one
/// target, so a human who marqueed four cards read `4 selected` in the
/// inspector, `Pointing at 4 selected: ... and 1 more` on the board, and
/// `pointing at Atlas pane renderer` on the composer. The surface attached to
/// the *question* was the one speaking in the singular, which is the worst
/// place for it: the composer is what a person looks at to check what "these"
/// is about to mean. The wire was never the problem, the whole ordered set
/// was already on it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerAnchor {
    /// The first member, which stays the single-target contract every
    /// existing reader already speaks.
    pub target: SemanticTargetRef,
    /// What the composer shows: one target's label, or the set's own
    /// sentence with its count first.
    pub label: String,
    pub count: usize,
    /// Every member's label, in the order the human built the set.
    pub labels: Vec<String>,
}

/// How many names a composer chip prints before it counts the tail. Three,
/// matching the board chip and the selection panel, so a person reading two
/// surfaces at once is reading the same sentence twice rather than comparing
/// two different claims.
const NAMED_IN_ANCHOR: usize = 3;

impl ComposerAnchor {
    /// The anchor for one whole gesture, first member first.
    pub fn over(targets: &[&SemanticTarget]) -> Self {
        let first = targets.first().expect("attention always carries a target");
        let labels: Vec<String> = targets
            .iter()
            .map(|target| target.label.trim().to_string())
            .collect();
        Self {
            target: first.target.clone(),
            label: anchor_label(&labels),
            count: labels.len(),
            labels,
        }
    }
}

/// One phrase for a set, count first. The count is the fact a wrong answer
/// usually gets wrong, and the one that survives truncation.
fn anchor_label(labels: &[String]) -> String {
    match labels.len() {
        0 => String::new(),
        1 => labels[0].clone(),
        count if count <= NAMED_IN_ANCHOR => format!("{count} selected: {}", labels.join(", ")),
        count => format!(
            "{count} selected: {}, and {} more",
            labels[..NAMED_IN_ANCHOR].join(", "),
            count - NAMED_IN_ANCHOR
        ),
    }
}

impl From<&SemanticTarget> for ComposerAnchor {
    fn from(target: &SemanticTarget) -> Self {
        Self::over(&[target])
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

impl AttentionMode {
    /// Is this something the participant is still *holding*, rather than a
    /// moment that has passed?
    ///
    /// The distinction decides whether a presence heartbeat renews the active
    /// lease. It exists because a selection was being stored in a lease
    /// designed for a pointer: a human selected four cards, thought about
    /// them for ninety seconds, and the set died underneath them with nothing
    /// on screen saying so, which silently breaks select, think, type, ask.
    /// A pointer and a reveal genuinely are brief and are left alone.
    ///
    /// The line is the same one the routes already draw: `Focus` and
    /// `Selection` arrive from a browser through
    /// `/semantic-targets/focus`, so somebody is holding them and their
    /// browser says so every twenty seconds; `Pointer` and `Reveal` are the
    /// agent's own tools and nothing renews them.
    pub fn is_held(self) -> bool {
        matches!(self, Self::Selection | Self::Focus)
    }
}

/// One active semantic gesture. Geometry remains browser-owned.
///
/// A gesture may carry more than one target: a marquee is one act of attention
/// over a set, not N separate acts. `target` is the first member and stays the
/// single-target contract every existing reader already speaks;
/// `additional_targets` carries the rest **in the order the human built the
/// set**, which is why it is a `Vec` and not a set type. Anything that wants
/// the whole gesture asks [`Attention::targets`], and anything that wants to
/// know whether this was one thing or several asks [`Attention::count`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attention {
    pub mode: AttentionMode,
    pub target: SemanticTarget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_targets: Vec<SemanticTarget>,
    pub anchor: ComposerAnchor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Attention {
    /// Every target of this one gesture, first member first.
    pub fn targets(&self) -> Vec<&SemanticTarget> {
        std::iter::once(&self.target)
            .chain(self.additional_targets.iter())
            .collect()
    }

    /// How many targets this one gesture carries. Never zero.
    pub fn count(&self) -> usize {
        1 + self.additional_targets.len()
    }

    /// The one sentence a set of targets reads as.
    ///
    /// Deliberately one sentence rather than one line per member: a human who
    /// marquees six cards made a single gesture, and a read-back that files it
    /// as six events tells the agent something that did not happen.
    pub fn read_back_sentence(&self, who: &str, verb: &str) -> String {
        let labels = self
            .targets()
            .into_iter()
            .map(|target| quoted(&target.label))
            .collect::<Vec<_>>();
        if labels.len() == 1 {
            return format!("{who} {verb} {}", labels[0]);
        }
        let count = labels.len();
        if count <= NAMED_IN_READ_BACK {
            format!("{who} {verb} {count}: {}", labels.join(", "))
        } else {
            format!(
                "{who} {verb} {count}: {}, and {} more",
                labels[..NAMED_IN_READ_BACK].join(", "),
                count - NAMED_IN_READ_BACK
            )
        }
    }
}

fn quoted(label: &str) -> String {
    format!("{:?}", label.trim())
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
    /// Bumped only when a **human** gesture changes, so an agent parking on
    /// [`Self::changed`] is not woken by its own pointer.
    human_sequence: AtomicU64,
    changed: tokio::sync::Notify,
    awareness: Mutex<BTreeMap<(ParticipantKind, String), LeasedAttention>>,
    /// The runtime resolves the calling actor before applying a tool. Capture
    /// that participant here so attention updates the MCP session's existing
    /// presence record instead of inventing a generic third participant.
    current_participant: Mutex<Participant>,
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
            human_sequence: AtomicU64::new(0),
            changed: tokio::sync::Notify::new(),
            awareness: Mutex::new(BTreeMap::new()),
            current_participant: Mutex::new(Participant::agent("surface-agent", "Agent")),
        })
    }

    /// Build a service against a resolver the caller owns.
    ///
    /// [`crate::App`] installs one automatically from the surface, which is the
    /// path every application takes. This exists for a host wiring the pieces
    /// itself and for tests that need a real service rather than a stand-in,
    /// because an attention contract proven against a fake is not proven.
    pub fn with_resolver(
        namespaces: impl IntoIterator<Item = String>,
        resolver: impl Fn(&SemanticTargetRef) -> Result<Option<SemanticTarget>, String>
            + Send
            + Sync
            + 'static,
        publish: impl Fn(AttentionSnapshot) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::new(namespaces, Arc::new(resolver), Arc::new(publish))
    }

    pub fn namespaces(&self) -> Vec<String> {
        self.namespaces.iter().cloned().collect()
    }

    fn note_caller(&self, actor: &Actor) {
        let participant = participant_from_actor(actor);
        *self.current_participant.lock() = participant;
    }

    /// The participant resolved at the dispatch boundary for the current
    /// action. Capturing this before constructing an effect prevents a later
    /// caller from changing the byline of an already-created operation.
    pub fn calling_participant(&self) -> Participant {
        self.current_participant.lock().clone()
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
    ///
    /// A **held** gesture is renewed here; a brief one is not. See
    /// [`AttentionMode::is_held`]: a selection is a thing the human is still
    /// holding, and the heartbeat from the browser holding it is the evidence
    /// that they are. A pointer or a reveal is a moment, and it still lapses
    /// on its own no matter how long the browser stays connected.
    pub fn present(&self, participant: Participant) -> Result<AttentionSnapshot, String> {
        participant.validate()?;
        let now = (self.clock)();
        let key = (participant.kind, participant.id.clone());
        let mut awareness = self.awareness.lock();
        let existing = awareness.remove(&key);
        let live_attention = existing.as_ref().and_then(|record| {
            (record.active_until > now)
                .then_some(record.attention.clone())
                .flatten()
        });
        let holds = live_attention
            .as_ref()
            .is_some_and(|attention| attention.mode.is_held());
        awareness.insert(
            key,
            LeasedAttention {
                participant,
                attention: live_attention,
                // To the *presence* lease, not the active one. A held
                // gesture is alive exactly as long as its holder is, which is
                // the honest statement and also the robust one: renewing to
                // the shorter active lease would let a single dropped
                // heartbeat kill a selection the human is looking at.
                active_until: if holds {
                    now.saturating_add(PRESENCE_LEASE_MS)
                } else {
                    existing
                        .as_ref()
                        .map(|record| record.active_until)
                        .filter(|until| *until > now)
                        .unwrap_or(now)
                },
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
        self.attend_many(participant, mode, vec![target_ref], message)
            .map(|mut resolved| resolved.remove(0))
    }

    /// One gesture over a set of targets, in the order the human built it.
    ///
    /// This is a single act of attention, so it takes one sequence number,
    /// fires one wake, and publishes one snapshot. A caller that loops over
    /// [`Self::attend`] instead would tell every waiter that N things happened.
    pub fn attend_many(
        &self,
        participant: Participant,
        mode: AttentionMode,
        target_refs: Vec<SemanticTargetRef>,
        message: Option<String>,
    ) -> Result<Vec<SemanticTarget>, String> {
        participant.validate()?;
        let message = normalize_message(message)?;
        if target_refs.is_empty() {
            return Err("attention needs at least one semantic target".to_string());
        }
        if target_refs.len() > MAX_ATTENTION_TARGETS {
            return Err(format!(
                "attention carries {} targets, more than the {MAX_ATTENTION_TARGETS} a single gesture may hold",
                target_refs.len()
            ));
        }
        // Resolve the whole set before writing any of it. A half-applied
        // gesture would leave the human selecting things the read-back denies.
        let mut resolved = Vec::with_capacity(target_refs.len());
        let mut seen = HashSet::new();
        for target_ref in target_refs {
            if !seen.insert(target_ref.clone()) {
                continue;
            }
            resolved.push(self.resolve(&target_ref)?);
        }
        let now = (self.clock)();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let target = resolved[0].clone();
        let additional_targets = resolved[1..].to_vec();
        let kind = participant.kind;
        self.awareness.lock().insert(
            (participant.kind, participant.id.clone()),
            LeasedAttention {
                participant,
                attention: Some(Attention {
                    mode,
                    anchor: ComposerAnchor::over(
                        &std::iter::once(&target)
                            .chain(additional_targets.iter())
                            .collect::<Vec<_>>(),
                    ),
                    target,
                    additional_targets,
                    message,
                }),
                active_until: now.saturating_add(ACTIVE_LEASE_MS),
                present_until: now.saturating_add(PRESENCE_LEASE_MS),
                sequence,
            },
        );
        self.publish_current();
        self.note_human_change(kind);
        Ok(resolved)
    }

    /// Revision of the newest human gesture. A waiter snapshots this before
    /// parking, so a gesture that lands between the check and the await is
    /// still seen when the notification arrives.
    pub fn human_attention_revision(&self) -> u64 {
        self.human_sequence.load(Ordering::Acquire)
    }

    /// Fires when a human's attention changes. Never for an agent's own.
    pub fn changed(&self) -> &tokio::sync::Notify {
        &self.changed
    }

    fn note_human_change(&self, kind: ParticipantKind) {
        if kind != ParticipantKind::Human {
            return;
        }
        self.human_sequence.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_waiters();
    }

    /// Clear active attention while retaining a short present lease.
    pub fn clear(&self, participant: Participant) -> Result<AttentionSnapshot, String> {
        participant.validate()?;
        let now = (self.clock)();
        let key = (participant.kind, participant.id.clone());
        let kind = participant.kind;
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
        let snapshot = self.publish_current();
        self.note_human_change(kind);
        Ok(snapshot)
    }

    /// Remove a participant immediately because its transport closed on
    /// purpose. This is different from clearing attention: `clear` keeps the
    /// participant present for the normal lease, while a deliberate departure
    /// is already direct evidence that the participant left.
    pub fn depart(&self, participant: Participant) -> Result<AttentionSnapshot, String> {
        participant.validate()?;
        let kind = participant.kind;
        let key = (kind, participant.id.clone());
        let removed = self.awareness.lock().remove(&key).is_some();
        let snapshot = self.publish_current();
        if removed {
            self.note_human_change(kind);
        }
        Ok(snapshot)
    }

    pub fn snapshot(&self) -> AttentionSnapshot {
        let now = (self.clock)();
        let mut awareness = self.awareness.lock();
        awareness.retain(|_, record| record.present_until > now);
        self.invalidate_missing_targets(now, &mut awareness);
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
        self.latest_active_where(kind, |_| true)
    }

    /// Most recently active attention matching one semantic mode and namespace.
    /// A newer gesture of another kind must not hide a still-live selection a
    /// caller explicitly asked for.
    pub fn latest_active_matching(
        &self,
        kind: ParticipantKind,
        mode: AttentionMode,
        extension_id: &str,
    ) -> Option<Attention> {
        self.latest_active_where(kind, |attention| {
            attention.mode == mode && attention.target.target.extension_id == extension_id
        })
    }

    fn latest_active_where(
        &self,
        kind: ParticipantKind,
        predicate: impl Fn(&Attention) -> bool,
    ) -> Option<Attention> {
        let now = (self.clock)();
        let mut awareness = self.awareness.lock();
        awareness.retain(|_, record| record.present_until > now);
        self.invalidate_missing_targets(now, &mut awareness);
        awareness
            .values()
            .filter(|record| {
                record.participant.kind == kind
                    && record.active_until > now
                    && record.attention.as_ref().is_some_and(&predicate)
            })
            .max_by_key(|record| record.sequence)
            .and_then(|record| record.attention.clone())
    }

    /// A semantic id can disappear after it was pointed at. Re-resolve every
    /// active target at the moment awareness is read and retain the
    /// participant as merely present when the target is gone. No subsequent
    /// question can then inherit context for an object that no longer exists.
    fn invalidate_missing_targets(
        &self,
        now: u64,
        awareness: &mut BTreeMap<(ParticipantKind, String), LeasedAttention>,
    ) {
        for record in awareness.values_mut() {
            let Some(attention) = record.attention.as_mut() else {
                continue;
            };
            // A set survives the loss of any one member, including the first:
            // deleting one of six marqueed cards leaves five things the human
            // is still pointing at, and blanking the whole gesture would lose
            // context that is genuinely still on their screen. Only an empty
            // survivor list invalidates the attention outright.
            let mut survivors = Vec::with_capacity(attention.count());
            for target in attention.targets() {
                let target_ref = target.target.clone();
                match (self.resolver)(&target_ref) {
                    Ok(Some(resolved)) if resolved.target == target_ref => survivors.push(resolved),
                    Ok(Some(_)) | Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(
                            extension = %target_ref.extension_id,
                            target = %target_ref.target_id,
                            %error,
                            "semantic attention target could not be re-resolved"
                        );
                    }
                }
            }
            if survivors.is_empty() {
                record.attention = None;
                record.active_until = now;
                continue;
            }
            attention.target = survivors.remove(0);
            attention.additional_targets = survivors;
            // Rebuilt from what is left, so a set that lost a member says so
            // on the composer in the same breath the read-back does.
            attention.anchor = ComposerAnchor::over(&attention.targets());
        }
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
                if attention.count() == 1 {
                    text.push_str(&format!(
                        " at {}:{}: {}",
                        attention.target.target.extension_id,
                        attention.target.target.target_id,
                        attention.target.description
                    ));
                } else {
                    // One gesture, one sentence. The ids ride along after the
                    // names so an agent can act on the set without a second
                    // lookup, but the count comes first because that is the
                    // fact a wrong answer usually gets wrong.
                    text.push_str(&format!(
                        " over {} in {}: {}",
                        attention.count(),
                        attention.target.target.extension_id,
                        attention
                            .targets()
                            .into_iter()
                            .map(|target| format!(
                                "{} ({})",
                                quoted(&target.label),
                                target.target.target_id
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
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

fn participant_from_actor(actor: &Actor) -> Participant {
    let (kind, fallback_id, fallback_label) = match actor.caller {
        Caller::Human => (ParticipantKind::Human, "surface-human", "Human"),
        Caller::Agent => (ParticipantKind::Agent, "surface-agent", "Agent"),
        Caller::Companion => (ParticipantKind::Agent, "companion-agent", "Companion agent"),
        Caller::Unknown => (ParticipantKind::Agent, "unknown-caller", "Unknown caller"),
    };
    let participant = Participant {
        id: actor
            .participant_id
            .clone()
            .unwrap_or_else(|| fallback_id.to_string()),
        label: actor
            .label
            .clone()
            .unwrap_or_else(|| fallback_label.to_string()),
        kind,
    };
    if participant.validate().is_ok() {
        participant
    } else {
        Participant {
            id: fallback_id.to_string(),
            label: fallback_label.to_string(),
            kind,
        }
    }
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
                    let participant = service.calling_participant();
                    let service = service.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let resolved =
                            service.attend(participant, AttentionMode::Pointer, target, message)?;
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
             or coordinates. Use this deliberately for a guided walkthrough \
             when the target is not already readable. Prefer pointing when it \
             is visible, because reveal may pan or zoom the human's view.",
            target_parameters(),
            {
                let service = service.clone();
                move |args| {
                    let target = target_arg(args);
                    let message = message_arg(args);
                    let participant = service.calling_participant();
                    let service = service.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let resolved =
                            service.attend(participant, AttentionMode::Reveal, target, message)?;
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
                    let participant = service.calling_participant();
                    let service = service.clone();
                    Effect::Mutate(Box::new(move |_| {
                        service.clear(participant)?;
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
        self.state.service.note_caller(actor);
        self.inner.note_caller(actor);
    }

    fn resolve_focus(&self, event: &str, id: &str) -> Result<Option<String>, String> {
        self.inner.resolve_focus(event, id)
    }

    fn bind_semantic_targets(&self, service: &Arc<SemanticTargetService>) {
        self.inner.bind_semantic_targets(service);
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
        // wrapped, with no namespaces there is nothing for the pointing
        // actions to do, so agents are not shown them.
        let resolver_surface = surface.clone();
        let service = Arc::new(SemanticTargetService::new(
            Vec::new(),
            Arc::new(move |target| resolver_surface.resolve_semantic_target(target)),
            Arc::new(publish),
        )?);
        surface.bind_semantic_targets(&service);
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
    surface.bind_semantic_targets(&service);
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
    use std::sync::atomic::{AtomicBool, AtomicU64};

    struct NoopState;

    impl SurfaceState for NoopState {
        fn backing(&self) -> StateBacking {
            StateBacking::Ephemeral
        }

        fn describe(&self) -> Result<String, String> {
            Ok(String::new())
        }

        fn snapshot(&self) -> Result<StateSnapshot, String> {
            Ok(StateSnapshot {
                backing: StateBacking::Ephemeral,
                body: JsonValue::Null,
                chrome: None,
            })
        }
    }

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
    fn one_gesture_carries_a_whole_set_in_the_order_it_was_built() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let resolved = service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                vec![
                    SemanticTargetRef::new("atlas", "third"),
                    SemanticTargetRef::new("atlas", "first"),
                    SemanticTargetRef::new("atlas", "second"),
                ],
                None,
            )
            .expect("a marquee over three");
        assert_eq!(resolved.len(), 3);

        let attention = service
            .latest_active_matching(ParticipantKind::Human, AttentionMode::Selection, "atlas")
            .expect("the selection is live");
        assert_eq!(attention.count(), 3);
        assert_eq!(
            attention
                .targets()
                .into_iter()
                .map(|target| target.target.target_id.as_str())
                .collect::<Vec<_>>(),
            ["third", "first", "second"],
            "selection order is the human's, not sorted"
        );
        // Inverted on purpose. This used to assert "the composer anchors on
        // the first member", which was the defect written down as a rule: the
        // inspector said `3 selected`, the board chip said `3 selected: ...`,
        // and the composer, the one surface attached to the question, named
        // one card. The whole ordered set was already on the wire.
        assert_eq!(
            attention.anchor.count, 3,
            "the anchor carries the gesture, not its head"
        );
        assert_eq!(
            attention.anchor.labels.len(),
            3,
            "and every member's name, in the human's order"
        );
        assert!(
            attention.anchor.label.starts_with("3 selected: "),
            "count first, the way the board chip and the panel already say it: {}",
            attention.anchor.label
        );
        assert_eq!(
            attention.anchor.target, attention.target.target,
            "the single-target contract still points at the first member"
        );
    }

    /// Past three names the chip counts the tail rather than growing without
    /// bound, which is the same rule the selection panel and the board chip
    /// already apply, so two surfaces read as one sentence twice.
    #[test]
    fn a_large_set_anchors_on_a_count_and_three_names() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                ["a", "b", "c", "d", "e"]
                    .into_iter()
                    .map(|id| SemanticTargetRef::new("atlas", id))
                    .collect(),
                None,
            )
            .expect("a marquee over five");
        let attention = service
            .latest_active(ParticipantKind::Human)
            .expect("the selection is live");
        assert_eq!(attention.anchor.count, 5);
        assert!(
            attention.anchor.label.starts_with("5 selected: ")
                && attention.anchor.label.ends_with(", and 2 more"),
            "{}",
            attention.anchor.label
        );
        assert_eq!(
            attention.anchor.labels.len(),
            5,
            "the tail is summarised for reading, not dropped from the payload"
        );
    }

    #[test]
    fn a_repeated_target_does_not_inflate_the_count() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                vec![
                    SemanticTargetRef::new("atlas", "one"),
                    SemanticTargetRef::new("atlas", "one"),
                    SemanticTargetRef::new("atlas", "two"),
                ],
                None,
            )
            .expect("a set with a duplicate");
        let attention = service
            .latest_active(ParticipantKind::Human)
            .expect("the selection is live");
        assert_eq!(attention.count(), 2, "a set counts things, not clicks");
    }

    #[test]
    fn an_empty_or_oversized_selection_is_refused_honestly() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let empty = service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                Vec::new(),
                None,
            )
            .expect_err("an empty gesture is not a gesture");
        assert!(empty.contains("at least one"), "{empty}");

        let too_many = service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                (0..=MAX_ATTENTION_TARGETS)
                    .map(|index| SemanticTargetRef::new("atlas", format!("n{index}")))
                    .collect(),
                None,
            )
            .expect_err("a gesture has a ceiling");
        assert!(
            too_many.contains(&MAX_ATTENTION_TARGETS.to_string()),
            "{too_many}"
        );
    }

    #[test]
    fn a_set_reads_back_as_one_sentence_that_counts_first() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                ["ingest", "queue", "worker"]
                    .into_iter()
                    .map(|id| SemanticTargetRef::new("atlas", id))
                    .collect(),
                None,
            )
            .expect("a marquee over three");
        let text = service.describe();
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("is active"))
                .count(),
            1,
            "one gesture is one line:\n{text}"
        );
        assert!(text.contains("over 3 in atlas"), "{text}");
        for id in ["ingest", "queue", "worker"] {
            assert!(text.contains(id), "{id} missing from:\n{text}");
        }
    }

    #[test]
    fn losing_one_member_prunes_the_set_and_losing_all_of_it_clears() {
        let live = Arc::new(Mutex::new(
            ["a", "b", "c"]
                .into_iter()
                .map(str::to_string)
                .collect::<BTreeSet<_>>(),
        ));
        let resolver_live = live.clone();
        let service = SemanticTargetService::new_with_clock(
            ["atlas".to_string()],
            Arc::new(move |target: &SemanticTargetRef| {
                Ok(resolver_live
                    .lock()
                    .contains(&target.target_id)
                    .then(|| SemanticTarget::new(target.clone(), &target.target_id, "a card")))
            }),
            Arc::new(|_| {}),
            Arc::new(|| 0),
        )
        .expect("service");

        service
            .attend_many(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                ["a", "b", "c"]
                    .into_iter()
                    .map(|id| SemanticTargetRef::new("atlas", id))
                    .collect(),
                None,
            )
            .expect("select three");

        live.lock().remove("a");
        let attention = service
            .latest_active(ParticipantKind::Human)
            .expect("two members are still on screen");
        assert_eq!(attention.count(), 2);
        assert_eq!(
            attention.target.target.target_id, "b",
            "losing the first member promotes the next, it does not blank the gesture"
        );
        assert_eq!(
            attention.anchor.label, "2 selected: b, c",
            "the anchor follows the promotion and still describes the set"
        );
        assert_eq!(attention.anchor.count, 2);
        assert_eq!(
            attention.anchor.target.target_id, "b",
            "and its single-target head is the promoted member"
        );

        live.lock().clear();
        assert!(
            service.latest_active(ParticipantKind::Human).is_none(),
            "an empty set is no selection at all"
        );
    }

    #[test]
    fn only_a_human_gesture_bumps_the_wake_revision() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let start = service.human_attention_revision();

        service
            .attend(
                Participant::agent("serving-agent", "Serving agent"),
                AttentionMode::Pointer,
                SemanticTargetRef::new("atlas", "one"),
                None,
            )
            .expect("the agent points");
        assert_eq!(
            service.human_attention_revision(),
            start,
            "an agent must not wake itself"
        );

        service
            .attend(
                Participant::human("humanseat", "You"),
                AttentionMode::Selection,
                SemanticTargetRef::new("atlas", "one"),
                None,
            )
            .expect("the human selects");
        assert_eq!(service.human_attention_revision(), start + 1);

        service
            .clear(Participant::human("humanseat", "You"))
            .expect("the human clears");
        assert_eq!(
            service.human_attention_revision(),
            start + 2,
            "clearing is a change a waiting agent needs to see"
        );
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
    fn deliberate_departure_removes_presence_without_waiting_for_a_lease() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(10)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let agent = Participant::agent("agent-seat", "agentseat");
        service.present(agent.clone()).expect("agent is present");
        assert_eq!(service.snapshot().participants.len(), 1);

        let departed = service.depart(agent).expect("agent departs");
        assert!(departed.participants.is_empty());
        assert!(service.snapshot().participants.is_empty());
    }

    /// F6, the other half of one chip per person: closing a tab is not a
    /// deliberate departure (nothing calls `depart`, the browser just stops
    /// sending its 15-20s presence heartbeat), so the chip has to fall off on
    /// its own. The stated bound is [`PRESENCE_LEASE_MS`]: 45 seconds.
    ///
    /// Two connections of the same person (two tabs, sharing one resume
    /// cookie once F6's server-side identity fix is in) both check in as the
    /// SAME participant id, so this also proves the dedup holds even while
    /// one of the two tabs is still open: a closed tab's absence must not
    /// remove a person who has another connection still present.
    #[test]
    fn a_closed_tabs_presence_lease_drops_the_chip_within_the_stated_bound() {
        let now = Arc::new(AtomicU64::new(0));
        let service = fixture_service(now.clone(), Arc::new(Mutex::new(Vec::new())));
        let person = Participant::human("person-1", "Mike");

        // Two tabs, same person: both check in at t=0.
        service.present(person.clone()).expect("tab one checks in");
        service.present(person.clone()).expect("tab two checks in");
        assert_eq!(
            service.snapshot().participants.len(),
            1,
            "two tabs of one person are one chip while both are open"
        );

        // Tab one closes silently (no depart). Tab two keeps heartbeating on
        // its own 20s cadence; because both connections share one participant
        // id, tab two's heartbeat renews the ONE record either tab could have
        // written.
        for beat in 1..=3u64 {
            now.store(beat * 20_000, Ordering::Relaxed);
            service.present(person.clone()).expect("tab two heartbeat");
        }
        assert_eq!(
            service.snapshot().participants.len(),
            1,
            "the still-open tab keeps the chip present past the closed tab's own lease"
        );

        // Now the last tab closes too: nothing checks in again. The chip must
        // still be there for up to PRESENCE_LEASE_MS (45s) past the final
        // heartbeat, and gone once that bound passes.
        let last_heartbeat = 3 * 20_000;
        now.store(last_heartbeat + PRESENCE_LEASE_MS - 1, Ordering::Relaxed);
        assert_eq!(
            service.snapshot().participants.len(),
            1,
            "still inside the {PRESENCE_LEASE_MS}ms bound"
        );
        now.store(last_heartbeat + PRESENCE_LEASE_MS + 1, Ordering::Relaxed);
        assert!(
            service.snapshot().participants.is_empty(),
            "past the {PRESENCE_LEASE_MS}ms bound, a closed tab's chip must be gone"
        );
    }

    /// The measured red assertion: a marquee over four cards was gone ninety
    /// seconds later with zero interaction, because a thirty-second pointer
    /// lease was the store for a held selection. The browser heartbeat is
    /// every twenty seconds, so three of them span the old lease; the
    /// selection has to be alive after all three.
    #[test]
    fn a_held_selection_survives_while_the_browser_holding_it_is_present() {
        let now = Arc::new(AtomicU64::new(10));
        let service = fixture_service(now.clone(), Arc::new(Mutex::new(Vec::new())));
        let human = Participant::human("human-seat", "You");
        service
            .attend_many(
                human.clone(),
                AttentionMode::Selection,
                vec![
                    SemanticTargetRef::new("atlas", "one"),
                    SemanticTargetRef::new("atlas", "two"),
                    SemanticTargetRef::new("atlas", "three"),
                    SemanticTargetRef::new("atlas", "four"),
                ],
                None,
            )
            .expect("a marquee over four");

        // 20s, 40s, 60s, 80s, 100s: the same beat the browser actually sends.
        for beat in 1..=5u64 {
            now.store(10 + beat * 20_000, Ordering::Relaxed);
            service.present(human.clone()).expect("heartbeat");
        }
        let held = service
            .latest_active_matching(ParticipantKind::Human, AttentionMode::Selection, "atlas")
            .expect("the selection is still held at 100 seconds");
        assert_eq!(
            held.count(),
            4,
            "the whole set survives, not just the first"
        );
        assert_eq!(
            held.targets()
                .into_iter()
                .map(|target| target.target.target_id.clone())
                .collect::<Vec<_>>(),
            vec!["one", "two", "three", "four"],
            "renewal must not reorder or truncate the set the human built"
        );
        let snapshot = service.snapshot();
        assert_eq!(snapshot.participants[0].status, "active");
    }

    /// The other half of the rule, and the reason this is not just a longer
    /// lease: an agent's pointer stays brief. Nothing renews it, and a human
    /// browser sitting in the room does not keep it alive either.
    #[test]
    fn a_pointer_still_lapses_while_its_participant_stays_present() {
        let now = Arc::new(AtomicU64::new(10));
        let service = fixture_service(now.clone(), Arc::new(Mutex::new(Vec::new())));
        let agent = Participant::agent("agent-seat", "agentseat");
        service
            .attend(
                agent.clone(),
                AttentionMode::Pointer,
                SemanticTargetRef::new("atlas", "one"),
                None,
            )
            .expect("a pointer");
        for beat in 1..=3u64 {
            now.store(10 + beat * 20_000, Ordering::Relaxed);
            service.present(agent.clone()).expect("heartbeat");
        }
        assert!(
            service.latest_active(ParticipantKind::Agent).is_none(),
            "a pointer is brief on purpose and presence must not renew it"
        );
        assert_eq!(
            service.snapshot().participants[0].status,
            "present",
            "the agent is still in the room, just not pointing"
        );
    }

    /// Nothing renews a selection whose browser went away, which is what
    /// keeps this from being an unbounded lease: a closed tab stops sending
    /// presence and the set lapses on the same thirty seconds as before.
    #[test]
    fn a_held_selection_lapses_once_its_browser_stops_reporting_presence() {
        let now = Arc::new(AtomicU64::new(10));
        let service = fixture_service(now.clone(), Arc::new(Mutex::new(Vec::new())));
        let human = Participant::human("human-seat", "You");
        service
            .attend(
                human.clone(),
                AttentionMode::Selection,
                SemanticTargetRef::new("atlas", "one"),
                None,
            )
            .expect("a selection");
        now.store(10 + 20_000, Ordering::Relaxed);
        service.present(human).expect("one last heartbeat");
        now.store(10 + 20_000 + PRESENCE_LEASE_MS + 1, Ordering::Relaxed);
        assert!(
            service
                .latest_active_matching(ParticipantKind::Human, AttentionMode::Selection, "atlas")
                .is_none(),
            "a selection nobody is holding any more must not outlive its holder"
        );
        assert!(
            service.snapshot().participants.is_empty(),
            "and the holder is gone from the room too"
        );
    }

    #[test]
    fn a_newer_pointer_does_not_hide_a_live_selection_when_selection_is_requested() {
        let service = fixture_service(
            Arc::new(AtomicU64::new(10)),
            Arc::new(Mutex::new(Vec::new())),
        );
        service
            .attend(
                Participant::human("human-selection", "Selecting human"),
                AttentionMode::Selection,
                SemanticTargetRef::new("atlas", "selected-node"),
                None,
            )
            .expect("atlas selection");
        service
            .attend(
                Participant::human("human-pointer", "Pointing human"),
                AttentionMode::Pointer,
                SemanticTargetRef::new("board", "newer-pointer"),
                None,
            )
            .expect("newer pointer");

        let selection = service.latest_active_matching(
            ParticipantKind::Human,
            AttentionMode::Selection,
            "atlas",
        );
        assert_eq!(
            selection.map(|attention| attention.target.target.target_id),
            Some("selected-node".to_string()),
            "a newer pointer must not blank a still-live Atlas selection"
        );
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
    fn semantic_target_point_reuses_the_calling_mcp_participant() {
        let service = Arc::new(fixture_service(
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        ));
        service
            .present(Participant::human("human-mike", "Mike"))
            .expect("human present");
        service
            .present(Participant::agent("agent-agentseat", "agentseat"))
            .expect("attached MCP agent present");
        service.note_caller(&crate::Actor {
            caller: crate::Caller::Agent,
            label: Some("agentseat".to_string()),
            participant_id: Some("agent-agentseat".to_string()),
            hue: None,
            responsible: None,
        });

        let point = host_actions(&service)
            .into_iter()
            .find(|action| action.name == POINT_ACTION)
            .expect("semantic_target_point action");
        let Effect::Mutate(apply) = (point.apply)(&json!({
            "extension_id": "atlas",
            "target_id": "node-1"
        })) else {
            panic!("semantic_target_point should mutate ephemeral awareness");
        };
        apply(&NoopState).expect("point accepted");

        let snapshot = service.snapshot();
        assert_eq!(snapshot.participants.len(), 2, "{snapshot:?}");
        let pointing = snapshot
            .participants
            .iter()
            .find(|entry| entry.active && entry.attention.is_some())
            .expect("one active pointer");
        assert_eq!(pointing.participant.id, "agent-agentseat");
        assert_eq!(pointing.participant.label, "agentseat");
    }

    #[test]
    fn deleted_target_is_not_returned_as_current_pointing_context() {
        let exists = Arc::new(AtomicBool::new(true));
        let resolver_exists = exists.clone();
        let service = SemanticTargetService::new_with_clock(
            ["atlas".to_string()],
            Arc::new(move |target| {
                Ok(resolver_exists
                    .load(Ordering::Relaxed)
                    .then(|| SemanticTarget::new(target.clone(), "live node", "a live atlas node")))
            }),
            Arc::new(|_| {}),
            Arc::new(|| 10),
        )
        .expect("service");
        service
            .attend(
                Participant::agent("agent-agentseat", "agentseat"),
                AttentionMode::Pointer,
                SemanticTargetRef::new("atlas", "node-1"),
                None,
            )
            .expect("point at live target");

        exists.store(false, Ordering::Relaxed);
        assert!(
            service.latest_active(ParticipantKind::Agent).is_none(),
            "a later act must not attach dead pointing context"
        );
        let read_back = service.describe();
        assert!(!read_back.contains("node-1"), "{read_back}");
        assert!(
            read_back.contains("agentseat (Agent) is present"),
            "{read_back}"
        );
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
