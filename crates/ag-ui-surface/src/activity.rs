//! Host-owned operational activity.
//!
//! The browser is a consumer of this feed, never its witness. Records are
//! stamped inside the runtime from the already-resolved [`Caller`]. Persistence
//! is attempted before broadcast; any failure marks durability degraded rather
//! than pretending the record is restart-safe. Persisted records replay to
//! subscribers that arrive after the event, including after a host restart.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ag_ui_core::JsonValue;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{
    ActionRouteDef, Caller, ClientModule, RouteDef, SemanticTarget, SemanticTargetRef,
    SnapshotPngFuture, StateBacking, StateSnapshot, Surface, SurfaceState, SurfaceStore, ToolDef,
};

pub const SCHEMA_VERSION: u32 = 1;
pub const EVENT_NAME: &str = "surface.activity";
const MAX_EVENTS: usize = 800;
const MAX_DETAIL_CHARS: usize = 2_000;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The normalized classes every operational view consumes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityCategory {
    Lifecycle,
    Message,
    Tool,
    Action,
    Connection,
    Provider,
    Failure,
}

impl ActivityCategory {
    pub const ALL: [Self; 7] = [
        Self::Lifecycle,
        Self::Message,
        Self::Tool,
        Self::Action,
        Self::Connection,
        Self::Provider,
        Self::Failure,
    ];
}

/// What the host observed as the outcome of one occurrence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityOutcome {
    Info,
    Started,
    Succeeded,
    Failed,
    Cancelled,
    Connected,
    Disconnected,
}

/// Human and agent actors deliberately use one participant shape.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityActorKind {
    Human,
    Agent,
    Host,
    Unknown,
}

/// Host-stamped identity attached to an activity record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityActor {
    pub id: String,
    pub kind: ActivityActorKind,
    pub label: String,
}

impl ActivityActor {
    fn from_caller(caller: Caller) -> Self {
        match caller {
            Caller::Human => Self {
                id: "surface-human".to_string(),
                kind: ActivityActorKind::Human,
                label: "Human".to_string(),
            },
            Caller::Agent => Self {
                id: "surface-agent".to_string(),
                kind: ActivityActorKind::Agent,
                label: "Agent".to_string(),
            },
            Caller::Companion => Self {
                id: "companion-agent".to_string(),
                kind: ActivityActorKind::Agent,
                label: "Companion agent".to_string(),
            },
            Caller::Unknown => Self {
                id: "unknown-caller".to_string(),
                kind: ActivityActorKind::Unknown,
                label: "Unknown caller".to_string(),
            },
        }
    }

    fn host() -> Self {
        Self {
            id: "surface-host".to_string(),
            kind: ActivityActorKind::Host,
            label: "Host".to_string(),
        }
    }
}

/// The process that directly witnessed and stamped the event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityObserver {
    pub id: String,
    pub kind: ActivityActorKind,
    pub label: String,
}

/// Host-generated linkage. Emitters do not supply actor or observer fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityCorrelation {
    pub event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// One extension/state revision sampled by the host around an event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStateRevision {
    pub scope: String,
    pub value: String,
}

impl ActivityStateRevision {
    pub fn new(scope: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            value: value.into(),
        }
    }

    pub(crate) fn namespaced(mut self, namespace: &str) -> Self {
        self.scope = format!("{namespace}/{}", self.scope);
        self
    }
}

/// One normalized, host-stamped occurrence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub sequence: u64,
    pub at_ms: u64,
    pub category: ActivityCategory,
    pub kind: String,
    pub actor: ActivityActor,
    pub observer: ActivityObserver,
    pub correlation: ActivityCorrelation,
    pub outcome: ActivityOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_revision_before: Vec<ActivityStateRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_revision_after: Vec<ActivityStateRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// How completely the current runtime instrumentation covers a category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentationCoverage {
    Covered,
    Partial,
}

