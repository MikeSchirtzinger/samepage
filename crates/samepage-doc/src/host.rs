//! The host's authority over the shared document.
//!
//! The host holds the authoritative [`Document`]. Every change reaches it one
//! of two ways, and both end in the same place:
//!
//! - an action (`invoke`) or an untrusted [`Batch`] (`commit_batch`): the
//!   capability's plan is resolved here, against the live view, never
//!   accepted pre-resolved;
//! - a replica frame (`receive_frame`): a Yrs update from a browser, admitted
//!   only after the checks below.
//!
//! Either way the change and its authorship stamp are written in **one** Yrs
//! transaction on the live document, under one lock held from the baseline
//! read to publication, so no observer, broadcast or read-back ever sees the
//! change without its stamp. A refused change produces no transaction, no
//! journal entry, no callback and no broadcast.
//!
//! # What a replica frame must satisfy
//!
//! The frame is applied to a disposable candidate built from the live state.
//! It is admitted only if, in the candidate's **settled** state:
//!
//! 1. nothing is pending: an update whose dependencies are missing is held in a
//!    bounded per-actor quarantine outside the live document, and retried
//!    after every later commit and every later frame from that actor;
//! 2. every new insertion range belongs to a Yrs client id the sending
//!    participant registered (or may register now); host ids and ids present
//!    when the document was opened are reserved;
//! 3. state outside the layout (other roots, nested types, non-scalar values)
//!    is identical to the live document's, and every existing record keeps its
//!    branch identity (a map replaced under the same id is refused);
//! 4. no effective write reported by the candidate's deep observer touches a
//!    reserved key (authorship, `kind` of an existing record), a key no enabled
//!    capability claims, or a record of a kind nobody owns, even when the value
//!    written equals the old one;
//! 5. the visible change passes the C1 audit, every record it creates is named
//!    in the sender's own client namespace, and each involved capability's
//!    `validate` accepts it.
//!
//! What this does not claim: the deep observer summarizes a transaction, so a
//! write that lost a concurrent conflict, or one made inside a parent the same
//! update created and deleted, may not be reported. Such writes are settled
//! as deleted and have no effect; they are not "authorized", they are inert.
//! Yrs also trims ranges it already knows, so contradictory bytes for an
//! already-known client and clock are dropped unexamined; public Yrs APIs
//! cannot certify them, and that trimming is not authentication.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use samepage_capability::{
    audit, is_reserved_key, Audience, AuditError, Batch, BatchBuilder, Call, CapError, Capability,
    Change, Composition, Delta, Effect, Origin, PlanError, Principal, ResolvedOp, View,
    AUTHORSHIP_KEYS, KIND_KEY,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use yrs::types::{EntryChange, Event, PathSegment};
use yrs::updates::encoder::Encode;
use yrs::{Any, BranchID, DeepObservable, Map, Out, Transact, Update};

use crate::document::{self, decode, id_client, DocError, Document};
use crate::journal::{Entry, Journal};
use crate::layout;
use crate::sync::{self, Incoming, SyncError};
use crate::{Actor, LAYOUT_VERSION};

/// Per-actor quarantine bounds.
pub const MAX_HELD_UPDATES: usize = 32;
pub const MAX_HELD_BYTES: usize = 1 << 20;
/// How many actors may hold unresolved updates at once.
pub const MAX_HELD_ACTORS: usize = 64;
/// Cascade rounds before a delete chain is refused as runaway.
const MAX_CASCADE_ROUNDS: usize = 8;
/// Observer key for the per-frame touched-record collector.
const TOUCHED_OBSERVER: &str = "samepage-doc.touched";