/// Whether this retained journal contains evidence for the category.
///
/// `Uncovered` is deliberately not serialized as an empty event list. It says
/// that this observation window has no affirmative evidence, so a diagnostic
/// cannot turn silence into "nothing happened."
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCoverage {
    Observed,
    Uncovered,
}

/// Explicit per-category coverage carried beside every snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityCoverage {
    pub category: ActivityCategory,
    pub instrumentation: InstrumentationCoverage,
    pub observation: ObservationCoverage,
    pub observed_events: usize,
    pub source: &'static str,
    pub note: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityDurabilityStatus {
    Available,
    Degraded,
    ProcessOnly,
}

/// Durability and provenance boundary for a snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDurability {
    pub status: ActivityDurabilityStatus,
    pub journal_id: String,
    pub observer_id: String,
    pub independent_of_open_tabs: bool,
    pub replays_after_restart: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Read model for Operational Trace and derived diagnostics.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySnapshot {
    pub schema_version: u32,
    pub durability: ActivityDurability,
    pub coverage: Vec<ActivityCoverage>,
    pub events: Vec<ActivityEvent>,
}

/// Replay first, then follow live events. A record racing subscription can be
/// present in both halves; consumers deduplicate by monotonic `sequence`.
pub struct ActivitySubscription {
    pub replay: Vec<ActivityEvent>,
    pub receiver: broadcast::Receiver<ActivityEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActivityError {
    #[error("activity journal {operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("activity journal contains invalid JSON at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("activity journal serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("activity journal is invalid: {0}")]
    Invalid(String),
    #[error("system clock is before the Unix epoch")]
    InvalidSystemTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedActivity {
    schema_version: u32,
    journal_id: String,
    next_sequence: u64,
    events: Vec<ActivityEvent>,
}

impl PersistedActivity {
    fn new() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            journal_id: format!("activity-{}", uuid::Uuid::new_v4().simple()),
            next_sequence: 1,
            events: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), ActivityError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ActivityError::Invalid(format!(
                "schema version {} is unsupported; expected {SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if self.journal_id.trim().is_empty() {
            return Err(ActivityError::Invalid(
                "journal id must not be empty".to_string(),
            ));
        }
        if self.events.len() > MAX_EVENTS {
            return Err(ActivityError::Invalid(format!(
                "journal retains {} events; maximum is {MAX_EVENTS}",
                self.events.len()
            )));
        }
        let mut previous = 0;
        for event in &self.events {
            if event.sequence <= previous {
                return Err(ActivityError::Invalid(
                    "event sequences must be strictly increasing".to_string(),
                ));
            }
            previous = event.sequence;
        }
        if self.next_sequence <= previous {
            return Err(ActivityError::Invalid(
                "next sequence must exceed retained events".to_string(),
            ));
        }
        Ok(())
    }
}

enum ActivityStorage {
    File(PathBuf),
    Memory,
}

/// Host-owned persistent journal and live fan-out.
pub struct ActivityFeed {
    inner: Mutex<PersistedActivity>,
    storage: ActivityStorage,
    observer_id: String,
    publish: broadcast::Sender<ActivityEvent>,
    persistence_error: Mutex<Option<String>>,
}

impl ActivityFeed {
    /// Open a durable journal. Malformed or unwritable state fails startup.
    pub fn open(path: impl Into<PathBuf>) -> Result<Arc<Self>, ActivityError> {
        let path = path.into();
        harden_existing_permissions(&path)?;
        let persisted = match std::fs::read(&path) {
            Ok(bytes) => {
                let parsed =
                    serde_json::from_slice::<PersistedActivity>(&bytes).map_err(|source| {
                        ActivityError::Parse {
                            path: path.clone(),
                            source,
                        }
                    })?;
                parsed.validate()?;
                parsed
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                let fresh = PersistedActivity::new();
                persist(&path, &fresh)?;
                fresh
            }
            Err(source) => {
                return Err(ActivityError::Io {
                    operation: "read",
                    path,
                    source,
                });
            }
        };
        let (publish, _) = broadcast::channel(512);
        Ok(Arc::new(Self {
            inner: Mutex::new(persisted),
            storage: ActivityStorage::File(path),
            observer_id: format!("host-{}", uuid::Uuid::new_v4().simple()),
            publish,
            persistence_error: Mutex::new(None),
        }))
    }

    /// Process-only feed for low-level runtime tests. Served applications use
    /// [`Self::open`] and therefore never claim restart durability from this.
    pub(crate) fn memory() -> Arc<Self> {
        let (publish, _) = broadcast::channel(64);
        Arc::new(Self {
            inner: Mutex::new(PersistedActivity::new()),
            storage: ActivityStorage::Memory,
            observer_id: format!("host-{}", uuid::Uuid::new_v4().simple()),
            publish,
            persistence_error: Mutex::new(None),
        })
    }

    pub fn snapshot(&self) -> ActivitySnapshot {
        let persisted = self.inner.lock().clone();
        let persistence_error = self.persistence_error.lock().clone();
        let status = match (&self.storage, persistence_error.as_ref()) {
            (ActivityStorage::Memory, _) => ActivityDurabilityStatus::ProcessOnly,
            (ActivityStorage::File(_), Some(_)) => ActivityDurabilityStatus::Degraded,
            (ActivityStorage::File(_), None) => ActivityDurabilityStatus::Available,
        };
        ActivitySnapshot {
            schema_version: SCHEMA_VERSION,
            durability: ActivityDurability {
                status,
                journal_id: persisted.journal_id,
                observer_id: self.observer_id.clone(),
                independent_of_open_tabs: true,
                replays_after_restart: matches!(self.storage, ActivityStorage::File(_)),
                error: persistence_error,
            },
            coverage: coverage_for(&persisted.events),
            events: persisted.events,
        }
    }