/// One accepted change, as every observer sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub seq: u64,
    pub actor: Actor,
    pub action: Option<String>,
    pub revision: String,
    pub created: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    /// False for an accepted frame that changed no record (for example a write
    /// that lost a concurrent conflict). Still journaled and broadcast: later
    /// updates from that client depend on it.
    pub visible_change: bool,
    /// A y-sync Update frame carrying the change and its stamps. Broadcast it
    /// to every peer exactly once.
    #[serde(skip)]
    pub update: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameReply {
    /// Sync answers only (a SyncStep2 for each SyncStep1 in the frame). Never
    /// a commit: commits are announced through [`Host::on_commit`].
    pub replies: Vec<Vec<u8>>,
    pub outcome: FrameOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Only sync messages, or content the document already had.
    SyncOnly,
    Committed {
        seq: u64,
    },
    /// Unresolved: held for this actor, nothing applied.
    Held {
        pending_updates: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum FrameRefusal {
    #[error("sync: {0}")]
    Sync(#[from] SyncError),
    #[error("document: {0}")]
    Doc(#[from] DocError),
    #[error("client {client} is reserved or registered to another participant")]
    Identity { client: u64 },
    #[error("record {id:?} is created outside the sender's client namespace")]
    ForeignId { id: String },
    #[error("state outside the document layout changed at {0:?}")]
    HiddenStructure(Vec<String>),
    #[error("record {0:?} was replaced by a different map under the same id")]
    Replaced(String),
    #[error("write to {key:?} on {id:?} is not allowed: {reason}")]
    Write {
        id: String,
        key: String,
        reason: &'static str,
    },
    #[error("{0}")]
    Audit(AuditError),
    #[error("{capability} refused: {error}")]
    Validate { capability: String, error: CapError },
    #[error("{capability} refused: {error}")]
    Verify { capability: String, error: CapError },
    #[error("the quarantine for this actor is full ({updates} updates, {bytes} bytes)")]
    QuarantineFull { updates: usize, bytes: usize },
    #[error("too many actors hold unresolved updates")]
    TooManyHeld,
    #[error("participant {participant:?} has held updates recorded under a different caller or label; the held updates keep their original actor")]
    ActorChanged { participant: String },
    #[error("the live document has pending content after a commit; this is a kernel bug")]
    Internal,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CommitError {
    #[error("plan: {0}")]
    Plan(#[from] PlanError),
    #[error("{capability} refused: {error}")]
    Capability { capability: String, error: CapError },
    #[error("{capability} refused: {error}")]
    Validate { capability: String, error: CapError },
    #[error("{capability} refused: {error}")]
    Verify { capability: String, error: CapError },
    #[error("deletes kept cascading after {MAX_CASCADE_ROUNDS} rounds")]
    RunawayCascade,
    #[error("the batch changes nothing; no commit was made")]
    Empty,
    #[error("document: {0}")]
    Doc(#[from] DocError),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum InvokeError {
    #[error("no action named {0:?}")]
    UnknownAction(String),
    #[error("{caller} callers may not use {action:?}")]
    Audience {
        action: String,
        caller: &'static str,
    },
    #[error("{capability} refused: {error}")]
    Capability { capability: String, error: CapError },
    #[error("{action:?} is declared as a query but planned {ops} write(s)")]
    QueryMutates { action: String, ops: usize },
    #[error("capability {capability:?} planned an invalid batch for {action:?}: {reason}")]
    PlannerBatch {
        capability: String,
        action: String,
        reason: String,
    },
    #[error("{0}")]
    Commit(#[from] CommitError),
}

#[derive(Debug, Error)]
pub enum OpenError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a saved document: {reason}")]
    Format { path: PathBuf, reason: String },
    #[error("{path} holds content that cannot be integrated (missing dependencies); refusing to open it as authoritative")]
    Unresolved { path: PathBuf },
    #[error("document: {0}")]
    Doc(#[from] DocError),
    #[error("capability set: {0}")]
    Capabilities(String),
}

#[derive(Debug, Error)]
pub enum SaveError {
    #[error("this host has no file")]
    InMemory,
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The result of an action.
#[derive(Debug, Clone, PartialEq)]
pub struct Invoked {
    pub action: String,
    pub readback: Vec<String>,
    pub ids: BTreeMap<String, String>,
    pub commit: Option<Commit>,
}

/// Native facts for one capability's authority checks.
///
/// Registered with [`Host::with_verifier`]. The host calls it, under its lock,
/// for every change that involves the capability, and hands the result to that
/// capability only, as [`Change::facts`]. Facts come from what the verifier
/// computes from the change and host resources, never from action names or
/// arguments alone.
///
/// Contract: bounded and quick (no network, no G8, no long-running process),
/// read-only (no side effects: the change may still be refused), and never
/// calls back into the [`Host`] (it runs under the host lock).
pub trait Verifier: Send + Sync {
    fn verify(&self, change: &Change<'_>) -> Result<serde_json::Value, CapError>;
}

/// Why an authority check refused a change.
enum Refused {
    Validate(String, CapError),
    Verify(String, CapError),
}

impl From<Refused> for FrameRefusal {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::Validate(capability, error) => FrameRefusal::Validate { capability, error },
            Refused::Verify(capability, error) => FrameRefusal::Verify { capability, error },
        }
    }
}

impl From<Refused> for CommitError {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::Validate(capability, error) => CommitError::Validate { capability, error },
            Refused::Verify(capability, error) => CommitError::Verify { capability, error },
        }
    }
}

type Observer = Arc<dyn Fn(&Commit) + Send + Sync>;
type RefusalObserver = Arc<dyn Fn(&Actor, &FrameRefusal) + Send + Sync>;

#[derive(Debug, Clone)]
struct Held {
    actor: Actor,
    updates: Vec<Vec<u8>>,
}

struct Inner {
    live: Document,
    journal: Journal,
    /// Participant -> unresolved updates, in arrival order of first hold.
    held: Vec<(String, Held)>,
    /// Saves so far, stored in the envelope.
    generation: u64,
}

/// A trusted native composition adapter, called under the document lock after
/// the portable planner's dependency reach has been checked. Owners still audit
/// every resulting op, and the whole batch keeps the original preferred action.
pub trait BatchComposer: Send + Sync {
    fn compose(&self, call: &Call, view: &View, batch: Batch) -> Result<Batch, CapError>;
}

pub struct Host {
    inner: Mutex<Inner>,
    /// Serializes saves, so envelopes reach the disk in generation order.
    save_lock: Mutex<()>,
    composition: Composition,
    capabilities: BTreeMap<String, Arc<dyn Capability>>,
    verifiers: BTreeMap<String, Arc<dyn Verifier>>,
    composers: BTreeMap<String, Arc<dyn BatchComposer>>,
    plan_context: BTreeMap<String, Vec<String>>,
    path: Option<PathBuf>,
    observers: Mutex<Vec<Observer>>,
    refusal_observers: Mutex<Vec<RefusalObserver>>,
    /// Anomalies found when the document was opened, preserved untouched.
    opened_anomalies: Vec<String>,
}

impl Host {
    pub fn in_memory(
        composition: Composition,
        capabilities: Vec<Arc<dyn Capability>>,
    ) -> Result<Self, OpenError> {
        Self::build(None, None, Journal::default(), 0, composition, capabilities)
    }

    /// Open (or start) the document saved at `path`.
    ///
    /// The file is one JSON envelope (`format: "samepage.document.v1"`) holding
    /// the Yrs state and the journal together, so the attribution record can
    /// never be paired with a different state. A missing file starts an empty
    /// document. A file in any other format, including the Atlas host's bare
    /// base64 state, is refused: use [`Host::import_legacy`], which reads it
    /// without ever writing to it.
    pub fn open(
        path: impl AsRef<Path>,
        composition: Composition,
        capabilities: Vec<Arc<dyn Capability>>,
    ) -> Result<Self, OpenError> {
        let path = path.as_ref().to_path_buf();
        let (state, journal, generation) = match std::fs::read(&path) {
            Ok(bytes) => {
                let envelope: Envelope =
                    serde_json::from_slice(&bytes).map_err(|error| OpenError::Format {
                        path: path.clone(),
                        reason: format!(
                            "not a {ENVELOPE_FORMAT} envelope ({error}); a legacy state file must be imported with Host::import_legacy"
                        ),
                    })?;
                if envelope.format != ENVELOPE_FORMAT {
                    return Err(OpenError::Format {
                        path,
                        reason: format!("format {:?}, expected {ENVELOPE_FORMAT}", envelope.format),
                    });
                }
                let state = base64::engine::general_purpose::STANDARD
                    .decode(envelope.state.as_bytes())
                    .map_err(|error| OpenError::Format {
                        path: path.clone(),
                        reason: error.to_string(),
                    })?;
                (Some(state), envelope.journal, envelope.generation)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (None, Journal::default(), 0)
            }
            Err(source) => return Err(OpenError::Read { path, source }),
        };
        Self::build(
            Some(path),
            state,
            journal,
            generation,
            composition,
            capabilities,
        )
    }

    /// Import a legacy state file (base64 of a full Yrs v1 update, as the
    /// Atlas host writes it) into a new envelope at `path`. The legacy file is
    /// only read. `path` must not exist yet. The import has no attribution
    /// record, so every client id already in the state is reserved.
    pub fn import_legacy(
        legacy: impl AsRef<Path>,
        path: impl AsRef<Path>,
        composition: Composition,
        capabilities: Vec<Arc<dyn Capability>>,
    ) -> Result<Self, OpenError> {
        let legacy = legacy.as_ref().to_path_buf();
        let path = path.as_ref().to_path_buf();
        if path.exists() || path == legacy {
            return Err(OpenError::Format {
                path,
                reason: "the import target already exists; importing never overwrites".to_string(),
            });
        }
        let encoded = std::fs::read(&legacy).map_err(|source| OpenError::Read {
            path: legacy.clone(),
            source,
        })?;
        let text = String::from_utf8_lossy(&encoded);
        let state = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|error| OpenError::Format {
                path: legacy.clone(),
                reason: format!("not a base64 legacy state: {error}"),
            })?;
        Self::build(
            Some(path),
            Some(state),
            Journal::default(),
            0,
            composition,
            capabilities,
        )
    }

    fn build(
        path: Option<PathBuf>,
        state: Option<Vec<u8>>,
        mut journal: Journal,
        generation: u64,
        composition: Composition,
        capabilities: Vec<Arc<dyn Capability>>,
    ) -> Result<Self, OpenError> {
        let capabilities: BTreeMap<String, Arc<dyn Capability>> = capabilities
            .into_iter()
            .map(|capability| (capability.descriptor().id, capability))
            .collect();
        for descriptor in composition.descriptors() {
            let Some(capability) = capabilities.get(&descriptor.id) else {
                return Err(OpenError::Capabilities(format!(
                    "no implementation for composed capability {:?}",
                    descriptor.id
                )));
            };
            if &capability.descriptor() != descriptor {
                return Err(OpenError::Capabilities(format!(
                    "implementation of {:?} declares a different descriptor than the composition",
                    descriptor.id
                )));
            }
        }

        // Load into a scratch document first to learn which client ids exist.
        let loaded = match &state {
            Some(bytes) => Document::from_state(bytes)?,
            None => Document::new(),
        };
        if loaded.has_missing_updates() {
            return Err(OpenError::Unresolved {
                path: path.clone().unwrap_or_default(),
            });
        }
        let existing: BTreeSet<u64> = loaded
            .state_vector()
            .iter()
            .map(|(client, _)| client.get())
            .collect();
        for client in &existing {
            if !journal.bindings.contains_key(client) {
                journal.reserved.insert(*client);
            }
        }
        // A fresh host client id nobody has used.
        let host_client = loop {
            let candidate = yrs::Doc::new().client_id().get();
            if !existing.contains(&candidate)
                && !journal.reserved.contains(&candidate)
                && !journal.bindings.contains_key(&candidate)
            {
                break candidate;
            }
        };
        journal.reserved.insert(host_client);
        journal.layout_version = LAYOUT_VERSION;
        let live = Document::with_client(host_client);
        if let Some(bytes) = &state {
            live.apply_remote(decode(bytes)?)?;
        }
        let opened_anomalies = live.inspect().anomalies.into_keys().collect();
        Ok(Self {
            inner: Mutex::new(Inner {
                live,
                journal,
                held: Vec::new(),
                generation,
            }),
            save_lock: Mutex::new(()),
            composition,
            capabilities,
            verifiers: BTreeMap::new(),
            composers: BTreeMap::new(),
            plan_context: BTreeMap::new(),
            path,
            observers: Mutex::new(Vec::new()),
            refusal_observers: Mutex::new(Vec::new()),
            opened_anomalies,
        })
    }

    /// Register the native verifier for `capability` (see [`Verifier`]).
    pub fn with_verifier(
        mut self,
        capability: &str,
        verifier: Arc<dyn Verifier>,
    ) -> Result<Self, OpenError> {
        if self.composition.descriptor(capability).is_none() {
            return Err(OpenError::Capabilities(format!(
                "no composed capability {capability:?} to verify"
            )));
        }
        if self
            .verifiers
            .insert(capability.to_string(), verifier)
            .is_some()
        {
            return Err(OpenError::Capabilities(format!(
                "capability {capability:?} already has a verifier"
            )));
        }
        Ok(self)
    }

    /// Register an explicit native adapter for cross-capability composition.
    pub fn with_composer(
        mut self,
        capability: &str,
        composer: Arc<dyn BatchComposer>,
    ) -> Result<Self, OpenError> {
        if self.composition.descriptor(capability).is_none()
            || self.composers.contains_key(capability)
        {
            return Err(OpenError::Capabilities(format!(
                "invalid composer registration for {capability}"
            )));
        }
        self.composers.insert(capability.to_string(), composer);
        Ok(self)
    }

    /// Supply explicitly selected optional target kinds to an existing owner.
    /// This is native composition, not an implicit dependency or write grant.
    /// It enables mutual references (marks and constraint members on drawings)
    /// without introducing a cycle into portable capability dependencies.
    pub fn with_plan_context(
        mut self,
        capability: &str,
        kinds: &[&str],
    ) -> Result<Self, OpenError> {
        if self.composition.descriptor(capability).is_none()
            || kinds
                .iter()
                .any(|kind| self.composition.owner_of_kind(kind).is_none())
        {
            return Err(OpenError::Capabilities(format!(
                "unselected planning context for {capability}"
            )));
        }
        self.plan_context.insert(
            capability.into(),
            kinds.iter().map(|s| s.to_string()).collect(),
        );
        Ok(self)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn composition(&self) -> &Composition {
        &self.composition
    }

    pub fn view(&self) -> View {
        self.lock().live.view()
    }

    pub fn revision(&self) -> String {
        self.lock().live.revision()
    }

    /// Full v1 state of the live document (never contains pending content).
    pub fn encode_full(&self) -> Vec<u8> {
        self.lock().live.encode_full()
    }

    /// The host's own Yrs client id for this process (reserved).
    pub fn client_id(&self) -> u64 {
        self.lock().live.client_id()
    }

    pub fn journal(&self) -> Journal {
        self.lock().journal.clone()
    }

    /// Locations outside the layout found when the document was opened. They
    /// are preserved exactly and never shown to a capability.
    pub fn opened_anomalies(&self) -> &[String] {
        &self.opened_anomalies
    }

    /// Unresolved updates currently held, per participant.
    pub fn held(&self) -> BTreeMap<String, usize> {
        self.lock()
            .held
            .iter()
            .map(|(participant, held)| (participant.clone(), held.updates.len()))
            .collect()
    }

    pub fn on_commit(&self, observer: impl Fn(&Commit) + Send + Sync + 'static) {
        self.observers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(Arc::new(observer));
    }

    /// Called when a held payload turns out to be invalid once it resolves,
    /// so the transport can reset that actor's replica.
    pub fn on_refusal(&self, observer: impl Fn(&Actor, &FrameRefusal) + Send + Sync + 'static) {
        self.refusal_observers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(Arc::new(observer));
    }

    /// Frames for a new connection: our state vector (so the peer sends what
    /// we lack) and our full state.
    pub fn hello(&self) -> Vec<Vec<u8>> {
        let inner = self.lock();
        vec![
            sync::step1(&inner.live.state_vector()),
            sync::step2(inner.live.encode_full()),
        ]
    }

    /// Save state and journal as one envelope: written to a unique temporary
    /// file in the same directory, flushed, then renamed over the old one.
    /// Saves are serialized; each carries the next generation number. A crash
    /// leaves either the previous envelope or the new one, never a mix.
    pub fn save(&self) -> Result<u64, SaveError> {
        let path = self.path.as_ref().ok_or(SaveError::InMemory)?;
        let _serial = self
            .save_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let envelope = {
            let mut inner = self.lock();
            inner.generation += 1;
            Envelope {
                format: ENVELOPE_FORMAT.to_string(),
                layout_version: LAYOUT_VERSION,
                generation: inner.generation,
                state: base64::engine::general_purpose::STANDARD.encode(inner.live.encode_full()),
                journal: inner.journal.clone(),
            }
        };
        let bytes = serde_json::to_vec(&envelope).map_err(|error| SaveError::Write {
            path: path.clone(),
            source: std::io::Error::other(error),
        })?;
        write_atomically(path, &bytes)?;
        Ok(envelope.generation)
    }

    /// The generation of the last save (or of the envelope opened).
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    // ── replica frames ───────────────────────────────────────────────────

    pub fn receive_frame(&self, actor: &Actor, frame: &[u8]) -> Result<FrameReply, FrameRefusal> {
        let _span = samepage_capability::activity::Span::new("doc", "doc.sync.receive");
        let messages = sync::parse(frame)?;
        let mut commits = Vec::new();
        let mut late_refusals = Vec::new();
        let result = {
            let mut inner = self.lock();
            let mut replies = Vec::new();
            let mut updates = Vec::new();
            for message in messages {
                match message {
                    Incoming::Step1(sv) => replies.push(sync::step2(inner.live.encode_state(&sv))),
                    Incoming::Update(update) => updates.push(update),
                }
            }
            if updates.is_empty() {
                Ok(FrameReply {
                    replies,
                    outcome: FrameOutcome::SyncOnly,
                })
            } else {
                match self.admit(&mut inner, actor, updates) {
                    Ok(outcome) => {
                        if let Admitted::Committed(commit) = &outcome {
                            commits.push((**commit).clone());
                        }
                        if matches!(outcome, Admitted::Committed(_)) {
                            self.drain(&mut inner, &mut commits, &mut late_refusals);
                        }
                        Ok(FrameReply {
                            replies,
                            outcome: match outcome {
                                Admitted::Committed(commit) => {
                                    FrameOutcome::Committed { seq: commit.seq }
                                }
                                Admitted::Held(pending_updates) => {
                                    FrameOutcome::Held { pending_updates }
                                }
                                Admitted::Replay => FrameOutcome::SyncOnly,
                            },
                        })
                    }
                    Err(refusal) => Err(refusal),
                }
            }
        };
        self.announce(&commits, &late_refusals);
        result
    }

    /// Admit one actor's new updates, combined with anything it has held.
    fn admit(
        &self,
        inner: &mut Inner,
        actor: &Actor,
        new: Vec<Vec<u8>>,
    ) -> Result<Admitted, FrameRefusal> {
        for update in &new {
            decode(update)?;
        }
        // Held updates keep the actor they arrived with. A later frame from
        // the same participant under a different caller or label is refused
        // while they are held, rather than relabelling history.
        if let Some((_, held)) = inner
            .held
            .iter()
            .find(|(participant, _)| participant == &actor.participant)
            && &held.actor != actor {
                return Err(FrameRefusal::ActorChanged {
                    participant: actor.participant.clone(),
                });
            }
        let held = inner
            .held
            .iter()
            .find(|(participant, _)| participant == &actor.participant)
            .map(|(_, held)| held.updates.clone())
            .unwrap_or_default();

        let mut combined = held.clone();
        combined.extend(new.iter().cloned());
        match self.validate_and_commit(inner, actor, &combined)? {
            Settled::Committed(commit) => {
                remove_held(inner, &actor.participant);
                return Ok(Admitted::Committed(commit));
            }
            Settled::Replay => {
                remove_held(inner, &actor.participant);
                return Ok(Admitted::Replay);
            }
            Settled::Unresolved => {}
        }
        // The new updates may be independent of what is held (another tab of
        // the same participant). Try them alone before holding them.
        if !held.is_empty() {
            match self.validate_and_commit(inner, actor, &new)? {
                Settled::Committed(commit) => return Ok(Admitted::Committed(commit)),
                Settled::Replay => return Ok(Admitted::Replay),
                Settled::Unresolved => {}
            }
        }
        // Over the bound: this frame is refused and what was already held
        // stays exactly as it was (it may still resolve after a later commit).
        let bytes: usize = combined.iter().map(Vec::len).sum();
        if combined.len() > MAX_HELD_UPDATES || bytes > MAX_HELD_BYTES {
            return Err(FrameRefusal::QuarantineFull {
                updates: combined.len(),
                bytes,
            });
        }
        let pending_updates = combined.len();
        match inner
            .held
            .iter_mut()
            .find(|(participant, _)| participant == &actor.participant)
        {
            Some((_, slot)) => slot.updates = combined,
            None => {
                if inner.held.len() >= MAX_HELD_ACTORS {
                    return Err(FrameRefusal::TooManyHeld);
                }
                inner.held.push((
                    actor.participant.clone(),
                    Held {
                        actor: actor.clone(),
                        updates: combined,
                    },
                ))
            }
        }
        Ok(Admitted::Held(pending_updates))
    }

    /// Retry every held payload after a commit, as its original actor, until
    /// nothing more resolves.
    fn drain(
        &self,
        inner: &mut Inner,
        commits: &mut Vec<Commit>,
        refusals: &mut Vec<(Actor, FrameRefusal)>,
    ) {
        loop {
            let mut progressed = false;
            let pending: Vec<(String, Held)> = inner.held.clone();
            for (participant, held) in pending {
                match self.validate_and_commit(inner, &held.actor, &held.updates) {
                    Ok(Settled::Committed(commit)) => {
                        remove_held(inner, &participant);
                        commits.push(*commit);
                        progressed = true;
                    }
                    Ok(Settled::Replay) => {
                        remove_held(inner, &participant);
                        progressed = true;
                    }
                    Ok(Settled::Unresolved) => {}
                    Err(refusal) => {
                        remove_held(inner, &participant);
                        refusals.push((held.actor.clone(), refusal));
                        progressed = true;
                    }
                }
            }
            if !progressed {
                break;
            }
        }
    }

    /// The whole admission check on a candidate, and the commit if it passes.
    /// Nothing about `inner` changes unless this returns `Committed`.
    fn validate_and_commit(
        &self,
        inner: &mut Inner,
        actor: &Actor,
        updates: &[Vec<u8>],
    ) -> Result<Settled, FrameRefusal> {
        let decoded: Vec<Update> = updates
            .iter()
            .map(|bytes| decode(bytes))
            .collect::<Result<_, _>>()?;
        let merged = Update::merge_updates(decoded).encode_v1();
        let update = decode(&merged)?;

        // (2) New insertion ranges must belong to this participant.
        let live_sv = inner.live.state_vector();
        let mut new_clients = BTreeSet::new();
        for (client, ranges) in update.insertions(true).iter() {
            let known = live_sv.get(client);
            if ranges.iter().any(|range| range.end > known) {
                let client = client.get();
                if !inner.journal.may_use(client, &actor.participant) {
                    return Err(FrameRefusal::Identity { client });
                }
                new_clients.insert(client);
            }
        }

        // Candidate from the live state, observed before the update applies.
        let candidate = Document::from_state(&inner.live.encode_full())?;
        let branches_before = branch_ids(&candidate);
        let touched = Arc::new(Mutex::new(Touched::default()));
        let objects = candidate.objects_ref();
        {
            let sink = touched.clone();
            objects.observe_deep(TOUCHED_OBSERVER, move |txn, events| {
                let mut sink = sink.lock().unwrap_or_else(|poison| poison.into_inner());
                for event in events.iter() {
                    sink.collect(txn, event);
                }
            });
        }
        let applied = candidate.apply_remote(update);
        objects.unobserve_deep(TOUCHED_OBSERVER);
        applied?;

        // (1) Settled only: pending content never reaches the live document.
        if candidate.has_missing_updates() {
            return Ok(Settled::Unresolved);
        }
        // Exact replay: nothing new, nothing to attribute.
        let live_revision = inner.live.revision();
        if candidate.revision() == live_revision {
            return Ok(Settled::Replay);
        }

        // (3) State outside the layout, and record identity.
        let live_inspection = inner.live.inspect();
        let candidate_inspection = candidate.inspect();
        if live_inspection.anomalies != candidate_inspection.anomalies {
            let mut changed: Vec<String> = live_inspection
                .anomalies
                .iter()
                .filter(|(key, value)| candidate_inspection.anomalies.get(*key) != Some(*value))
                .map(|(key, _)| key.clone())
                .collect();
            changed.extend(
                candidate_inspection
                    .anomalies
                    .keys()
                    .filter(|key| !live_inspection.anomalies.contains_key(*key))
                    .cloned(),
            );
            return Err(FrameRefusal::HiddenStructure(changed));
        }
        let branches_after = branch_ids(&candidate);
        for (id, before) in &branches_before {
            if let Some(after) = branches_after.get(id)
                && after != before {
                    return Err(FrameRefusal::Replaced(id.clone()));
                }
        }

        let before = layout::view(&live_inspection, &live_revision);
        let after = layout::view(&candidate_inspection, &candidate.revision());

        // (4) Effective writes, including same-value ones the projection hides.
        let touched = touched
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(path) = touched.nested {
            return Err(FrameRefusal::HiddenStructure(vec![path]));
        }
        for (id, key) in &touched.keys {
            let existed = before.get(id).is_some();
            let kind = after
                .get(id)
                .or_else(|| before.get(id))
                .map(|record| record.kind.clone());
            if key == KIND_KEY {
                if existed {
                    return Err(FrameRefusal::Write {
                        id: id.clone(),
                        key: key.clone(),
                        reason: "the kind of an existing record never changes",
                    });
                }
                continue;
            }
            if AUTHORSHIP_KEYS.contains(&key.as_str()) || is_reserved_key(key) {
                return Err(FrameRefusal::Write {
                    id: id.clone(),
                    key: key.clone(),
                    reason: "written only by the host",
                });
            }
            let Some(kind) = kind else {
                // Created and deleted within the update: inert.
                continue;
            };
            if self.composition.writer_of(&kind, key).is_none() {
                return Err(FrameRefusal::Write {
                    id: id.clone(),
                    key: key.clone(),
                    reason: "no enabled capability owns this key",
                });
            }
        }

        // (5) The visible change.
        let delta = audit(&self.composition, &before, &after).map_err(FrameRefusal::Audit)?;
        for id in &delta.created {
            match id_client(id) {
                Some(client) if new_clients.contains(&client) => {}
                _ => return Err(FrameRefusal::ForeignId { id: id.clone() }),
            }
        }
        // Every write the frame made, including same-value writes the delta
        // cannot show: the effective writes the deep observer reported, plus
        // every key of each record the frame created (keys written inside a
        // map created in the same update are not reported separately).
        let mut attempted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (id, key) in &touched.keys {
            if key != KIND_KEY && !key.starts_with('<') {
                attempted.entry(id.clone()).or_default().insert(key.clone());
            }
        }
        for id in &delta.created {
            if let Some(record) = after.get(id) {
                attempted
                    .entry(id.clone())
                    .or_default()
                    .extend(record.props.keys().cloned());
            }
        }
        let principal = Principal::from(actor);
        self.authorize(
            &principal,
            &Origin::ReplicaFrame,
            &before,
            &after,
            &delta,
            &attempted,
        )?;

        // Commit: the update and its stamps, one transaction.
        let stamp_update = decode(&merged)?;
        let ((), raw) = inner.live.write(|txn, objects| {
            txn.apply_update(stamp_update)
                .map_err(|error| DocError::Apply(error.to_string()))?;
            stamp(txn, objects, &delta, actor)
        })?;
        if inner.live.has_missing_updates() {
            return Err(FrameRefusal::Internal);
        }
        for client in new_clients {
            inner
                .journal
                .bindings
                .insert(client, actor.participant.clone());
        }
        let commit = record(inner, actor, None, &delta, raw);
        Ok(Settled::Committed(Box::new(commit)))
    }

    /// The authority check for one change, whatever its route. Every
    /// capability involved (owner of a touched kind, writer of a touched or
    /// attempted key, everyone on a deletion) gets its verifier's facts, if it
    /// has a verifier, and then its `validate_change`. Runs whenever anything
    /// was written or attempted, even when no value changed.
    fn authorize(
        &self,
        principal: &Principal,
        origin: &Origin,
        before: &View,
        after: &View,
        delta: &Delta,
        attempted: &BTreeMap<String, BTreeSet<String>>,
    ) -> Result<(), Refused> {
        if delta.is_empty() && attempted.values().all(BTreeSet::is_empty) {
            return Ok(());
        }
        let mut involved = BTreeSet::new();
        let mut touch = |id: &str, keys: &mut dyn Iterator<Item = &String>| {
            let kind = after
                .get(id)
                .or_else(|| before.get(id))
                .map(|record| record.kind.clone());
            if let Some(kind) = kind {
                // The owner, whoever extends the kind, and whoever declared it
                // in `reads`. A reader may refuse a change to a record it can
                // see; it still cannot write a key it does not own.
                involved.extend(
                    self.composition
                        .interested_in_kind(&kind)
                        .map(str::to_string),
                );
                for key in keys {
                    if let Some(writer) = self.composition.writer_of(&kind, key) {
                        involved.insert(writer.to_string());
                    }
                }
            }
        };
        for (id, keys) in attempted {
            touch(id, &mut keys.iter());
        }
        for (id, keys) in &delta.modified {
            touch(id, &mut keys.iter());
        }
        for id in &delta.created {
            let keys: Vec<String> = after
                .get(id)
                .map(|record| record.props.keys().cloned().collect())
                .unwrap_or_default();
            touch(id, &mut keys.iter());
        }
        // Deleted records: every capability is told; owners check their own
        // references and may refuse by who is deleting (`before` holds them).
        if !delta.deleted.is_empty() {
            involved.extend(self.capabilities.keys().cloned());
        }
        for id in involved {
            let Some(capability) = self.capabilities.get(&id) else {
                continue;
            };
            let bare = Change {
                principal,
                origin,
                before,
                after,
                delta,
                attempted,
                facts: None,
            };
            let facts = match self.verifiers.get(&id) {
                Some(verifier) => Some(
                    verifier
                        .verify(&bare)
                        .map_err(|error| Refused::Verify(id.clone(), error))?,
                ),
                None => None,
            };
            let change = Change {
                facts: facts.as_ref(),
                ..bare
            };
            capability
                .validate_change(&change)
                .map_err(|error| Refused::Validate(id.clone(), error))?;
        }
        Ok(())
    }

    // ── actions and batches ──────────────────────────────────────────────

    pub fn invoke(
        &self,
        actor: &Actor,
        action: &str,
        args: serde_json::Value,
    ) -> Result<Invoked, InvokeError> {
        let resolved = self
            .composition
            .resolve_action(action)
            .ok_or_else(|| InvokeError::UnknownAction(action.to_string()))?;
        let capability_id = resolved.capability.to_string();
        let preferred = resolved.preferred.to_string();
        let declaration = self
            .composition
            .descriptor(&capability_id)
            .and_then(|descriptor| descriptor.actions.iter().find(|a| a.name == preferred))
            .ok_or_else(|| InvokeError::UnknownAction(action.to_string()))?
            .clone();
        let allowed = matches!(
            (actor.caller, declaration.audience),
            (_, Audience::Both)
                | (crate::Caller::Human, Audience::Human)
                | (crate::Caller::Agent, Audience::Agent)
        );
        if !allowed {
            return Err(InvokeError::Audience {
                action: preferred,
                caller: actor.caller.as_str(),
            });
        }
        let capability = self
            .capabilities
            .get(&capability_id)
            .ok_or_else(|| InvokeError::UnknownAction(action.to_string()))?
            .clone();

        let mut commits = Vec::new();
        let mut refusals = Vec::new();
        let result = {
            let mut inner = self.lock();
            let full = inner.live.view();
            let scoped = scoped_view(&self.composition, &capability_id, &full);
            let view = if let Some(kinds) = self.plan_context.get(&capability_id) {
                View::new(
                    scoped.revision(),
                    scoped.records().cloned().chain(
                        full.records()
                            .filter(|r| kinds.contains(&r.kind) && !scoped.contains(&r.id))
                            .cloned(),
                    ),
                )
                .expect("views from the same valid document are disjoint")
            } else {
                scoped
            };
            let call = Call {
                action: preferred.clone(),
                args,
            };
            let _plan_span = samepage_capability::activity::Span::new(&capability_id, &format!("{capability_id}.plan"));
            let batch =
                capability
                    .plan_batch(&call, &view)
                    .map_err(|error| InvokeError::Capability {
                        capability: capability_id.clone(),
                        error,
                    })?;
            let refuse = |reason: String| InvokeError::PlannerBatch {
                capability: capability_id.clone(),
                action: preferred.clone(),
                reason,
            };
            // A planned batch is checked before anything is written: it must
            // be planned at the revision the planner was shown, and it may
            // only target the planner itself or a declared dependency. An
            // entry naming another capability is routing, not authority: that
            // owner's rules and its own validate_change still decide.
            if batch.base_revision != view.revision() {
                return Err(refuse(format!(
                    "base revision {:?} is not the revision it planned against ({:?})",
                    batch.base_revision,
                    view.revision()
                )));
            }
            for entry in &batch.entries {
                if self.composition.descriptor(&entry.capability).is_none() {
                    return Err(refuse(format!(
                        "entry names {:?}, which is not in this composition",
                        entry.capability
                    )));
                }
                if !self
                    .composition
                    .may_target(&capability_id, &entry.capability)
                {
                    return Err(refuse(format!(
                        "entry names {:?}, which {capability_id:?} does not declare as a dependency",
                        entry.capability
                    )));
                }
            }
            let batch = if let Some(composer) = self.composers.get(&capability_id) {
                let composed =
                    composer
                        .compose(&call, &inner.live.view(), batch)
                        .map_err(|error| InvokeError::Capability {
                            capability: capability_id.clone(),
                            error,
                        })?;
                if composed.base_revision != view.revision() {
                    return Err(refuse("native composer changed the base revision".into()));
                }
                for entry in &composed.entries {
                    if self.composition.descriptor(&entry.capability).is_none() {
                        return Err(refuse(format!(
                            "{} capability is not loaded",
                            entry.capability
                        )));
                    }
                }
                composed
            } else {
                batch
            };
            if declaration.effect == Effect::Query {
                let ops: usize = batch.entries.iter().map(|entry| entry.plan.ops.len()).sum();
                if ops > 0 {
                    return Err(InvokeError::QueryMutates {
                        action: preferred,
                        ops,
                    });
                }
                Ok(Invoked {
                    action: preferred,
                    // A query still speaks: readback comes back from the
                    // entries in order.
                    readback: batch
                        .entries
                        .iter()
                        .map(|entry| entry.plan.readback.clone())
                        .filter(|readback| !readback.is_empty())
                        .collect(),
                    ids: BTreeMap::new(),
                    commit: None,
                })
            } else {
                // The origin is the capability whose action was invoked and
                // the preferred name of that action. An entry naming another
                // owner is routing; it never becomes the author of the
                // change, and a batch with no entries at all is a no-op
                // rather than something to index into.
                let origin = Origin::HostAction {
                    capability: capability_id.clone(),
                    action: preferred.clone(),
                };
                match self.commit_locked(
                    &mut inner,
                    actor,
                    &batch,
                    Some(preferred.clone()),
                    &origin,
                ) {
                    Ok((commit, ids, readback)) => {
                        commits.push(commit.clone());
                        self.drain(&mut inner, &mut commits, &mut refusals);
                        Ok(Invoked {
                            action: preferred,
                            readback,
                            ids,
                            commit: Some(commit),
                        })
                    }
                    // A mutating action whose plan changes nothing is an
                    // explicit no-op: no journal entry, no announcement.
                    Err(CommitError::Empty) => Ok(Invoked {
                        action: preferred,
                        readback: batch
                            .entries
                            .iter()
                            .map(|entry| entry.plan.readback.clone())
                            .filter(|readback| !readback.is_empty())
                            .collect(),
                        ids: BTreeMap::new(),
                        commit: None,
                    }),
                    Err(error) => Err(error.into()),
                }
            }
        };
        self.announce(&commits, &refusals);
        result
    }

    /// Commit a batch planned by a native composition adapter for an existing
    /// action. This is not an input route: the dispatcher supplies the actor
    /// and the native adapter supplies the plan. All owners' authority hooks,
    /// native verifiers, resolution, cascades and attribution still run before
    /// one publication. The journal retains the action's preferred name.
    pub fn invoke_composed(
        &self,
        actor: &Actor,
        action: &str,
        batch: &Batch,
    ) -> Result<Invoked, InvokeError> {
        let resolved = self
            .composition
            .resolve_action(action)
            .ok_or_else(|| InvokeError::UnknownAction(action.into()))?;
        let capability = resolved.capability.to_string();
        let preferred = resolved.preferred.to_string();
        let declaration = self
            .composition
            .descriptor(&capability)
            .and_then(|d| d.actions.iter().find(|a| a.name == preferred))
            .ok_or_else(|| InvokeError::UnknownAction(action.into()))?;
        if !matches!(
            (actor.caller, declaration.audience),
            (_, Audience::Both)
                | (crate::Caller::Human, Audience::Human)
                | (crate::Caller::Agent, Audience::Agent)
        ) {
            return Err(InvokeError::Audience {
                action: preferred,
                caller: actor.caller.as_str(),
            });
        }
        if declaration.effect != Effect::Mutate {
            return Err(InvokeError::QueryMutates {
                action: preferred,
                ops: batch.entries.iter().map(|e| e.plan.ops.len()).sum(),
            });
        }
        let origin = Origin::HostAction {
            capability,
            action: preferred.clone(),
        };
        let mut commits = Vec::new();
        let mut refusals = Vec::new();
        let result = {
            let mut inner = self.lock();
            match self.commit_locked(&mut inner, actor, batch, Some(preferred.clone()), &origin) {
                Ok((commit, ids, readback)) => {
                    commits.push(commit.clone());
                    self.drain(&mut inner, &mut commits, &mut refusals);
                    Ok(Invoked {
                        action: preferred,
                        readback,
                        ids,
                        commit: Some(commit),
                    })
                }
                Err(CommitError::Empty) => Ok(Invoked {
                    action: preferred,
                    ids: BTreeMap::new(),
                    commit: None,
                    readback: batch
                        .entries
                        .iter()
                        .map(|e| e.plan.readback.clone())
                        .filter(|s| !s.is_empty())
                        .collect(),
                }),
                Err(e) => Err(e.into()),
            }
        };
        self.announce(&commits, &refusals);
        result
    }

    /// Commit an untrusted batch (for example one a browser planned). It is
    /// resolved here against the live document; a batch that resolved
    /// elsewhere is never accepted as such.
    pub fn commit_batch(&self, actor: &Actor, batch: &Batch) -> Result<Commit, CommitError> {
        let _span = samepage_capability::activity::Span::new("doc", "doc.batch.apply");
        let mut commits = Vec::new();
        let mut refusals = Vec::new();
        let result = {
            let mut inner = self.lock();
            let result =
                self.commit_locked(&mut inner, actor, batch, None, &Origin::UntrustedBatch);
            if let Ok((commit, _, _)) = &result {
                commits.push(commit.clone());
                self.drain(&mut inner, &mut commits, &mut refusals);
            }
            result.map(|(commit, _, _)| commit)
        };
        self.announce(&commits, &refusals);
        result
    }

    #[allow(clippy::type_complexity)]
    fn commit_locked(
        &self,
        inner: &mut Inner,
        actor: &Actor,
        batch: &Batch,
        action: Option<String>,
        origin: &Origin,
    ) -> Result<(Commit, BTreeMap<String, String>, Vec<String>), CommitError> {
        let _span = samepage_capability::activity::Span::new("doc", "doc.batch.resolve");
        let before = inner.live.view();
        if batch.base_revision != before.revision() {
            return Err(PlanError::Stale {
                expected: batch.base_revision.clone(),
                found: before.revision().to_string(),
            }
            .into());
        }
        let mut minter = inner.live.minter();
        let mut builder = BatchBuilder::new(&self.composition, &before);
        for entry in &batch.entries {
            builder.push(&entry.capability, &entry.plan, &mut minter)?;
        }
        // Deletes cascade to capabilities that hold references.
        let mut announced: BTreeSet<String> = BTreeSet::new();
        for round in 0.. {
            let deleted: Vec<String> = before
                .records()
                .map(|record| record.id.clone())
                .filter(|id| !builder.working_view().contains(id) && !announced.contains(id))
                .collect();
            if deleted.is_empty() {
                break;
            }
            if round >= MAX_CASCADE_ROUNDS {
                return Err(CommitError::RunawayCascade);
            }
            announced.extend(deleted.iter().cloned());
            for descriptor in self.composition.descriptors() {
                let Some(capability) = self.capabilities.get(&descriptor.id) else {
                    continue;
                };
                let working =
                    scoped_view(&self.composition, &descriptor.id, builder.working_view());
                let plan = capability
                    .cascade_with_before(
                        &deleted,
                        &scoped_view(&self.composition, &descriptor.id, &before),
                        &working,
                    )
                    .map_err(|error| CommitError::Capability {
                        capability: descriptor.id.clone(),
                        error,
                    })?;
                if !plan.ops.is_empty() {
                    builder.push(&descriptor.id, &plan, &mut minter)?;
                }
            }
        }
        let resolved = builder.finish()?;
        resolved.ensure_current(&inner.live.revision())?;
        let delta = Delta::between(&before, resolved.projected());
        // Every key each op wrote, including values equal to the old ones.
        let mut attempted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for op in resolved.ops() {
            match op {
                ResolvedOp::Create { id, props, .. } | ResolvedOp::Set { id, props, .. } => {
                    attempted
                        .entry(id.clone())
                        .or_default()
                        .extend(props.keys().cloned());
                }
                ResolvedOp::Delete { .. } => {}
            }
        }
        let principal = Principal::from(actor);
        self.authorize(
            &principal,
            origin,
            &before,
            resolved.projected(),
            &delta,
            &attempted,
        )?;

        let ops = resolved.ops().to_vec();
        if ops.is_empty() {
            return Err(CommitError::Empty);
        }
        let ((), raw) = inner.live.write(|txn, objects| {
            document::write_ops(txn, objects, &ops)?;
            stamp(txn, objects, &delta, actor)
        })?;
        inner.live.advance(&minter);
        if raw.is_empty() {
            // Nothing reached the document; there is nothing to announce.
            return Err(CommitError::Empty);
        }
        let commit = record(inner, actor, action, &delta, raw);
        Ok((commit, resolved.ids().clone(), resolved.readback().to_vec()))
    }

    fn announce(&self, commits: &[Commit], refusals: &[(Actor, FrameRefusal)]) {
        if !commits.is_empty() {
            let observers = self
                .observers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            for commit in commits {
                for observer in &observers {
                    observer(commit);
                }
            }
        }
        if !refusals.is_empty() {
            let observers = self
                .refusal_observers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            for (actor, refusal) in refusals {
                for observer in &observers {
                    observer(actor, refusal);
                }
            }
        }
    }
}

enum Admitted {
    Committed(Box<Commit>),
    Held(usize),
    Replay,
}

enum Settled {
    Committed(Box<Commit>),
    Unresolved,
    Replay,
}

fn remove_held(inner: &mut Inner, participant: &str) {
    inner.held.retain(|(held, _)| held != participant);
}

/// Stamp authorship on every record the delta created or modified, inside the
/// transaction that makes the change.
fn stamp(
    txn: &mut yrs::TransactionMut,
    objects: &yrs::MapRef,
    delta: &Delta,
    actor: &Actor,
) -> Result<(), DocError> {
    let byline = Any::from(actor.byline());
    let participant = Any::from(actor.participant.as_str());
    let created_at = Any::from(rfc3339_now());
    for id in &delta.created {
        let record = document::record(txn, objects, id)?;
        record.insert(txn, "created_by", byline.clone());
        record.insert(txn, "created_by_participant", participant.clone());
        record.insert(txn, "created_at", created_at.clone());
    }
    for id in delta.created.iter().chain(delta.modified.keys()) {
        let record = document::record(txn, objects, id)?;
        record.insert(txn, "touched_by", byline.clone());
        record.insert(txn, "touched_by_participant", participant.clone());
    }
    Ok(())
}

/// The host clock as RFC 3339 UTC to the second, for `created_at` stamps.
fn rfc3339_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339(seconds)
}

/// Civil date from days since 1970-01-01 (proleptic Gregorian calendar).
fn rfc3339(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod time_tests {
    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(super::rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::rfc3339(1_791_782_904), "2026-10-12T05:28:24Z");
    }
}

fn record(
    inner: &mut Inner,
    actor: &Actor,
    action: Option<String>,
    delta: &Delta,
    raw: Vec<u8>,
) -> Commit {
    let revision = inner.live.revision();
    let mut commit = Commit {
        seq: 0,
        actor: actor.clone(),
        action: action.clone(),
        revision: revision.clone(),
        created: delta.created.iter().cloned().collect(),
        modified: delta.modified.keys().cloned().collect(),
        deleted: delta.deleted.iter().cloned().collect(),
        visible_change: !delta.is_empty(),
        update: sync::update(raw),
    };
    commit.seq = inner.journal.record(Entry {
        seq: 0,
        actor: actor.clone(),
        action,
        revision,
        created: commit.created.clone(),
        modified: commit.modified.clone(),
        deleted: commit.deleted.clone(),
        visible_change: commit.visible_change,
    });
    commit
}

/// What a capability sees: the kinds it owns, extends or reads.
fn scoped_view(composition: &Composition, capability: &str, view: &View) -> View {
    let Some(descriptor) = composition.descriptor(capability) else {
        return View::new(view.revision(), Vec::new()).expect("empty view");
    };
    let kinds: Vec<&str> = descriptor
        .kinds
        .iter()
        .map(|claim| claim.kind.as_str())
        .chain(descriptor.props.iter().map(|claim| claim.kind.as_str()))
        .chain(descriptor.reads.iter().map(String::as_str))
        .collect();
    view.of_kinds(&kinds)
}

/// Branch identity of every record map, so a replaced map is caught even when
/// its content is identical.
fn branch_ids(document: &Document) -> BTreeMap<String, BranchID> {
    let objects = document.objects_ref();
    let txn = document.doc().transact();
    objects
        .iter(&txn)
        .filter_map(|(id, value)| match value {
            Out::YMap(map) => Some((id.to_string(), map.as_ref().id())),
            _ => None,
        })
        .collect()
}

/// Effective writes the candidate's deep observer reported.
#[derive(Debug, Clone, Default)]
struct Touched {
    keys: BTreeSet<(String, String)>,
    nested: Option<String>,
}

impl Touched {
    fn collect(&mut self, txn: &yrs::TransactionMut, event: &Event) {
        let Event::Map(map_event) = event else {
            self.nested
                .get_or_insert_with(|| "a non-map event".to_string());
            return;
        };
        let path = map_event.path();
        match path.len() {
            // The objects root: a record inserted, replaced or removed.
            0 => {
                for (id, change) in map_event.keys(txn) {
                    if let EntryChange::Updated(..) = change {
                        // A map replaced under an existing id; also caught by
                        // branch identity, reported here for the reason.
                        self.keys.insert((id.to_string(), "<record>".to_string()));
                    }
                }
            }
            // A record map: which keys were written, whatever the value.
            1 => {
                let Some(PathSegment::Key(id)) = path.front() else {
                    self.nested
                        .get_or_insert_with(|| "an index path".to_string());
                    return;
                };
                for key in map_event.keys(txn).keys() {
                    self.keys.insert((id.to_string(), key.to_string()));
                }
            }
            _ => {
                self.nested
                    .get_or_insert_with(|| format!("nested content under {path:?}"));
            }
        }
    }
}

/// Format tag of the saved envelope.
pub const ENVELOPE_FORMAT: &str = "samepage.document.v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    layout_version: u32,
    generation: u64,
    /// Base64 of the full Yrs v1 state.
    state: String,
    journal: Journal,
}

static TEMPORARY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    use std::io::Write as _;
    let failed = |path: &Path, source| SaveError::Write {
        path: path.to_path_buf(),
        source,
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| failed(parent, e))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".to_string());
    let temporary = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TEMPORARY.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| failed(path, e))
}