    pub fn subscribe(&self, after_sequence: Option<u64>) -> ActivitySubscription {
        let receiver = self.publish.subscribe();
        let replay = self
            .inner
            .lock()
            .events
            .iter()
            .filter(|event| after_sequence.is_none_or(|after| event.sequence > after))
            .cloned()
            .collect();
        ActivitySubscription { replay, receiver }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_action(
        &self,
        caller: Caller,
        name: &str,
        outcome: ActivityOutcome,
        run_id: Option<&str>,
        before: Vec<ActivityStateRevision>,
        after: Vec<ActivityStateRevision>,
        detail: Option<&str>,
    ) {
        self.append(
            ActivityCategory::Action,
            format!("action.{name}"),
            ActivityActor::from_caller(caller),
            outcome,
            correlation(run_id, None, None),
            before,
            after,
            detail,
        );
    }

    pub(crate) fn record_lifecycle(
        &self,
        kind: &str,
        outcome: ActivityOutcome,
        run_id: Option<&str>,
        detail: Option<&str>,
    ) {
        self.append(
            ActivityCategory::Lifecycle,
            kind.to_string(),
            ActivityActor::host(),
            outcome,
            correlation(run_id, None, None),
            Vec::new(),
            Vec::new(),
            detail,
        );
    }

    pub(crate) fn record_message(
        &self,
        caller: Caller,
        kind: &str,
        outcome: ActivityOutcome,
        run_id: Option<&str>,
        message_id: Option<&str>,
        detail: Option<&str>,
    ) {
        self.append(
            ActivityCategory::Message,
            kind.to_string(),
            ActivityActor::from_caller(caller),
            outcome,
            correlation(run_id, message_id, None),
            Vec::new(),
            Vec::new(),
            detail,
        );
    }

    pub(crate) fn record_host_message(
        &self,
        kind: &str,
        outcome: ActivityOutcome,
        run_id: Option<&str>,
        message_id: Option<&str>,
        detail: Option<&str>,
    ) {
        self.append(
            ActivityCategory::Message,
            kind.to_string(),
            ActivityActor::host(),
            outcome,
            correlation(run_id, message_id, None),
            Vec::new(),
            Vec::new(),
            detail,
        );
    }

    pub(crate) fn record_tool(
        &self,
        kind: &str,
        outcome: ActivityOutcome,
        run_id: Option<&str>,
        tool_call_id: &str,
        detail: Option<&str>,
    ) {
        self.append(
            ActivityCategory::Tool,
            kind.to_string(),
            ActivityActor::from_caller(Caller::Agent),
            outcome,
            correlation(run_id, None, Some(tool_call_id)),
            Vec::new(),
            Vec::new(),
            detail,
        );
    }

    pub(crate) fn record_connection(
        &self,
        kind: &str,
        outcome: ActivityOutcome,
        detail: Option<&str>,
    ) {
        self.append(
            ActivityCategory::Connection,
            kind.to_string(),
            ActivityActor::host(),
            outcome,
            correlation(None, None, None),
            Vec::new(),
            Vec::new(),
            detail,
        );
    }

    pub(crate) fn record_provider(
        &self,
        kind: &str,
        outcome: ActivityOutcome,
        run_id: Option<&str>,
        provider_id: &str,
    ) {
        self.append(
            ActivityCategory::Provider,
            kind.to_string(),
            ActivityActor::host(),
            outcome,
            correlation(run_id, None, None),
            Vec::new(),
            Vec::new(),
            Some(provider_id),
        );
    }

    pub(crate) fn record_failure(
        &self,
        kind: &str,
        caller: Option<Caller>,
        run_id: Option<&str>,
        detail: &str,
    ) {
        self.append(
            ActivityCategory::Failure,
            kind.to_string(),
            caller.map_or_else(ActivityActor::host, ActivityActor::from_caller),
            ActivityOutcome::Failed,
            correlation(run_id, None, None),
            Vec::new(),
            Vec::new(),
            Some(detail),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn append(
        &self,
        category: ActivityCategory,
        kind: String,
        actor: ActivityActor,
        outcome: ActivityOutcome,
        correlation: ActivityCorrelation,
        before: Vec<ActivityStateRevision>,
        after: Vec<ActivityStateRevision>,
        detail: Option<&str>,
    ) {
        let at_ms = match now_ms() {
            Ok(at_ms) => at_ms,
            Err(error) => {
                tracing::error!(%error, "activity event was not recorded");
                return;
            }
        };
        let detail = detail.map(normalize_detail);
        let (event, persistence) = {
            let mut persisted = self.inner.lock();
            let sequence = persisted.next_sequence;
            let Some(next_sequence) = sequence.checked_add(1) else {
                tracing::error!("activity event sequence exhausted");
                return;
            };
            persisted.next_sequence = next_sequence;
            let event = ActivityEvent {
                sequence,
                at_ms,
                category,
                kind,
                actor,
                observer: ActivityObserver {
                    id: self.observer_id.clone(),
                    kind: ActivityActorKind::Host,
                    label: "Host".to_string(),
                },
                correlation,
                outcome,
                state_revision_before: before,
                state_revision_after: after,
                detail,
            };
            persisted.events.push(event.clone());
            if persisted.events.len() > MAX_EVENTS {
                let remove = persisted.events.len() - MAX_EVENTS;
                persisted.events.drain(0..remove);
            }
            let result = match &self.storage {
                ActivityStorage::File(path) => persist(path, &persisted),
                ActivityStorage::Memory => Ok(()),
            };
            (event, result)
        };
        match persistence {
            Ok(()) => *self.persistence_error.lock() = None,
            Err(error) => {
                tracing::error!(%error, "activity journal persistence failed");
                *self.persistence_error.lock() = Some(error.to_string());
            }
        }
        let _ = self.publish.send(event);
    }
}

fn correlation(
    run_id: Option<&str>,
    message_id: Option<&str>,
    tool_call_id: Option<&str>,
) -> ActivityCorrelation {
    ActivityCorrelation {
        event_id: format!("event-{}", uuid::Uuid::new_v4().simple()),
        run_id: run_id.map(str::to_string),
        message_id: message_id.map(str::to_string),
        tool_call_id: tool_call_id.map(str::to_string),
    }
}

fn normalize_detail(detail: &str) -> String {
    let detail = detail.trim();
    if detail.chars().count() <= MAX_DETAIL_CHARS {
        return detail.to_string();
    }
    let mut truncated = detail.chars().take(MAX_DETAIL_CHARS).collect::<String>();
    truncated.push('…');
    truncated
}

fn coverage_for(events: &[ActivityEvent]) -> Vec<ActivityCoverage> {
    let mut counts = BTreeMap::new();
    for event in events {
        *counts.entry(event.category).or_insert(0usize) += 1;
    }
    ActivityCategory::ALL
        .into_iter()
        .map(|category| {
            let observed_events = counts.get(&category).copied().unwrap_or(0);
            let (instrumentation, source, note) = instrumentation(category);
            ActivityCoverage {
                category,
                instrumentation,
                observation: if observed_events == 0 {
                    ObservationCoverage::Uncovered
                } else {
                    ObservationCoverage::Observed
                },
                observed_events,
                source,
                note,
            }
        })
        .collect()
}

fn instrumentation(
    category: ActivityCategory,
) -> (InstrumentationCoverage, &'static str, &'static str) {
    match category {
        ActivityCategory::Lifecycle => (
            InstrumentationCoverage::Covered,
            "runtime tutor lifecycle boundary",
            "Host lifecycle transitions are recorded independently of browser delivery.",
        ),
        ActivityCategory::Message => (
            InstrumentationCoverage::Covered,
            "runtime transcript and narration boundary",
            "Human submissions and host-authorized assistant message completion are observed.",
        ),
        ActivityCategory::Tool => (
            InstrumentationCoverage::Covered,
            "provider-independent tool lifecycle boundary",
            "All adapters project tool start, argument completion, and result through one host seam.",
        ),
        ActivityCategory::Action => (
            InstrumentationCoverage::Covered,
            "shared typed action dispatcher",
            "Every dispatcher outcome is stamped from its resolved Caller before returning.",
        ),
        ActivityCategory::Connection => (
            InstrumentationCoverage::Partial,
            "runtime SSE and WebSocket handlers",
            "Host connection establishment is observed; abrupt HTTP disconnect timing is best effort.",
        ),
        ActivityCategory::Provider => (
            InstrumentationCoverage::Partial,
            "provider supervisor",
            "Selection, startup, adoption, readiness, and terminal supervisor outcomes are observed.",
        ),
        ActivityCategory::Failure => (
            InstrumentationCoverage::Partial,
            "runtime dispatcher, turn, and provider failure boundaries",
            "Runtime failures are observed; extension-private background failures need their own host adapter.",
        ),
    }
}

fn now_ms() -> Result<u64, ActivityError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ActivityError::InvalidSystemTime)?
        .as_millis();
    u64::try_from(millis).map_err(|_| ActivityError::InvalidSystemTime)
}

fn persist(path: &Path, persisted: &PersistedActivity) -> Result<(), ActivityError> {
    let bytes = serde_json::to_vec_pretty(persisted).map_err(ActivityError::Serialize)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|source| ActivityError::Io {
            operation: "create parent directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_path = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp_path)
            .map_err(|source| ActivityError::Io {
                operation: "create temporary file",
                path: temp_path.clone(),
                source,
            })?;
        file.write_all(&bytes).map_err(|source| ActivityError::Io {
            operation: "write temporary file",
            path: temp_path.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| ActivityError::Io {
            operation: "sync temporary file",
            path: temp_path.clone(),
            source,
        })?;
        std::fs::rename(&temp_path, path).map_err(|source| ActivityError::Io {
            operation: "replace",
            path: path.to_path_buf(),
            source,
        })?;
        harden_existing_permissions(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

#[cfg(unix)]
fn harden_existing_permissions(path: &Path) -> Result<(), ActivityError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ActivityError::Io {
                operation: "inspect permissions",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut permissions = metadata.permissions();
    if permissions.mode() & 0o077 != 0 {
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(|source| ActivityError::Io {
            operation: "harden permissions",
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_existing_permissions(_path: &Path) -> Result<(), ActivityError> {
    Ok(())
}

struct HostedState {
    inner: Arc<dyn Surface>,
    feed: Arc<ActivityFeed>,
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

    fn activity_state_revision(&self) -> Result<Vec<ActivityStateRevision>, String> {
        self.inner.state().activity_state_revision()
    }

    fn activity_feed(&self) -> Option<&ActivityFeed> {
        Some(self.feed.as_ref())
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

    fn semantic_targets(&self) -> Option<&crate::SemanticTargetService> {
        self.inner.state().semantic_targets()
    }

    fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
        self.inner.state().ws_hello()
    }

    fn ws_receive(&self, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.inner.state().ws_receive(data)
    }

    fn reconnect_events(&self) -> Vec<(String, JsonValue)> {
        self.inner.state().reconnect_events()
    }
}

struct HostedSurface {
    inner: Arc<dyn Surface>,
    state: HostedState,
}

impl Surface for HostedSurface {
    fn state(&self) -> &dyn SurfaceState {
        &self.state
    }

    fn tools(&self) -> &[ToolDef] {
        self.inner.tools()
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

pub(crate) fn install(surface: Arc<dyn Surface>, feed: Arc<ActivityFeed>) -> Arc<dyn Surface> {
    Arc::new(HostedSurface {
        state: HostedState {
            inner: surface.clone(),
            feed,
        },
        inner: surface,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ag-ui-activity-{name}-{}-{}.json",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn caller_stamps_actor_and_ignores_emitter_vocabulary() {
        let feed = ActivityFeed::memory();
        feed.record_action(
            Caller::Companion,
            "claim_actor_human",
            ActivityOutcome::Succeeded,
            None,
            Vec::new(),
            Vec::new(),
            None,
        );
        let event = feed.snapshot().events.pop().expect("one activity event");
        assert_eq!(event.actor.kind, ActivityActorKind::Agent);
        assert_eq!(event.actor.id, "companion-agent");
        assert_eq!(event.observer.kind, ActivityActorKind::Host);
    }

    #[test]
    fn every_category_has_explicit_coverage_even_when_empty() {
        let snapshot = ActivityFeed::memory().snapshot();
        assert_eq!(snapshot.coverage.len(), ActivityCategory::ALL.len());
        assert!(snapshot.events.is_empty());
        assert!(snapshot.coverage.iter().all(|coverage| {
            coverage.observation == ObservationCoverage::Uncovered && coverage.observed_events == 0
        }));
    }

    #[test]
    fn durable_feed_replays_after_reopen_without_a_subscriber() {
        let path = temp_path("restart");
        let feed = ActivityFeed::open(path.clone()).expect("open activity journal");
        feed.record_action(
            Caller::Agent,
            "persisted_action",
            ActivityOutcome::Succeeded,
            Some("run-1"),
            Vec::new(),
            Vec::new(),
            None,
        );
        drop(feed);

        let reopened = ActivityFeed::open(path.clone()).expect("reopen activity journal");
        let subscription = reopened.subscribe(Some(0));
        assert_eq!(subscription.replay.len(), 1);
        assert_eq!(subscription.replay[0].kind, "action.persisted_action");
        assert!(reopened.snapshot().durability.replays_after_restart);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corrupt_journal_fails_loudly() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"{not-json").expect("write corrupt fixture");
        let error = ActivityFeed::open(path.clone())
            .err()
            .expect("corrupt activity journal must fail");
        assert!(matches!(error, ActivityError::Parse { .. }));
        let _ = std::fs::remove_file(path);
    }
}
