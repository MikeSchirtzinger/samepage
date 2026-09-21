//! The agent's half of the shared atlas.
//!
//! The human writes to the CRDT straight from the browser replica over `/ws`.
//! The agent writes to the same CRDT through the actions below. There is no
//! privileged writer and no server-side "apply the model's intent" step: both
//! sides call the same `same_page_atlas_core` vocabulary against the same
//! document, and yrs merges whatever overlaps.
//!
//! Two rules give the surface teeth:
//!
//! 1. A node that names a source is verified against the real file before the
//!    write is accepted, so a claim cannot point at a line range that does not
//!    exist (`Effect::Reject`, which the runtime turns into a retryable tool
//!    error rather than a silent success).
//! 2. `atlas_read` reports who last touched every node, so the human moving,
//!    re-labelling, or flagging something is *in the agent's next context*
//!    instead of being invisible to it.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use ag_ui_canvas::codec::{decode_frame, encode_sync, Frame};
use ag_ui_canvas::scene::{Author, Scene};
use ag_ui_canvas::sync::{greeting, handle_payload, update_message};
use ag_ui_surface::diagram;
use ag_ui_surface::services::{ServiceRegistry, ServiceRequirement};
use ag_ui_surface::{
    ActionAudience, Attention as SemanticAttention, AttentionMode, ClientModule, Effect, Extension,
    HttpMethod, ParticipantKind, RouteDef, RouteRequest, RouteResponse, SemanticTarget,
    SemanticTargetRef, SemanticTargetService, StateBacking, StateSnapshot, SurfaceState, ToolDef,
    Transport, WasmMount,
};
use base64::Engine as _;
use parking_lot::Mutex;
use same_page_atlas_core as atlas;
use serde_json::{json, Value as JsonValue};
use yrs::Subscription;

use crate::{cement, import_archify, lanes, map_cement, source::Repo, tier};

#[path = "page_validation.rs"]
mod page_validation;

const FOCUS_EVENT: &str = "atlas.focus";
const FLUSH_INTERVAL: Duration = Duration::from_millis(1_500);
const ATTENTION_STALE_AFTER: Duration = Duration::from_secs(10);
const REJECTED_GESTURE_STALE_AFTER: Duration = Duration::from_secs(30);
const DEFAULT_WAIT_SECONDS: u64 = 60;
const MAX_WAIT_SECONDS: u64 = 600;

#[derive(Clone, Debug)]
struct HumanAttention {
    viewport: atlas::ViewportRect,
    dragging: Option<String>,
    zoom: Option<f64>,
    sequence: u64,
    source: AttentionSource,
    /// What the human's browser will do with an agent's reveal: `follow` moves
    /// the view, `ask` offers the jump at the pane edge, `hold` keeps the
    /// camera theirs. Reported so the agent can describe what it just did
    /// accurately instead of assuming the view moved.
    camera: CameraPolicy,
    observed_at: Instant,
}

#[derive(Clone, Debug)]
struct RejectedGesture {
    key: String,
    target: Option<String>,
    reason: String,
    observed_at: Instant,
}

#[derive(Clone, Debug)]
struct RejectedGestureDraft {
    key: String,
    target: Option<String>,
    reason: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum AttentionSource {
    #[default]
    Selection,
    Composer,
}

impl AttentionSource {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "selection" => Some(Self::Selection),
            "composer" => Some(Self::Composer),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum CameraPolicy {
    #[default]
    Follow,
    Ask,
    Hold,
}

impl CameraPolicy {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "follow" => Some(Self::Follow),
            "ask" => Some(Self::Ask),
            "hold" => Some(Self::Hold),
            _ => None,
        }
    }

    /// How the read-back says it, in terms of what the agent can do next.
    fn reading(self) -> &'static str {
        match self {
            Self::Follow => {
                "Camera: the human lets a deliberate reveal move their view, except while their own hand is on it."
            }
            Self::Ask => {
                "Camera: the human keeps the view; a reveal becomes a marker at the pane edge they can click. Say where to look, do not claim to have moved them."
            }
            Self::Hold => {
                "Camera: the human holds the view; a reveal will not move it. Describe where to look in their own terms."
            }
        }
    }
}

#[derive(Default)]
struct CommitJournal {
    sequence: u64,
    latest_byline: HashMap<String, u64>,
}

pub struct AtlasState {
    validation: Mutex<page_validation::BrowserRegistry>,
    scene: Mutex<Scene>,
    transport: Transport,
    /// What the agent was told the page said, the last time it read it. The
    /// basis for the CHANGED SINCE YOUR LAST READ section, see
    /// [`AtlasState::changes_since_last_read`].
    last_read: Mutex<Option<atlas::Digest>>,
    decision_previews: Mutex<std::collections::BTreeMap<String, cement::DecisionBundle>>,
    /// Which committed operation the last `atlas_read` included. This is
    /// session-only, on the same terms as `last_read`.
    read_cursor: Mutex<u64>,
    /// Latest committed operation from each Atlas byline. This is enough to
    /// answer whether somebody other than a waiting caller has written since
    /// its last read, without retaining an unbounded transaction log.
    commits: Mutex<CommitJournal>,
    /// Keeps one scene transaction and its byline record adjacent. The Yrs
    /// observer fires synchronously during the transaction, then this gate
    /// records the caller before another writer can enter.
    commit_gate: Mutex<()>,
    /// Incremented by the existing Yrs observer for every real update. A sync
    /// frame that changes nothing therefore does not become a false commit.
    update_sequence: AtomicU64,
    /// Wakes `await_atlas` after CRDT commits. Attention posts never touch the
    /// document or this notifier.
    changed: tokio::sync::Notify,
    /// What the human can currently see and which node is mid-drag. Like
    /// `last_read`, this is session context, not shared document state.
    attention: Mutex<Option<HumanAttention>>,
    /// A shortcut that could not act is still an observable human event. It
    /// has its own lease so routine viewport refreshes do not erase it before
    /// an attached agent can read why no mark appeared.
    rejected_gesture: Mutex<Option<RejectedGesture>>,
    /// Host-issued order for ephemeral observations. This is separate from
    /// document commits because attention never mutates the CRDT.
    attention_sequence: AtomicU64,
    /// The repository this atlas is *about*, injected by the host as the
    /// declared `repository` service rather than handed in by `main`.
    ///
    /// Set once, by [`AtlasExtension::bind_services`], before the surface is
    /// built and before any request is served. It is a `OnceLock` rather than a
    /// constructor argument because a declared requirement composition can
    /// verify is worth more than an `Arc` that only `main` knows about, see
    /// `ag_ui_surface::services`.
    repo: OnceLock<Arc<Repo>>,
    state_path: PathBuf,
    flush: Mutex<(bool, Instant)>,
    /// Keeps the update observer alive for the process lifetime; dropping the
    /// subscription would silently stop broadcasting the agent's own edits.
    _updates: Mutex<Option<Subscription>>,
    /// The display name of the agent whose call is currently being dispatched,
    /// stashed by [`Extension::note_caller`] and read while applying a write.
    ///
    /// Every agent write used to sign the generic `Author::Agent`, so a map
    /// edited by three different models recorded one indistinguishable author.
    /// Last-writer-wins is sound here for the reason `Surface::note_caller`
    /// documents: a surface that records authorship also serialises its writes.
    actor_label: Mutex<Option<String>>,
    /// The host attention service, handed over once at install time.
    ///
    /// `atlas_read` gets the service through the `&dyn SurfaceState` its effect
    /// is called with, but a parked wait does not: an `Effect::AsyncQuery`
    /// future holds this state and nothing else, so `await_atlas` and
    /// `await_input` had no way to say where the human is in the payload they
    /// wake with. Holding the handle here is what makes a wake carry the live
    /// selection. Context only, exactly as everywhere else; it authorizes
    /// nothing and is never consulted for permission.
    attention_host: OnceLock<Arc<SemanticTargetService>>,
    /// A notifier that never fires, returned when no attention host is bound.
    ///
    /// Handing back a real `&Notify` in both cases is what lets a waiter
    /// `enable()` itself before it checks its predicate. An `Option` would
    /// force the caller to write the wait twice, and the version without the
    /// early enrolment is exactly the one that loses a gesture landing between
    /// the check and the await.
    attention_quiet: tokio::sync::Notify,
    /// Whether the agent-ink tool vocabulary is advertised and reachable.
    ///
    /// Read once from `AGUI_AGENT_INK` at startup (see `main.rs`) and never
    /// changed afterward, so a running process cannot flip from advertising
    /// zero agent-ink tools to seven mid-session. Default off: with this
    /// false, `actions()` and the `/atlas/agent-ink` route behave exactly as
    /// they did before B4.
    agent_ink: bool,
    /// Trust-tier bookkeeping no CRDT node can honestly carry: mode, hash
    /// baselines, lane claims, cemented obligation ids. See `src/tier.rs`.
    tiers: Mutex<tier::TierRegistry>,
    tiers_path: PathBuf,
}

struct CommitGuard<'a> {
    state: &'a AtlasState,
    _gate: parking_lot::MutexGuard<'a, ()>,
    byline: String,
    before: u64,
}

impl Drop for CommitGuard<'_> {
    fn drop(&mut self) {
        if self.state.update_sequence.load(Ordering::Acquire) != self.before {
            self.state.record_commit(&self.byline);
        }
    }
}

impl AtlasState {
    /// Who to sign an agent-audience write as: the announced participant when
    /// one attached, otherwise the generic agent this always used to be.
    ///
    /// Reads `ag_ui_surface::current_actor()` first: that value is scoped to
    /// the request being served, so it cannot be overwritten by a concurrent
    /// caller the way `actor_label` can between `note_caller` and the write.
    /// `actor_label` stays as the fallback for paths with no request, such as
    /// the in-page turn loop.
    fn agent_author(&self) -> Author {
        if let Some(actor) = ag_ui_surface::current_actor() {
            return match actor.label {
                Some(label) => Author::Named(label),
                None => Author::Agent,
            };
        }
        match self.actor_label.lock().clone() {
            Some(label) => Author::Named(label),
            None => Author::Agent,
        }
    }

    pub fn open(
        transport: Transport,
        state_path: PathBuf,
        agent_ink: bool,
    ) -> Result<Arc<Self>, String> {
        let scene = match std::fs::read_to_string(&state_path) {
            Ok(encoded) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded.trim())
                    .map_err(|error| format!("saved atlas is not valid base64: {error}"))?;
                Scene::from_state(&bytes)
                    .map_err(|error| format!("saved atlas could not be loaded: {error}"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Scene::new(),
            Err(error) => return Err(format!("could not read {}: {error}", state_path.display())),
        };

        // A document saved under the old "living ink" name carries objects
        // tagged with the old kinds. Rewrite them here, before anything else
        // reads or broadcasts the document, so every later comparison sees
        // one spelling. Persisted below once the state exists.
        let migrated = same_page_atlas_core::agent_ink::migrate_legacy_kinds(&scene)
            .map_err(|error| format!("saved atlas could not be migrated: {error}"))?;

        let tiers_path = tier::registry_path(&state_path);
        let tiers = tier::TierRegistry::load(&tiers_path);
        let state = Arc::new(Self {
            validation: Mutex::new(page_validation::BrowserRegistry::default()),
            scene: Mutex::new(scene),
            transport,
            last_read: Mutex::new(None),
            decision_previews: Mutex::new(std::collections::BTreeMap::new()),
            read_cursor: Mutex::new(0),
            commits: Mutex::new(CommitJournal::default()),
            commit_gate: Mutex::new(()),
            update_sequence: AtomicU64::new(0),
            changed: tokio::sync::Notify::new(),
            attention: Mutex::new(None),
            rejected_gesture: Mutex::new(None),
            attention_sequence: AtomicU64::new(0),
            repo: OnceLock::new(),
            state_path,
            flush: Mutex::new((false, Instant::now())),
            _updates: Mutex::new(None),
            actor_label: Mutex::new(None),
            attention_host: OnceLock::new(),
            attention_quiet: tokio::sync::Notify::new(),
            agent_ink,
            tiers: Mutex::new(tiers),
            tiers_path,
        });

        // Every committed transaction, the agent's own writes and the merged
        // result of the human's, goes out on the binary transport as a y-sync
        // Update. This is the whole broadcast path; there is no second one.
        let subscription = {
            let ws_tx = state.transport.ws_tx.clone();
            let weak = Arc::downgrade(&state);
            let scene = state.scene.lock();
            scene
                .on_update(move |update| {
                    let _ = ws_tx.send(encode_sync(&update_message(update)));
                    if let Some(state) = weak.upgrade() {
                        state.update_sequence.fetch_add(1, Ordering::AcqRel);
                        state.changed.notify_waiters();
                    }
                })
                .map_err(|error| format!("could not observe the atlas: {error}"))?
        };
        *state._updates.lock() = Some(subscription);
        state.spawn_flusher();
        if migrated > 0 {
            tracing::info!(migrated, "rewrote pre-rename agent ink object kinds");
            state.persist(true);
        }

        Ok(state)
    }

    /// Drain the pending save that [`Self::persist`] leaves behind.
    ///
    /// A throttled `persist(false)` marks the document dirty and returns
    /// without writing. Nothing else was scheduled to write it, so the LAST
    /// edit of any burst, which is every edit a human makes and then stops
    /// making, sat unsaved until some unrelated later write happened to get
    /// through, and was lost outright on restart. This ticker is what makes
    /// the throttle a delay rather than a drop.
    ///
    /// Held weakly so the task cannot keep the state alive, and skipped
    /// entirely outside a Tokio runtime so the unit tests still construct
    /// state directly.
    fn spawn_flusher(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(self);
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(state) = weak.upgrade() else { return };
                if state.flush.lock().0 {
                    state.persist(true);
                }
            }
        });
    }

    fn read(&self) -> Result<atlas::Atlas, String> {
        atlas::read(&self.scene.lock())
    }

    fn read_with_revision(&self) -> Result<(atlas::Atlas, String), String> {
        let scene = self.scene.lock();
        let atlas = atlas::read(&scene)?;
        let revision = scene
            .state_vector_v1()
            .map_err(|error| format!("could not read the atlas document revision: {error}"))?;
        Ok((
            atlas,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(revision),
        ))
    }

    fn agent_ink_enabled(&self) -> bool {
        self.agent_ink
    }

    pub(crate) fn agent_byline(&self) -> String {
        if let Some(actor) = ag_ui_surface::current_actor() {
            if let Some(label) = actor.label {
                return label;
            }
        }
        self.actor_label
            .lock()
            .clone()
            .unwrap_or_else(|| "agent".to_string())
    }

    fn begin_commit<'a>(&'a self, byline: impl Into<String>) -> CommitGuard<'a> {
        let gate = self.commit_gate.lock();
        let before = self.update_sequence.load(Ordering::Acquire);
        CommitGuard {
            state: self,
            _gate: gate,
            byline: byline.into(),
            before,
        }
    }

    fn record_commit(&self, byline: &str) {
        let mut commits = self.commits.lock();
        commits.sequence = commits.sequence.saturating_add(1);
        let sequence = commits.sequence;
        commits.latest_byline.insert(byline.to_string(), sequence);
        drop(commits);
        // The observer may have woken a waiter before this byline was safe to
        // record. Wake it once more now that the caller-aware check can pass.
        self.changed.notify_waiters();
    }

    fn read_with_commit_cursor(&self) -> Result<(atlas::Atlas, u64), String> {
        let _gate = self.commit_gate.lock();
        let atlas = self.read()?;
        let cursor = self.commits.lock().sequence;
        Ok((atlas, cursor))
    }

    pub(crate) fn unread_from_others(&self, me: &str) -> bool {
        let cursor = *self.read_cursor.lock();
        self.commits
            .lock()
            .latest_byline
            .iter()
            .any(|(byline, sequence)| *sequence > cursor && byline != me)
    }

    fn record_attention(&self, body: Option<JsonValue>) -> Result<u64, String> {
        let (viewport, dragging, camera, zoom, source, rejected_gesture) = parse_attention(body)?;
        let mut attention = self.attention.lock();
        let previous = self
            .attention_sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |sequence| {
                sequence.checked_add(1)
            })
            .map_err(|_| "attention observation sequence is exhausted".to_string())?;
        let sequence = previous + 1;
        *attention = Some(HumanAttention {
            viewport,
            dragging,
            zoom,
            sequence,
            source,
            camera,
            observed_at: Instant::now(),
        });
        if let Some(rejected) = rejected_gesture {
            *self.rejected_gesture.lock() = Some(RejectedGesture {
                key: rejected.key,
                target: rejected.target,
                reason: rejected.reason,
                observed_at: Instant::now(),
            });
        }
        Ok(sequence)
    }

    /// Revision of the newest human attention gesture. Zero with no host.
    ///
    /// Pointing writes nothing to the CRDT, so no document notifier carries
    /// it, and an agent parked on document changes alone slept through a human
    /// selecting half the board.
    pub(crate) fn human_attention_revision(&self) -> u64 {
        self.attention_host
            .get()
            .map_or(0, |service| service.human_attention_revision())
    }

    /// Fires when a human's attention changes. Never for an agent's own.
    pub(crate) fn attention_notifier(&self) -> &tokio::sync::Notify {
        self.attention_host
            .get()
            .map_or(&self.attention_quiet, |service| service.changed())
    }

    fn current_attention(&self) -> Option<HumanAttention> {
        let mut attention = self.attention.lock();
        if attention
            .as_ref()
            .is_some_and(|reading| reading.observed_at.elapsed() > ATTENTION_STALE_AFTER)
        {
            *attention = None;
        }
        attention.clone()
    }

    fn current_rejected_gesture(&self) -> Option<RejectedGesture> {
        let mut rejected = self.rejected_gesture.lock();
        if rejected
            .as_ref()
            .is_some_and(|gesture| gesture.observed_at.elapsed() > REJECTED_GESTURE_STALE_AFTER)
        {
            *rejected = None;
        }
        rejected.clone()
    }

    /// Opaque revision of the live CRDT document.
    ///
    /// A yrs state vector names exactly which clocks this replica has seen,
    /// which is the useful revision boundary for a cement receipt. It is not
    /// presented as a scalar sequence: concurrent replicas make that claim
    /// false. URL-safe base64 keeps the exact v1 bytes portable in JSON.
    pub(crate) fn document_revision(&self) -> Result<String, String> {
        let revision = self
            .scene
            .lock()
            .state_vector_v1()
            .map_err(|error| format!("could not read the atlas document revision: {error}"))?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(revision))
    }

    /// The agent's read of the page, plus what changed since its last one.
    ///
    /// Deliberately separate from [`SurfaceState::describe`], which stays
    /// side-effect free. Two reasons, and both bite in practice:
    ///
    /// * `describe()` is also served by a generic debug route. If *that*
    ///   advanced the mark, an HTTP probe would silently eat the delta and the
    ///   agent's next turn would be told nothing changed.
    /// * The browser's replica computes `Atlas::describe()` too, and "Do we
    ///   agree?" compares the two character-for-character. "What you were last
    ///   told" is session state, not document state, the browser has no way
    ///   to know it and no business knowing it.
    ///
    /// The first read of a session reports nothing rather than the whole page
    /// as new: all of it is listed above anyway.
    /// `reader` is whose own writes to discount, passed in rather than read
    /// from `actor_label` here. `actor_label` is shared state that the next
    /// caller overwrites, and `await_atlas` can be parked across that change:
    /// resolving the byline at read time would credit a waiting agent with the
    /// writes of whoever woke it, and then hide them.
    pub(crate) fn read_back(
        &self,
        surface: &dyn SurfaceState,
        reader: &str,
    ) -> Result<String, String> {
        let (atlas, commit_cursor) = self.read_with_commit_cursor()?;
        let mut text = atlas.describe();
        text.push_str(&format!(
            "\nPAGE VALIDATION\n{}\n",
            self.validation_report()?
        ));
        text.push_str(&hierarchy_readback(&atlas));
        text.push_str(&self.stale_claim_sources(&atlas));
        // The hosted state knows the service; a bare `AtlasState` parked in a
        // wait does not, and falls back to the handle bound at install time.
        // Without the fallback, every wake payload silently dropped WHERE THE
        // HUMAN IS while `semantic_targets_read` reported it correctly, which
        // is the F8 failure shape one layer further out.
        let selection = surface
            .semantic_targets()
            .or_else(|| self.attention_host.get().map(Arc::as_ref))
            .and_then(|targets| {
                targets.latest_active_matching(
                    ParticipantKind::Human,
                    AttentionMode::Selection,
                    "atlas",
                )
            });
        text.push_str(&human_attention_section(
            &atlas,
            selection.as_ref(),
            self.current_attention().as_ref(),
            self.current_rejected_gesture().as_ref(),
        ));
        let digest = atlas.digest();
        {
            let mut last = self.last_read.lock();
            if let Some(before) = last.as_ref() {
                text.push_str(&atlas::describe_changes_for(before, &digest, reader));
            }
            *last = Some(digest);
        }
        *self.read_cursor.lock() = commit_cursor;
        Ok(text)
    }

    /// Atlas document changes for a unified inbound wait. Register the
    /// returned notification before checking [`Self::unread_from_others`].
    pub(crate) fn changed(&self) -> &tokio::sync::Notify {
        &self.changed
    }

    /// Persist the whole document, at most once per [`FLUSH_INTERVAL`].
    ///
    /// Rough spot, stated plainly: this is throttled, so up to 1.5s of edits
    /// are lost if the process is killed mid-session. A real application wants
    /// an update log rather than whole-document snapshots.
    fn persist(&self, force: bool) {
        {
            let mut flush = self.flush.lock();
            flush.0 = true;
            if !force && flush.1.elapsed() < FLUSH_INTERVAL {
                return;
            }
            flush.0 = false;
            flush.1 = Instant::now();
        }
        let encoded = match self.scene.lock().encode_full() {
            Ok(bytes) => base64::engine::general_purpose::STANDARD.encode(bytes),
            Err(error) => {
                tracing::warn!(%error, "could not encode the atlas for persistence");
                return;
            }
        };
        if let Some(parent) = self.state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let temporary = self.state_path.with_extension("tmp");
        if let Err(error) = std::fs::write(&temporary, encoded)
            .and_then(|()| std::fs::rename(&temporary, &self.state_path))
        {
            tracing::warn!(%error, "could not persist the atlas");
        }
    }

    /// Bind the injected repository service. Called once by
    /// [`AtlasExtension::bind_services`]; a second call is a wiring bug rather
    /// than a silent rebind.
    pub fn bind_repo(&self, repo: Arc<Repo>) -> Result<(), String> {
        self.repo
            .set(repo)
            .map_err(|_| "the atlas repository service was already bound".to_string())
    }

    /// The bound repository.
    ///
    /// Composition binds services before the surface is built, so an unbound
    /// read cannot happen in a composed application. It is still an error
    /// rather than an `unwrap`: a source read that answered from nothing would
    /// be the fabrication this surface exists to make impossible.
    fn repo(&self) -> Result<&Arc<Repo>, String> {
        self.repo.get().ok_or_else(|| {
            "the atlas repository service is not bound; no source can be read".to_string()
        })
    }

    /// The active mode: `explain`, `map`, or `cement`. See `src/tier.rs`.
    fn atlas_mode(&self) -> String {
        self.tiers.lock().mode.clone()
    }

    /// Set the active mode. Shared, not per-session: a human clicking Map in
    /// the browser and an agent calling `atlas_mode_set` see the same value,
    /// on the same terms as everything else on this page.
    fn set_atlas_mode(&self, mode: &str) -> Result<(), String> {
        if !tier::MODES.contains(&mode) {
            return Err(format!(
                "{mode:?} is not a mode; choose one of {:?}",
                tier::MODES
            ));
        }
        let mut tiers = self.tiers.lock();
        tiers.mode = mode.to_string();
        tiers.save(&self.tiers_path)
    }

    /// Run the extractor over the bound project root and reconcile its lanes
    /// against the current Atlas.
    fn lanes_report(&self) -> Result<JsonValue, String> {
        let repo = self.repo()?;
        let report = lanes::scan(repo.root())?;
        let atlas = self.read()?;
        let tiers = self.tiers.lock();
        let g8 = tier::read_g8_status(repo.root());
        Ok(lanes::reconcile(&report, &atlas, &tiers, &g8))
    }

    /// `atlas_claim_lane`: bind an existing card's source link to a lane's
    /// evidence, and pin the baseline hash so the very next drift check has
    /// something to compare against. The only agent-writable path from
    /// `undeclared` to `verified`.
    /// Add a lane to a card's binding set. This never touches the CRDT scene:
    /// a card is not one file, and a claim is one more thing the card
    /// vouches for, not a replacement of what it already vouched for. See
    /// `src/tier.rs` module docs.
    fn claim_lane(&self, card_id: &str, lane_id: &str) -> Result<String, String> {
        let repo = self.repo()?;
        let atlas = self.read()?;
        atlas
            .node(card_id)
            .ok_or_else(|| format!("no node {card_id:?} on the atlas"))?;
        let report = lanes::scan(repo.root())?;
        let lane = report
            .lanes
            .iter()
            .find(|lane| lanes::lane_id(lane) == lane_id)
            .ok_or_else(|| format!("no lane {lane_id:?} in the current scan"))?;
        let path = lane.evidence.path.to_string_lossy().to_string();
        let line = lane.evidence.line.to_string();
        self.verify_source(&path, &line, "")
            .map_err(|error| format!("lane evidence does not hold: {error}"))?;
        let excerpt = repo
            .read(&path, &line)
            .map_err(|error| format!("could not read lane evidence: {error}"))?;
        let sha256 = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(excerpt.text.as_bytes()))
        };
        let mut tiers = self.tiers.lock();
        tier::record_claim(&mut tiers, &self.tiers_path, lane_id, card_id, &path, &line, &sha256)?;
        let count = tier::effective_bindings(atlas.node(card_id).expect("checked above"), &tiers).len();
        Ok(format!(
            "{card_id} now also claims lane {lane_id} ({path}:{line}), verified at {}; it now vouches for {count} lane{}",
            &sha256[..6],
            if count == 1 { "" } else { "s" }
        ))
    }

    /// `atlas_cement`: write the agreed Map as G8 obligations at the project
    /// root, one per verified or proposed card, plus one removal obligation
    /// per card struck by a live `replaces` edge.
    fn cement_map(&self) -> Result<JsonValue, String> {
        let repo = self.repo()?;
        let atlas = self.read()?;
        let mut tiers = self.tiers.lock();
        let g8 = tier::read_g8_status(repo.root());
        let computed = tier::annotate(&atlas, repo, &mut tiers, &self.tiers_path, &g8);
        let result = map_cement::build(&atlas, repo, &tiers, &computed);
        map_cement::write(&result, &mut tiers, &self.tiers_path)?;
        Ok(json!({
            "path": result.path.display().to_string(),
            "obligations_written": result.minted.len(),
            "minted": result.minted.iter().map(|(node_id, obligation_id)| json!({"node_id": node_id, "obligation_id": obligation_id})).collect::<Vec<_>>(),
            "skipped": result.skipped,
        }))
    }

    /// `atlas_remove_lane`: cement a removal obligation for an extractor
    /// lane, so the undeclared card renders as a removal gate (red until
    /// the code is actually gone) instead of a plain `undeclared` pulse.
    fn remove_lane(&self, lane_id: &str, reason: &str) -> Result<JsonValue, String> {
        let repo = self.repo()?;
        let report = lanes::scan(repo.root())?;
        let lane = report
            .lanes
            .iter()
            .find(|lane| lanes::lane_id(lane) == lane_id)
            .ok_or_else(|| format!("no lane {lane_id:?} in the current scan"))?;
        let reason = if reason.trim().is_empty() {
            "this lane should not exist in the code"
        } else {
            reason
        };
        let obligation = map_cement::build_lane_removal(lane, lane_id, reason).ok_or_else(|| {
            "this lane's evidence has no matchable text to gate on".to_string()
        })?;
        let obligations_path = repo.root().join("specs/obligations-v0.1.json");
        map_cement::merge_and_write(&obligations_path, vec![obligation.clone()])?;
        let mut tiers = self.tiers.lock();
        tiers.removals.insert(
            lane_id.to_string(),
            tier::RemovalRecord {
                obligation_id: obligation["id"].as_str().unwrap().to_string(),
                kind: format!("{:?}", lane.kind).to_lowercase(),
                label: lane.label.clone(),
                path: lane.evidence.path.to_string_lossy().to_string(),
                line: lane.evidence.line,
                snippet: lane.evidence.snippet.clone(),
            },
        );
        tiers.save(&self.tiers_path)?;
        Ok(json!({
            "path": obligations_path.display().to_string(),
            "obligation_id": obligation["id"],
            "lane": {"path": lane.evidence.path, "line": lane.evidence.line, "kind": format!("{:?}", lane.kind).to_lowercase()},
        }))
    }

    /// Verify a source reference by actually reading it. A claim that names a
    /// file it cannot open is rejected at the writer.
    /// PROBLEMS the core cannot measure: a verified claim whose cited range
    /// no longer resolves in the repository. Empty when every source holds.
    fn stale_claim_sources(&self, atlas: &atlas::Atlas) -> String {
        let stale: Vec<String> = atlas
            .claims
            .iter()
            .filter(|claim| claim.live() && claim.basis == "verified")
            .filter(|claim| {
                self.verify_source(&claim.path, &claim.lines, &claim.revision)
                    .is_err()
            })
            .map(|claim| {
                format!(
                    "- claim {} on \"{}\" is verified but its source no longer resolves [{}]\n",
                    claim.id,
                    atlas
                        .node(&claim.about)
                        .map(|node| node.label.as_str())
                        .unwrap_or(claim.about.as_str()),
                    claim.source_ref()
                )
            })
            .collect();
        if stale.is_empty() {
            return String::new();
        }
        format!(
            "\nPROBLEMS ({}) checked against the repository\n{}",
            stale.len(),
            stale.concat()
        )
    }

    fn verify_source(&self, path: &str, lines: &str, revision: &str) -> Result<(), String> {
        if path.trim().is_empty() {
            return Ok(());
        }
        let repo = self.repo()?;
        if !revision.trim().is_empty() {
            return verify_source_at_revision(repo, path, lines, revision);
        }
        match repo.read(path, lines) {
            Ok(_) => Ok(()),
            Err(error)
                if !lines.trim().is_empty()
                    && error.contains("larger than this surface will read") =>
            {
                verify_large_source_range(repo, path, lines)
            }
            Err(error) => Err(error),
        }
    }

    /// Whether nothing has been written yet, no claims, no relations, no
    /// marks, no drawing. The boot import in `main` fires only into an empty
    /// document, so restarting the server with `AGUI_ATLAS_IMPORT` still set
    /// cannot land the same diagram twice.
    pub fn is_empty(&self) -> Result<bool, String> {
        let atlas = self.read()?;
        Ok(atlas.nodes.is_empty()
            && atlas.edges.is_empty()
            && atlas.marks.is_empty()
            && atlas.shapes.is_empty()
            && atlas.constraints.is_empty())
    }

    /// Land an `.excalidraw` document from this repository onto the atlas as
    /// the agent's shapes, and report what the drawing turned out to MEAN,
    /// a diagram that lands on top of the map now says things about the cards
    /// underneath it that nobody intended.
    ///
    /// One code path for both ways of asking: the `atlas_import` action and
    /// the `AGUI_ATLAS_IMPORT` boot import in `main`.
    pub fn import_excalidraw(&self, path: &str, dx: f64, dy: f64) -> Result<String, String> {
        let document = self.repo()?.read_whole(path)?;
        let import = atlas::excalidraw::from_excalidraw(&document)?;
        let author = self.agent_author();
        let landed = {
            let _commit = self.begin_commit(author.as_str());
            let mut scene = self.scene.lock();
            atlas::excalidraw::land_import(&mut scene, import, &author, dx, dy)
        };
        self.persist(true);
        if landed.ids.is_empty() {
            return Err(format!(
                "nothing from {path} could land on the atlas: {}",
                landed.skipped.join("; ")
            ));
        }
        let atlas = self.read()?;
        let readings = atlas::readings(&atlas);
        let claims = landed
            .ids
            .iter()
            .filter_map(|id| readings.iter().find(|reading| &reading.shape == id))
            .filter(|reading| !reading.targets.is_empty())
            .map(|reading| format!("{} {}", reading.shape, reading.relation))
            .collect::<Vec<_>>();
        let mut summary = format!("imported {} shape(s) from {path}", landed.ids.len());
        if landed.bound_connectors > 0 {
            summary.push_str(&format!(
                ". Preserved bindings on {} connector(s)",
                landed.bound_connectors
            ));
        }
        if !claims.is_empty() {
            summary.push_str(&format!(
                ". Landed on top of the map: {}",
                claims.join(", ")
            ));
        }
        // Changed-on-the-way and not-imported are different sentences; a
        // loose arrow that is on the board must never be listed as absent.
        if !landed.notes.is_empty() {
            summary.push_str(&format!(
                ". Changed on the way in: {}",
                landed.notes.join("; ")
            ));
        }
        if !landed.skipped.is_empty() {
            summary.push_str(&format!(
                ". NOT imported ({}): {}",
                landed.skipped.len(),
                landed.skipped.join("; ")
            ));
        }
        Ok(summary)
    }

    /// Import one supported diagram format through the same path used by the
    /// boot variable and the live `atlas_import` action.
    pub fn import_file(&self, path: &str, dx: f64, dy: f64) -> Result<String, String> {
        if path.to_ascii_lowercase().ends_with(".json") {
            let document = self.repo()?.read_whole(path)?;
            return self.import_archify_document(path, &document, dx, dy);
        }
        self.import_excalidraw(path, dx, dy)
    }

    fn import_archify_document(
        &self,
        path: &str,
        document: &str,
        dx: f64,
        dy: f64,
    ) -> Result<String, String> {
        let plan = import_archify::parse(document, dx, dy)?;
        for source in plan.verified_sources() {
            self.verify_source(&source.path, &source.lines, &source.revision)
                .map_err(|error| {
                    format!(
                        "Archify node evidence {}:{}@{} does not hold: {error}",
                        source.path, source.lines, source.revision
                    )
                })?;
        }
        let author = self.agent_author();
        let imported = {
            let _commit = self.begin_commit(author.as_str());
            let mut scene = self.scene.lock();
            let encoded = scene
                .encode_full()
                .map_err(|error| format!("could not validate Archify import: {error}"))?;
            let mut probe = Scene::from_state(&encoded)
                .map_err(|error| format!("could not validate Archify import: {error}"))?;
            plan.land(&mut probe, &author)
                .map_err(|error| format!("Archify import refused before writing: {error}"))?;
            plan.land(&mut scene, &author)
                .map_err(|error| format!("Archify import stopped while writing: {error}"))?
        };
        self.persist(true);
        Ok(format!(
            "imported Archify {} {:?} from {path}: {} node(s), {} frame(s), {} relation(s), and {} claim(s)",
            imported.diagram_type,
            imported.title,
            imported.node_ids.len(),
            imported.frame_ids.len(),
            imported.relation_ids.len(),
            imported.claim_ids.len()
        ))
    }

    /// Turn semantic nodes and edges into the Atlas node register. The shared
    /// diagram engine validates and lays out the graph; Atlas contributes CRDT
    /// node ids, links, group constraints, and a collision-free origin.
    fn draw_diagram(&self, args: &JsonValue) -> Result<String, String> {
        let mut request = args.clone();
        request
            .as_object_mut()
            .ok_or_else(|| "atlas_diagram arguments must be an object".to_string())?
            .insert("clear".to_string(), JsonValue::Bool(false));
        let spec = diagram::Spec::parse(&request)?;
        let node_values = args
            .get("nodes")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| "atlas_diagram needs `nodes`".to_string())?;

        // The style and evidence each node authored, resolved and validated
        // for the whole batch before a single card lands.
        //
        // These four fields used to be "compatibility hints" that were checked
        // and then thrown away, which taught every agent that authored one
        // that the field worked. They are the node register's style vocabulary
        // now, so the check below decides what gets STORED rather than what
        // gets counted and discarded.
        let mut styles: Vec<atlas::NodePatch> = Vec::new();
        for (index, (node, value)) in spec.nodes.iter().zip(node_values).enumerate() {
            if node.label.chars().count() > atlas::MAX_LABEL {
                return Err(format!(
                    "diagram node {index} label is longer than {} characters",
                    atlas::MAX_LABEL
                ));
            }
            let kind = value
                .get("shape")
                .and_then(JsonValue::as_str)
                .unwrap_or(atlas::DEFAULT_KIND);
            let kind = match kind {
                "rect" | "rectangle" => "rect",
                "ellipse" => "ellipse",
                "diamond" => "diamond",
                other => {
                    return Err(format!(
                        "diagram node {index} shape must be rect, ellipse, or diamond; got {other:?}"
                    ));
                }
            };
            // An arbitrary hex snaps to the nearest palette name rather than
            // being stored raw, because the read-back hands the colour back as
            // the word the agent can reason about and the renderer only knows
            // the closed list.
            let color = match node.color.as_deref() {
                None => None,
                Some(color) if atlas::INKS.contains(&color) => Some(color.to_string()),
                Some(color) if color.starts_with('#') => {
                    Some(atlas::nearest_ink(color).to_string())
                }
                Some(color) => {
                    return Err(format!(
                        "diagram node {index} color must be a named Atlas ink or hex color; got {color:?}"
                    ));
                }
            };
            // `filled` is the older spelling of "draw this one heavy". It
            // still works and now does what it always looked like it did.
            // When a caller says both and they disagree, the write is refused:
            // guessing which of two explicit claims was meant is exactly the
            // silent normalisation this whole change is undoing.
            let filled = match value.get("filled") {
                None => None,
                Some(filled) => Some(
                    filled
                        .as_bool()
                        .ok_or_else(|| format!("diagram node {index} filled must be a boolean"))?,
                ),
            };
            let authored_emphasis = match value.get("emphasis") {
                None => None,
                Some(emphasis) => {
                    let emphasis = emphasis
                        .as_str()
                        .ok_or_else(|| format!("diagram node {index} emphasis must be a string"))?;
                    if !atlas::EMPHASES.contains(&emphasis) {
                        return Err(format!(
                            "diagram node {index} emphasis must be one of {}; got {emphasis:?}",
                            atlas::EMPHASES.join(", ")
                        ));
                    }
                    Some(emphasis.to_string())
                }
            };
            let emphasis = match (authored_emphasis, filled) {
                (Some(emphasis), Some(filled)) if filled != (emphasis == "strong") => {
                    return Err(format!(
                        "diagram node {index} says filled={filled} and emphasis={emphasis:?}, which disagree; drop `filled` and keep `emphasis`"
                    ));
                }
                (Some(emphasis), _) => Some(emphasis),
                (None, Some(true)) => Some("strong".to_string()),
                (None, Some(false)) => Some(atlas::DEFAULT_EMPHASIS.to_string()),
                (None, None) => None,
            };
            styles.push(atlas::NodePatch {
                path: optional_string(value, "path"),
                lines: optional_string(value, "lines"),
                note: optional_string(value, "note"),
                kind: Some(kind.to_string()),
                color,
                emphasis,
                size: Some(
                    match node.size {
                        diagram::NodeSize::Hero => "hero",
                        diagram::NodeSize::Primary => "primary",
                        diagram::NodeSize::Normal => atlas::DEFAULT_SIZE,
                    }
                    .to_string(),
                ),
                ..atlas::NodePatch::default()
            });
        }
        // Evidence is part of the atomic diagram, not a refinement pass. A
        // stale range refuses the whole graph before layout or mutation, and
        // the verified fields below are the same fields the browser inspector
        // later resolves through the host repository service.
        for (index, node) in styles.iter().enumerate() {
            if let Some(path) = &node.path {
                self.verify_source(path, node.lines.as_deref().unwrap_or_default(), "")
                    .map_err(|error| {
                        format!("diagram node {index} source reference does not hold: {error}")
                    })?;
            }
        }
        for (index, edge) in spec.edges.iter().enumerate() {
            if !edge.arrow {
                return Err(format!(
                    "diagram edge {index} is undirected, but node-register links are directed; use atlas_sketch for an undirected annotation"
                ));
            }
            if edge
                .label
                .as_deref()
                .is_some_and(|label| label.chars().count() > atlas::MAX_LABEL)
            {
                return Err(format!(
                    "diagram edge {index} label is longer than {} characters",
                    atlas::MAX_LABEL
                ));
            }
        }
        if spec
            .title
            .as_deref()
            .is_some_and(|title| title.chars().count() > atlas::MAX_LABEL)
        {
            return Err(format!(
                "diagram title is longer than {} characters",
                atlas::MAX_LABEL
            ));
        }

        let before = self.read()?;
        let existing = atlas::layout(&before).extent;
        let diagram_number = |key: &str| -> Result<Option<f64>, String> {
            match args.get(key) {
                Some(value) => value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(Some)
                    .ok_or_else(|| format!("atlas_diagram `{key}` must be a finite number")),
                None => Ok(None),
            }
        };
        let label_layout = spec.layout();
        let unit = diagram_unit(&label_layout);
        let origin_x = diagram_number("origin_x")?.unwrap_or_else(|| {
            existing
                .map(|(_, _, right, _)| right + DIAGRAM_TOP_GAP)
                .unwrap_or(80.0)
        });
        let origin_y = diagram_number("origin_y")?.unwrap_or(120.0);

        // Width comes from the shared label geometry, then height comes from
        // the Atlas card estimator the projection itself uses. Packing the
        // label-only rectangles would be a synthetic proof: the DOM card also
        // has a header, padding, and explicit line breaks, and those are the
        // boxes one Fit has to frame.
        let node_sizes = label_layout
            .nodes
            .iter()
            .zip(&spec.nodes)
            .zip(&styles)
            .map(|((placed, node), style)| {
                // Source cards carry a path and evidence text beyond the label.
                // Reserve readable width before estimating height and routing edges.
                let minimum_width = if style.path.as_deref().is_some_and(|path| !path.is_empty()) {
                    280.0
                } else {
                    120.0
                };
                let width = (placed.w * unit)
                    .max(unit * 1.5)
                    .clamp(minimum_width, 640.0);
                let size = style.size.as_deref().unwrap_or(atlas::DEFAULT_SIZE);
                (
                    width,
                    atlas::estimate_source_backed_diagram_height(
                        &node.label,
                        style.path.as_deref().unwrap_or_default(),
                        style.lines.as_deref().unwrap_or_default(),
                        style.note.as_deref().unwrap_or_default(),
                        width,
                        size,
                    ),
                )
            })
            .collect::<Vec<_>>();

        if spec.kind == diagram::DiagramKind::StateMachine {
            return self.draw_state_machine_spec(
                &spec,
                &styles,
                &node_sizes,
                origin_x,
                origin_y,
                &before,
            );
        }
        if args.get("containers").is_some() {
            return self.draw_explicit_hierarchy_spec(
                &spec,
                &styles,
                &node_sizes,
                origin_x,
                origin_y,
                &before,
            );
        }

        // Keep groups in first-appearance order. That order is authored and
        // stable, and it lets the top-level fit preserve the argument's read
        // order rather than alphabetising containers before laying them out.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        let mut group_of = vec![None; spec.nodes.len()];
        for (node_index, node) in spec.nodes.iter().enumerate() {
            let Some(group) = node.group.as_ref() else {
                continue;
            };
            if group.chars().count() > atlas::MAX_LABEL {
                return Err(format!(
                    "diagram group is longer than {} characters",
                    atlas::MAX_LABEL
                ));
            }
            let group_index = groups
                .iter()
                .position(|(label, _)| label == group)
                .unwrap_or_else(|| {
                    groups.push((group.clone(), Vec::new()));
                    groups.len() - 1
                });
            groups[group_index].1.push(node_index);
            group_of[node_index] = Some(group_index);
        }
        let additions = spec.nodes.len() + spec.edges.len() + groups.len();
        let current = before.nodes.len()
            + before.edges.len()
            + before.marks.len()
            + before.shapes.len()
            + before.constraints.len();
        if current + additions > atlas::MAX_OBJECTS {
            return Err(format!(
                "this diagram would put {} objects on an atlas that holds {}; remove something or use a smaller diagram",
                current + additions,
                atlas::MAX_OBJECTS
            ));
        }

        // A group becomes a container node, not a constraint. The difference
        // is the one gaps-r1 charged G5b for: a constraint is a claim ABOUT a
        // set that the set does not carry, so nothing the human drags, nothing
        // the read-back nests, and nothing a later tool asks "what is in this"
        // can see it. A parent pointer travels with the member.
        //
        // The container's own reference has to miss every diagram node id, or
        // a caller with a node called "Runtime" in a group called "Runtime"
        // would silently put that node inside itself.
        let group_reference = |label: &str| -> String {
            let mut candidate = format!("group:{label}");
            while spec.nodes.iter().any(|node| node.id == candidate) {
                candidate.insert_str(0, "group:");
            }
            candidate
        };

        // Fit every parent's real child rectangles first. No rectangle is
        // scaled: Atlas text has a fixed DOM font size, so the fit path chooses
        // rows and columns rather than shrinking boxes out from under labels.
        let group_fits = groups
            .iter()
            .enumerate()
            .map(|(group_index, (_, members))| {
                let sizes = members
                    .iter()
                    .map(|&index| node_sizes[index])
                    .collect::<Vec<_>>();
                // A two-card relation reads across. Larger subsystems use a
                // compact grid so an external connector has a lane to an
                // interior card instead of having to pass through every card
                // below it in a single stack.
                let child_viewport = if members.len() <= 2 {
                    DIAGRAM_PAIR_VIEWPORT
                } else {
                    DIAGRAM_CHILD_GRID_VIEWPORT
                };
                let local_edges = spec
                    .edges
                    .iter()
                    .filter(|edge| {
                        group_of[edge.from] == Some(group_index)
                            && group_of[edge.to] == Some(group_index)
                    })
                    .filter_map(|edge| {
                        Some((
                            members.iter().position(|&member| member == edge.from)?,
                            members.iter().position(|&member| member == edge.to)?,
                        ))
                    })
                    .collect::<Vec<_>>();
                let mut best: Option<(diagram::RectangleFit, usize)> = None;
                for order in diagram_candidate_orders(members.len()) {
                    let fit = diagram_fit_in_original_order(
                        &sizes,
                        child_viewport,
                        DIAGRAM_CHILD_GAP,
                        &order,
                    );
                    let rectangles = fit
                        .nodes
                        .iter()
                        .map(|node| (node.x, node.y, node.w, node.h))
                        .collect::<Vec<_>>();
                    let crossings = diagram_crossing_count(
                        &rectangles,
                        &[],
                        &vec![None; members.len()],
                        &local_edges,
                    );
                    let replace = best.as_ref().is_none_or(|(current, current_crossings)| {
                        crossings < *current_crossings
                            || (crossings == *current_crossings && fit.scale > current.scale + 1e-9)
                    });
                    if replace {
                        best = Some((fit, crossings));
                    }
                }
                best.expect("a non-empty group has a packing").0
            })
            .collect::<Vec<_>>();
        let group_sizes = group_fits
            .iter()
            .map(|fit| {
                (
                    fit.width + atlas::CONTAINER_PAD * 2.0,
                    fit.height + atlas::CONTAINER_PAD * 2.0 + atlas::CONTAINER_TITLE_HEIGHT,
                )
            })
            .collect::<Vec<_>>();

        #[derive(Clone, Copy)]
        enum TopItem {
            Node(usize),
            Group(usize),
        }
        let mut top_items = Vec::new();
        let mut seen_groups = vec![false; groups.len()];
        for (node_index, group_index) in group_of.iter().enumerate() {
            match group_index {
                Some(group_index) if !seen_groups[*group_index] => {
                    seen_groups[*group_index] = true;
                    top_items.push(TopItem::Group(*group_index));
                }
                Some(_) => {}
                None => top_items.push(TopItem::Node(node_index)),
            }
        }
        let top_sizes = top_items
            .iter()
            .map(|item| match *item {
                TopItem::Node(index) => node_sizes[index],
                TopItem::Group(index) => group_sizes[index],
            })
            .collect::<Vec<_>>();
        let edge_indices = spec
            .edges
            .iter()
            .map(|edge| (edge.from, edge.to))
            .collect::<Vec<_>>();

        struct PackingCandidate {
            fit: diagram::RectangleFit,
            rectangles: Vec<(f64, f64, f64, f64)>,
            group_frames: Vec<(f64, f64, f64, f64)>,
            crossings: usize,
            aspect_error: f64,
        }

        // Row count alone does not make a readable diagram. Try two routing
        // grids for the requested flow direction, then score every candidate
        // against that directional target. A readable candidate with clear
        // straight links wins before raw zoom. Larger diagrams get a bounded
        // one-swap neighbourhood from `diagram_candidate_orders`.
        let (fit_viewport, packing_viewports) = match spec.direction {
            diagram::Direction::Right => (
                DIAGRAM_FIT_VIEWPORT,
                [DIAGRAM_FIT_VIEWPORT, DIAGRAM_ROUTING_VIEWPORT],
            ),
            diagram::Direction::Down => (
                DIAGRAM_DOWN_FIT_VIEWPORT,
                [DIAGRAM_DOWN_FIT_VIEWPORT, DIAGRAM_DOWN_ROUTING_VIEWPORT],
            ),
        };
        let mut best: Option<PackingCandidate> = None;
        for packing_viewport in packing_viewports {
            for order in diagram_candidate_orders(top_items.len()) {
                let ordered_sizes = order
                    .iter()
                    .map(|&index| top_sizes[index])
                    .collect::<Vec<_>>();
                let fit =
                    diagram::fit_rectangles(&ordered_sizes, packing_viewport, DIAGRAM_TOP_GAP);
                let mut rectangles = vec![(0.0, 0.0, 0.0, 0.0); spec.nodes.len()];
                let mut group_frames = vec![(0.0, 0.0, 0.0, 0.0); groups.len()];
                for (&item_index, slot) in order.iter().zip(&fit.nodes) {
                    match top_items[item_index] {
                        TopItem::Node(index) => {
                            rectangles[index] =
                                (origin_x + slot.x, origin_y + slot.y, slot.w, slot.h);
                        }
                        TopItem::Group(group_index) => {
                            let frame = (origin_x + slot.x, origin_y + slot.y, slot.w, slot.h);
                            group_frames[group_index] = frame;
                            let local = &group_fits[group_index];
                            for (&node_index, child) in
                                groups[group_index].1.iter().zip(&local.nodes)
                            {
                                rectangles[node_index] = (
                                    frame.0 + atlas::CONTAINER_PAD + child.x,
                                    frame.1
                                        + atlas::CONTAINER_PAD
                                        + atlas::CONTAINER_TITLE_HEIGHT
                                        + child.y,
                                    child.w,
                                    child.h,
                                );
                            }
                        }
                    }
                }
                let crossings =
                    diagram_crossing_count(&rectangles, &group_frames, &group_of, &edge_indices);
                let viewport_aspect = fit_viewport.0 / fit_viewport.1;
                let aspect_error = ((fit.width / fit.height) / viewport_aspect).ln().abs();
                let candidate = PackingCandidate {
                    fit,
                    rectangles,
                    group_frames,
                    crossings,
                    aspect_error,
                };
                let replace = best.as_ref().is_none_or(|current| {
                    let display_scale = |fit: &diagram::RectangleFit| {
                        (fit_viewport.0 / fit.width)
                            .min(fit_viewport.1 / fit.height)
                            .min(1.0)
                    };
                    let candidate_scale = display_scale(&candidate.fit);
                    let current_scale = display_scale(&current.fit);
                    let candidate_readable = candidate_scale >= DIAGRAM_LEGIBLE_ZOOM;
                    let current_readable = current_scale >= DIAGRAM_LEGIBLE_ZOOM;
                    (candidate_readable && !current_readable)
                        || (candidate_readable == current_readable
                            && (candidate.crossings < current.crossings
                                || (candidate.crossings == current.crossings
                                    && (candidate_scale > current_scale + 1e-9
                                        || ((candidate_scale - current_scale).abs() <= 1e-9
                                            && candidate.aspect_error
                                                < current.aspect_error - 1e-9)))))
                });
                if replace {
                    best = Some(candidate);
                }
            }
        }
        let PackingCandidate {
            fit: top_fit,
            rectangles,
            group_frames,
            ..
        } = best.expect("a non-empty diagram has a top-level packing");
        let placed_bounds = (
            origin_x,
            origin_y,
            origin_x + top_fit.width,
            origin_y + top_fit.height,
        );
        if [
            placed_bounds.0,
            placed_bounds.1,
            placed_bounds.2,
            placed_bounds.3,
        ]
        .iter()
        .any(|value| value.abs() > atlas::WORLD_LIMIT)
        {
            return Err(format!(
                "diagram would leave the Atlas world limit of +/-{}; choose a nearer origin or a smaller graph",
                atlas::WORLD_LIMIT as i64
            ));
        }

        // Containers are written first so a child can resolve its parent by
        // batch reference. Their projected bounds are content-derived in the
        // core model, but landing the planned frame origin and width keeps the
        // transient node honest before the first child arrives too.
        let containers = groups
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let frame = group_frames[index];
                atlas::NodeDraft {
                    reference: Some(group_reference(label)),
                    patch: atlas::NodePatch {
                        label: Some(label.clone()),
                        x: Some(frame.0),
                        y: Some(frame.1),
                        w: Some(frame.2),
                        ..atlas::NodePatch::default()
                    },
                }
            })
            .collect::<Vec<_>>();
        let parent_of = |id: &str| -> Option<String> {
            let index = spec.nodes.iter().position(|node| node.id == id)?;
            group_of[index].map(|group_index| group_reference(&groups[group_index].0))
        };
        // Containers first: `atlas::draw` resolves a parent against what has
        // already landed in the same batch, so the owner has to be written
        // before the owned.
        let nodes = containers
            .into_iter()
            .chain(spec.nodes.iter().zip(&rectangles).zip(&styles).map(
                |((node, &(x, y, w, _h)), style)| atlas::NodeDraft {
                    reference: Some(node.id.clone()),
                    patch: atlas::NodePatch {
                        label: Some(node.label.clone()),
                        path: style.path.clone(),
                        lines: style.lines.clone(),
                        note: style.note.clone(),
                        x: Some(x),
                        y: Some(y),
                        w: Some(w),
                        parent: parent_of(&node.id),
                        kind: style.kind.clone(),
                        color: style.color.clone(),
                        emphasis: style.emphasis.clone(),
                        size: style.size.clone(),
                        ..atlas::NodePatch::default()
                    },
                },
            ))
            .collect::<Vec<_>>();
        let links = spec
            .edges
            .iter()
            .map(|edge| atlas::LinkDraft {
                from: spec.nodes[edge.from].id.clone(),
                to: spec.nodes[edge.to].id.clone(),
                label: edge.label.clone().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let container_references = groups
            .iter()
            .map(|(label, _)| group_reference(label))
            .collect::<Vec<_>>();

        let author = self.agent_author();
        let drawn = {
            let _commit = self.begin_commit(author.as_str());
            let mut scene = self.scene.lock();

            let encoded = scene
                .encode_full()
                .map_err(|error| format!("could not validate diagram: {error}"))?;
            let mut probe = Scene::from_state(&encoded)
                .map_err(|error| format!("could not validate diagram: {error}"))?;
            apply_diagram_plan(
                &mut probe,
                &nodes,
                &links,
                diagram::DiagramKind::Hierarchy,
                &container_references,
                &[],
                &[],
                &author,
            )
            .map_err(|error| format!("diagram refused before writing: {error}"))?;

            apply_diagram_plan(
                &mut scene,
                &nodes,
                &links,
                diagram::DiagramKind::Hierarchy,
                &container_references,
                &[],
                &[],
                &author,
            )
            .map_err(|error| format!("diagram stopped while writing: {error}"))?
        };
        self.persist(false);
        let mapping = drawn
            .nodes
            .iter()
            .filter_map(|(reference, id)| {
                reference
                    .as_ref()
                    .map(|reference| format!("{reference} -> {id}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        let mut reply = format!(
            "drew an editable diagram in the node register with {} node(s), {} link(s), and {} container(s) holding them: {mapping}",
            spec.nodes.len(),
            spec.edges.len(),
            groups.len()
        );
        if let Some(title) = &spec.title {
            reply.push_str(&format!(". Diagram title: {title}"));
        }
        // What the style register actually stored, counted from the patches
        // that were written rather than from the fields that arrived. The
        // sentence this replaces said the hints had been "normalized to shared
        // Atlas cards", which was a polite way of saying they were dropped.
        let styled = styles
            .iter()
            .filter(|style| {
                style.color.is_some()
                    || style
                        .kind
                        .as_deref()
                        .is_some_and(|kind| kind != atlas::DEFAULT_KIND)
                    || style
                        .emphasis
                        .as_deref()
                        .is_some_and(|emphasis| emphasis != atlas::DEFAULT_EMPHASIS)
                    || style
                        .size
                        .as_deref()
                        .is_some_and(|size| size != atlas::DEFAULT_SIZE)
            })
            .count();
        if styled > 0 {
            reply.push_str(&format!(
                ". Stored kind, colour, emphasis or size on {styled} node(s); say what your colours and emphasis MEAN when you present this, the atlas reads the words back and does not invent a meaning for them"
            ));
        }
        let source_backed = styles
            .iter()
            .filter(|node| node.path.as_deref().is_some_and(|path| !path.is_empty()))
            .count();
        if source_backed > 0 {
            reply.push_str(&format!(
                ". Verified and attached repository evidence to {source_backed} node(s) before anything landed"
            ));
        }
        reply.push_str(
            ". Use semantic_target_point for visible targets or semantic_target_reveal for a deliberate guided camera move",
        );
        Ok(reply)
    }

    fn draw_state_machine_spec(
        &self,
        spec: &diagram::Spec,
        styles: &[atlas::NodePatch],
        node_sizes: &[(f64, f64)],
        origin_x: f64,
        origin_y: f64,
        before: &atlas::Atlas,
    ) -> Result<String, String> {
        let current = before.nodes.len()
            + before.edges.len()
            + before.marks.len()
            + before.shapes.len()
            + before.constraints.len()
            + before.variables.len()
            + before.relations.len()
            + usize::from(before.explanation.is_some());
        let additions = spec.nodes.len() + spec.edges.len() + 1;
        if current + additions > atlas::MAX_OBJECTS {
            return Err(format!(
                "this diagram would put {} objects on an atlas that holds {}; remove something or use a smaller diagram",
                current + additions,
                atlas::MAX_OBJECTS
            ));
        }
        let title = spec
            .title
            .as_deref()
            .ok_or_else(|| "a state_machine diagram needs a non-empty `title`".to_string())?;
        let layout = diagram::layout_state_machine(
            node_sizes,
            &spec.edge_pairs(),
            spec.direction,
            DIAGRAM_CHILD_GAP,
        )?;
        let machine_width = layout.width + atlas::CONTAINER_PAD * 2.0;
        let machine_height =
            layout.height + atlas::CONTAINER_PAD * 2.0 + atlas::CONTAINER_TITLE_HEIGHT;
        if [
            origin_x,
            origin_y,
            origin_x + machine_width,
            origin_y + machine_height,
        ]
        .iter()
        .any(|value| value.abs() > atlas::WORLD_LIMIT)
        {
            return Err(format!(
                "diagram would leave the Atlas world limit of +/-{}; choose a nearer origin or a smaller graph",
                atlas::WORLD_LIMIT as i64
            ));
        }

        let mut machine_reference = "diagram:state-machine".to_string();
        while spec.nodes.iter().any(|node| node.id == machine_reference) {
            machine_reference.insert_str(0, "diagram:");
        }
        let mut nodes = vec![atlas::NodeDraft {
            reference: Some(machine_reference.clone()),
            patch: atlas::NodePatch {
                label: Some(title.to_string()),
                x: Some(origin_x),
                y: Some(origin_y),
                w: Some(machine_width),
                ..atlas::NodePatch::default()
            },
        }];
        nodes.extend(spec.nodes.iter().zip(&layout.nodes).zip(styles).map(
            |((node, placed), style)| atlas::NodeDraft {
                reference: Some(node.id.clone()),
                patch: atlas::NodePatch {
                    label: Some(node.label.clone()),
                    path: style.path.clone(),
                    lines: style.lines.clone(),
                    note: style.note.clone(),
                    x: Some(origin_x + atlas::CONTAINER_PAD + placed.x),
                    y: Some(
                        origin_y + atlas::CONTAINER_PAD + atlas::CONTAINER_TITLE_HEIGHT + placed.y,
                    ),
                    w: Some(placed.w),
                    parent: Some(machine_reference.clone()),
                    kind: style.kind.clone(),
                    color: style.color.clone(),
                    emphasis: style.emphasis.clone(),
                    size: style.size.clone(),
                    ..atlas::NodePatch::default()
                },
            },
        ));
        let state_roles = spec
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.initial, node.terminal))
            .collect::<Vec<_>>();
        let transitions = spec
            .edges
            .iter()
            .map(|edge| {
                Ok(PlannedTransition {
                    from: spec.nodes[edge.from].id.clone(),
                    to: spec.nodes[edge.to].id.clone(),
                    event: edge.event.clone().ok_or_else(|| {
                        "a state_machine transition lost its validated event".to_string()
                    })?,
                    guard: edge.guard.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let container_references = vec![machine_reference];
        let author = self.agent_author();
        let drawn = {
            let _commit = self.begin_commit(author.as_str());
            let mut scene = self.scene.lock();
            let encoded = scene
                .encode_full()
                .map_err(|error| format!("could not validate diagram: {error}"))?;
            let mut probe = Scene::from_state(&encoded)
                .map_err(|error| format!("could not validate diagram: {error}"))?;
            apply_diagram_plan(
                &mut probe,
                &nodes,
                &[],
                diagram::DiagramKind::StateMachine,
                &container_references,
                &state_roles,
                &transitions,
                &author,
            )
            .map_err(|error| format!("diagram refused before writing: {error}"))?;
            apply_diagram_plan(
                &mut scene,
                &nodes,
                &[],
                diagram::DiagramKind::StateMachine,
                &container_references,
                &state_roles,
                &transitions,
                &author,
            )
            .map_err(|error| format!("diagram stopped while writing: {error}"))?
        };
        self.persist(false);
        let mapping = drawn
            .nodes
            .iter()
            .filter_map(|(reference, id)| {
                reference
                    .as_ref()
                    .map(|reference| format!("{reference} -> {id}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        Ok(format!(
            "drew state machine {title:?} with {} state(s), {} transition(s), and one container: {mapping}. Use atlas_read to inspect the STATE MACHINE grammar and PROBLEMS",
            spec.nodes.len(),
            spec.edges.len()
        ))
    }

    fn draw_explicit_hierarchy_spec(
        &self,
        spec: &diagram::Spec,
        styles: &[atlas::NodePatch],
        node_sizes: &[(f64, f64)],
        origin_x: f64,
        origin_y: f64,
        before: &atlas::Atlas,
    ) -> Result<String, String> {
        for container in &spec.containers {
            if container.label.chars().count() > atlas::MAX_LABEL {
                return Err(format!(
                    "diagram container {:?} label is longer than {} characters",
                    container.id,
                    atlas::MAX_LABEL
                ));
            }
            if let Some(parent) = container.parent.as_deref() {
                let local = spec
                    .containers
                    .iter()
                    .any(|candidate| candidate.id == parent);
                if !local && before.node(parent).is_none() {
                    return Err(format!(
                        "diagram container {:?} names parent {parent:?}, which is neither a container in this call nor a node already on the atlas",
                        container.id
                    ));
                }
            }
            let has_child_container = spec
                .containers
                .iter()
                .any(|child| child.parent.as_deref() == Some(container.id.as_str()));
            let has_child_node = spec
                .nodes
                .iter()
                .any(|node| node.group.as_deref() == Some(container.id.as_str()));
            if !has_child_container && !has_child_node {
                return Err(format!(
                    "hierarchy container {:?} has no child node or container",
                    container.id
                ));
            }
        }
        let current = before.nodes.len()
            + before.edges.len()
            + before.marks.len()
            + before.shapes.len()
            + before.constraints.len()
            + before.variables.len()
            + before.relations.len()
            + usize::from(before.explanation.is_some());
        let additions = spec.nodes.len() + spec.edges.len() + spec.containers.len();
        if current + additions > atlas::MAX_OBJECTS {
            return Err(format!(
                "this diagram would put {} objects on an atlas that holds {}; remove something or use a smaller diagram",
                current + additions,
                atlas::MAX_OBJECTS
            ));
        }

        let container_index = |id: &str| {
            spec.containers
                .iter()
                .position(|container| container.id == id)
        };
        let node_parents = spec
            .nodes
            .iter()
            .map(|node| node.group.as_deref().and_then(container_index))
            .collect::<Vec<_>>();
        let container_parents = spec
            .containers
            .iter()
            .map(|container| container.parent.as_deref().and_then(container_index))
            .collect::<Vec<_>>();
        let absolute_container_depth = |index: usize| {
            let mut depth = 0usize;
            let mut at = index;
            while let Some(parent) = spec.containers[at].parent.as_deref() {
                if let Some(parent_index) = container_index(parent) {
                    depth += 1;
                    at = parent_index;
                } else {
                    depth += before.depth(parent) + 1;
                    break;
                }
            }
            depth
        };
        for (index, container) in spec.containers.iter().enumerate() {
            let depth = absolute_container_depth(index);
            if depth > atlas::HIERARCHY_MAX_DEPTH {
                return Err(format!(
                    "containment exceeds HIERARCHY_MAX_DEPTH {}: container {:?} would be at depth {depth}",
                    atlas::HIERARCHY_MAX_DEPTH,
                    container.id
                ));
            }
        }
        for node in &spec.nodes {
            if let Some(parent) = node.group.as_deref().and_then(container_index) {
                let depth = absolute_container_depth(parent) + 1;
                if depth > atlas::HIERARCHY_MAX_DEPTH {
                    return Err(format!(
                        "containment exceeds HIERARCHY_MAX_DEPTH {}: node {:?} would be at depth {depth}",
                        atlas::HIERARCHY_MAX_DEPTH,
                        node.id
                    ));
                }
            }
        }
        let fit_viewport = match spec.direction {
            diagram::Direction::Right => DIAGRAM_FIT_VIEWPORT,
            diagram::Direction::Down => DIAGRAM_DOWN_FIT_VIEWPORT,
        };
        let hierarchy = diagram::layout_hierarchy(
            node_sizes,
            &node_parents,
            &container_parents,
            fit_viewport,
            DIAGRAM_CHILD_GRID_VIEWPORT,
            DIAGRAM_CHILD_GAP,
            DIAGRAM_TOP_GAP,
            atlas::CONTAINER_PAD,
            atlas::CONTAINER_TITLE_HEIGHT,
        )?;
        if [
            origin_x,
            origin_y,
            origin_x + hierarchy.width,
            origin_y + hierarchy.height,
        ]
        .iter()
        .any(|value| value.abs() > atlas::WORLD_LIMIT)
        {
            return Err(format!(
                "diagram would leave the Atlas world limit of +/-{}; choose a nearer origin or a smaller graph",
                atlas::WORLD_LIMIT as i64
            ));
        }

        let container_references = spec
            .containers
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let mut reference = format!("diagram:container:{index}");
                while spec.nodes.iter().any(|node| node.id == reference) {
                    reference.insert_str(0, "diagram:");
                }
                reference
            })
            .collect::<Vec<_>>();
        let mut container_order = (0..spec.containers.len()).collect::<Vec<_>>();
        container_order.sort_by_key(|index| absolute_container_depth(*index));
        let mut nodes = container_order
            .iter()
            .map(|&index| {
                let container = &spec.containers[index];
                let frame = hierarchy.containers[index];
                let parent = container.parent.as_deref().map(|parent| {
                    container_index(parent).map_or_else(
                        || parent.to_string(),
                        |parent_index| container_references[parent_index].clone(),
                    )
                });
                atlas::NodeDraft {
                    reference: Some(container_references[index].clone()),
                    patch: atlas::NodePatch {
                        label: Some(container.label.clone()),
                        x: Some(origin_x + frame.x),
                        y: Some(origin_y + frame.y),
                        w: Some(frame.w),
                        parent,
                        ..atlas::NodePatch::default()
                    },
                }
            })
            .collect::<Vec<_>>();
        nodes.extend(spec.nodes.iter().zip(&hierarchy.nodes).zip(styles).map(
            |((node, placed), style)| {
                atlas::NodeDraft {
                    reference: Some(node.id.clone()),
                    patch: atlas::NodePatch {
                        label: Some(node.label.clone()),
                        path: style.path.clone(),
                        lines: style.lines.clone(),
                        note: style.note.clone(),
                        x: Some(origin_x + placed.x),
                        y: Some(origin_y + placed.y),
                        w: Some(placed.w),
                        parent: node
                            .group
                            .as_deref()
                            .and_then(container_index)
                            .map(|index| container_references[index].clone()),
                        kind: style.kind.clone(),
                        color: style.color.clone(),
                        emphasis: style.emphasis.clone(),
                        size: style.size.clone(),
                        ..atlas::NodePatch::default()
                    },
                }
            },
        ));
        let links = spec
            .edges
            .iter()
            .map(|edge| atlas::LinkDraft {
                from: spec.nodes[edge.from].id.clone(),
                to: spec.nodes[edge.to].id.clone(),
                label: edge.label.clone().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let author = self.agent_author();
        let drawn = {
            let _commit = self.begin_commit(author.as_str());
            let mut scene = self.scene.lock();
            let encoded = scene
                .encode_full()
                .map_err(|error| format!("could not validate diagram: {error}"))?;
            let mut probe = Scene::from_state(&encoded)
                .map_err(|error| format!("could not validate diagram: {error}"))?;
            apply_diagram_plan(
                &mut probe,
                &nodes,
                &links,
                diagram::DiagramKind::Hierarchy,
                &container_references,
                &[],
                &[],
                &author,
            )
            .map_err(|error| format!("diagram refused before writing: {error}"))?;
            apply_diagram_plan(
                &mut scene,
                &nodes,
                &links,
                diagram::DiagramKind::Hierarchy,
                &container_references,
                &[],
                &[],
                &author,
            )
            .map_err(|error| format!("diagram stopped while writing: {error}"))?
        };
        self.persist(false);
        let mapping = drawn
            .nodes
            .iter()
            .filter_map(|(reference, id)| {
                reference
                    .as_ref()
                    .map(|reference| format!("{reference} -> {id}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        Ok(format!(
            "drew a hierarchy with {} node(s), {} link(s), and {} nested container(s): {mapping}. Use atlas_read to inspect the HIERARCHY tree and PROBLEMS",
            spec.nodes.len(),
            spec.edges.len(),
            spec.containers.len()
        ))
    }
}

fn verify_source_at_revision(
    repo: &Repo,
    relative: &str,
    lines: &str,
    revision: &str,
) -> Result<(), String> {
    let revision = revision.trim();
    if revision.len() != 40
        || !revision
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!(
            "revision {revision:?} must be 40 hexadecimal characters, the full commit hash"
        ));
    }
    let relative = validate_revision_source_path(relative)?;
    let (start, end) = parse_source_range(lines)?;

    let commit = format!("{revision}^{{commit}}");
    let commit_check = Command::new("git")
        .arg("-C")
        .arg(repo.root())
        .args(["cat-file", "-e"])
        .arg(&commit)
        .output()
        .map_err(|error| format!("could not run git to verify revision {revision}: {error}"))?;
    if !commit_check.status.success() {
        return Err(format!(
            "revision {revision} is not a commit in this repository"
        ));
    }

    let object = format!("{revision}:{relative}");
    let source = Command::new("git")
        .arg("-C")
        .arg(repo.root())
        .args(["show", "--no-ext-diff", "--no-textconv"])
        .arg(&object)
        .output()
        .map_err(|error| {
            format!("could not run git to read {relative:?} at revision {revision}: {error}")
        })?;
    if !source.status.success() {
        return Err(format!(
            "path {relative:?} does not exist as a file at revision {revision}"
        ));
    }
    let body = String::from_utf8(source.stdout)
        .map_err(|_| format!("path {relative:?} at revision {revision} is not UTF-8 text"))?;
    let total = body.lines().count();
    if start > total {
        return Err(format!(
            "line {start} is past the end of {relative:?} at revision {revision} ({total} lines); the reference is stale"
        ));
    }
    if end > total {
        return Err(format!(
            "line range {lines:?} ends past the end of {relative:?} at revision {revision} ({total} lines); the reference is stale"
        ));
    }
    Ok(())
}

fn validate_revision_source_path(relative: &str) -> Result<String, String> {
    const DENIED_DIRS: &[&str] = &[
        ".git",
        ".local",
        ".claude",
        ".ssh",
        "target",
        "node_modules",
        "pkg",
    ];
    const DENIED_FILE_HINTS: &[&str] = &["secret", "credential", "password", "token", "auth.json"];
    const DENIED_EXTENSIONS: &[&str] = &[
        "pem", "key", "p12", "pfx", "crt", "der", "wasm", "png", "jpg", "jpeg", "gif", "webp",
        "ico", "pdf", "zip", "gz", "tar", "bin", "so", "dylib", "dll", "mp4", "mov", "woff",
        "woff2", "ttf",
    ];

    let relative = relative.trim().trim_start_matches("./");
    if relative.is_empty() {
        return Err("a revision-pinned source needs a repository-relative path".to_string());
    }
    if relative.starts_with('/') || relative.starts_with('~') || relative.contains('\\') {
        return Err(format!("{relative:?} must be a repository-relative path"));
    }
    if relative.chars().any(char::is_control) {
        return Err(format!("{relative:?} contains control characters"));
    }
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("{relative:?} must not contain . or .. segments"));
        }
        if DENIED_DIRS.contains(&segment) {
            return Err(format!("{segment:?} is not readable through this surface"));
        }
    }
    let lower = relative.to_ascii_lowercase();
    if DENIED_FILE_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Err(format!(
            "{relative:?} looks credential-shaped and is not readable through this surface"
        ));
    }
    if let Some(extension) = std::path::Path::new(relative)
        .extension()
        .and_then(|value| value.to_str())
    {
        let extension = extension.to_ascii_lowercase();
        if DENIED_EXTENSIONS.contains(&extension.as_str()) {
            return Err(format!(
                "{relative:?} is a {extension} file; only text sources are readable here"
            ));
        }
    }
    Ok(relative.to_string())
}

fn parse_source_range(lines: &str) -> Result<(usize, usize), String> {
    let lines = lines.trim();
    if lines.is_empty() {
        return Err("a revision-pinned source needs a line range".to_string());
    }
    let (start, end) = match lines.split_once('-') {
        Some((start, end)) => (start.trim(), end.trim()),
        None => (lines, lines),
    };
    let start = start
        .parse::<usize>()
        .map_err(|_| format!("line range {lines:?} is not N or N-M"))?;
    let end = end
        .parse::<usize>()
        .map_err(|_| format!("line range {lines:?} is not N or N-M"))?;
    if start == 0 || end < start {
        return Err(format!(
            "line range {lines:?} is not a forward 1-based range"
        ));
    }
    Ok((start, end))
}

fn verify_large_source_range(repo: &Repo, relative: &str, lines: &str) -> Result<(), String> {
    let lines = lines.trim();
    let (start, end) = parse_source_range(lines)?;

    let relative = relative.trim().trim_start_matches("./");
    let path = repo.root().join(relative);
    let canonical = path
        .canonicalize()
        .map_err(|_| format!("{relative:?} does not exist in this project"))?;
    if !canonical.starts_with(repo.root()) {
        return Err(format!("{relative:?} resolves outside the project"));
    }
    let file = std::fs::File::open(&canonical)
        .map_err(|error| format!("could not read {relative:?}: {error}"))?;
    let mut last_line = 0usize;
    for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
        line.map_err(|error| format!("could not read {relative:?}: {error}"))?;
        last_line = index + 1;
        if last_line >= end {
            return Ok(());
        }
    }
    if start > last_line {
        Err(format!(
            "line {start} is past the end of the file ({last_line} lines); the reference is stale"
        ))
    } else {
        Err(format!(
            "line range {lines:?} ends past the end of the file ({last_line} lines); the reference is stale"
        ))
    }
}

#[derive(Clone)]
struct PlannedTransition {
    from: String,
    to: String,
    event: String,
    guard: Option<String>,
}

fn drawn_reference<'a>(drawn: &'a atlas::Drawn, reference: &str) -> Result<&'a str, String> {
    drawn
        .nodes
        .iter()
        .find_map(|(candidate, id)| {
            (candidate.as_deref() == Some(reference)).then_some(id.as_str())
        })
        .ok_or_else(|| format!("diagram reference {reference:?} did not land"))
}

#[allow(clippy::too_many_arguments)]
fn apply_diagram_plan(
    scene: &mut Scene,
    nodes: &[atlas::NodeDraft],
    links: &[atlas::LinkDraft],
    diagram_kind: diagram::DiagramKind,
    container_references: &[String],
    state_roles: &[(String, bool, bool)],
    transitions: &[PlannedTransition],
    author: &Author,
) -> Result<atlas::Drawn, String> {
    let mut drawn = atlas::draw(scene, nodes, links, author)?;
    for reference in container_references {
        let id = drawn_reference(&drawn, reference)?.to_string();
        atlas::set_node_diagram_kind(scene, &id, diagram_kind.as_str(), author)?;
    }
    for (reference, initial, terminal) in state_roles {
        let id = drawn_reference(&drawn, reference)?.to_string();
        atlas::set_state_roles(scene, &id, Some(*initial), Some(*terminal), author)?;
    }
    for transition in transitions {
        let from = drawn_reference(&drawn, &transition.from)?.to_string();
        let to = drawn_reference(&drawn, &transition.to)?.to_string();
        let id = atlas::transition(
            scene,
            &from,
            &to,
            &transition.event,
            transition.guard.as_deref(),
            author,
        )?;
        drawn.edges.push(id);
    }
    Ok(drawn)
}

fn hierarchy_readback(atlas: &atlas::Atlas) -> String {
    fn describe_branch(out: &mut String, atlas: &atlas::Atlas, node: &atlas::Node, depth: usize) {
        out.push_str(&format!("{}- {}\n", "  ".repeat(depth), node.label));
        for child in atlas.children_of(&node.id) {
            describe_branch(out, atlas, child, depth + 1);
        }
    }

    let roots = atlas
        .nodes
        .iter()
        .filter(|node| {
            node.diagram_kind == "hierarchy"
                && atlas
                    .node(&node.parent)
                    .is_none_or(|parent| parent.diagram_kind != "hierarchy")
        })
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nHIERARCHY\n");
    for root in roots {
        describe_branch(&mut out, atlas, root, 0);
    }
    out
}

/// World pixels the renderer draws a shape label at.
///
/// Pinned to the renderer by a test rather than by this comment, because the
/// number lives in JavaScript and nothing else would notice it moving.
const DIAGRAM_LABEL_PX: f64 = 13.5;

/// Usable atlas pixels inside the real 1440x813 CSS-pixel round-2 browser:
/// the board and side panes have already been removed, as has Fit's 40px
/// margin on every side. Packing against the surface that actually displays
/// the diagram is what turns "viewport aspect" into a reproducible geometry
/// choice rather than a generic 16:9 guess.
const DIAGRAM_FIT_VIEWPORT: (f64, f64) = (606.0, 640.0);
const DIAGRAM_PAIR_VIEWPORT: (f64, f64) = (800.0, 200.0);
const DIAGRAM_CHILD_GRID_VIEWPORT: (f64, f64) = (700.0, 300.0);
const DIAGRAM_ROUTING_VIEWPORT: (f64, f64) = (1_200.0, 640.0);
/// Down-flow packing stays narrow enough that it cannot silently choose the
/// same row-first grid as a right-flow diagram.
const DIAGRAM_DOWN_FIT_VIEWPORT: (f64, f64) = (520.0, 1_200.0);
const DIAGRAM_DOWN_ROUTING_VIEWPORT: (f64, f64) = (520.0, 1_600.0);
const DIAGRAM_CHILD_GAP: f64 = 56.0;
const DIAGRAM_TOP_GAP: f64 = 96.0;
const DIAGRAM_LEGIBLE_ZOOM: f64 = 0.29;

/// Pack a candidate permutation, then put the returned slots back in the
/// caller's original node order.
///
/// `fit_rectangles` uses input order as row-major order. Diagram groups need
/// to try structural orders without changing the stable node order used by
/// ids, links, and read-back, so only the geometry is permuted.
fn diagram_fit_in_original_order(
    sizes: &[(f64, f64)],
    viewport: (f64, f64),
    gap: f64,
    order: &[usize],
) -> diagram::RectangleFit {
    let ordered_sizes = order.iter().map(|&index| sizes[index]).collect::<Vec<_>>();
    let fit = diagram::fit_rectangles(&ordered_sizes, viewport, gap);
    let mut nodes = vec![
        diagram::PackedRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        sizes.len()
    ];
    for (&original, &placed) in order.iter().zip(&fit.nodes) {
        nodes[original] = placed;
    }
    diagram::RectangleFit {
        nodes,
        width: fit.width,
        height: fit.height,
        scale: fit.scale,
    }
}

fn diagram_candidate_orders(count: usize) -> Vec<Vec<usize>> {
    fn visit(prefix: &mut Vec<usize>, remaining: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if remaining.is_empty() {
            out.push(prefix.clone());
            return;
        }
        for index in 0..remaining.len() {
            let value = remaining.remove(index);
            prefix.push(value);
            visit(prefix, remaining, out);
            prefix.pop();
            remaining.insert(index, value);
        }
    }

    let original = (0..count).collect::<Vec<_>>();
    if count <= 7 {
        let mut out = Vec::new();
        visit(&mut Vec::new(), &mut original.clone(), &mut out);
        return out;
    }

    // Above seven top-level items, exhaustive permutations stop being cheap.
    // The original authored order plus every one-swap neighbour gives the
    // scorer useful alternatives while keeping the authoring call bounded.
    let mut out = vec![original.clone()];
    for left in 0..count {
        for right in left + 1..count {
            let mut order = original.clone();
            order.swap(left, right);
            out.push(order);
        }
    }
    out
}

fn diagram_segment_hits_box(
    a: (f64, f64),
    b: (f64, f64),
    (left, top, right, bottom): (f64, f64, f64, f64),
) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut enter, mut exit) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-dx, a.0 - left),
        (dx, right - a.0),
        (-dy, a.1 - top),
        (dy, bottom - a.1),
    ] {
        if p.abs() < f64::EPSILON {
            if q < 0.0 {
                return false;
            }
            continue;
        }
        let ratio = q / p;
        if p < 0.0 {
            enter = enter.max(ratio);
        } else {
            exit = exit.min(ratio);
        }
        if enter > exit {
            return false;
        }
    }
    true
}

fn diagram_crossing_count(
    rectangles: &[(f64, f64, f64, f64)],
    group_frames: &[(f64, f64, f64, f64)],
    group_of: &[Option<usize>],
    edges: &[(usize, usize)],
) -> usize {
    let center = |(x, y, width, height): (f64, f64, f64, f64)| (x + width / 2.0, y + height / 2.0);
    let bounds = |(x, y, width, height): (f64, f64, f64, f64)| (x, y, x + width, y + height);
    let mut crossings = 0;
    for &(from, to) in edges {
        let start = center(rectangles[from]);
        let end = center(rectangles[to]);
        for (index, &rectangle) in rectangles.iter().enumerate() {
            if index != from
                && index != to
                && diagram_segment_hits_box(start, end, bounds(rectangle))
            {
                crossings += 1;
            }
        }
        for (group_index, &frame) in group_frames.iter().enumerate() {
            if group_of[from] != Some(group_index)
                && group_of[to] != Some(group_index)
                && diagram_segment_hits_box(start, end, bounds(frame))
            {
                crossings += 1;
            }
        }
    }
    crossings
}

/// World pixels per diagram layout unit.
///
/// The layout engine sizes every box to hold its own label at `text_size`
/// units and reports that size, so the unit that makes a box fit its text is
/// the one landing `text_size` on what the renderer will actually draw. The
/// room surface gets this for free by handing `layout.text_size` to its
/// renderer. Atlas shapes travel through the CRDT, which carries no per-shape
/// type size, so it solves for the unit instead of passing the size along.
///
/// This was a fixed 64, which put `text_size` at about 40px against a
/// renderer drawing 12px: every box came out more than three times the size
/// of the type inside it, and a diagram that should have fitted the pane was
/// wide enough to hit the zoom floor and crop.
fn diagram_unit(layout: &diagram::Layout) -> f64 {
    DIAGRAM_LABEL_PX / layout.text_size
}

/// One validated `/atlas/attention` report: viewport, dragged object id,
/// camera policy, zoom, and whether the pointing came from selection or the
/// composer.
type Attention = (
    atlas::ViewportRect,
    Option<String>,
    CameraPolicy,
    Option<f64>,
    AttentionSource,
    Option<RejectedGestureDraft>,
);

fn parse_attention(body: Option<JsonValue>) -> Result<Attention, String> {
    let object = body
        .as_ref()
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "attention request body must be a JSON object".to_string())?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "viewport" | "dragging" | "camera" | "zoom" | "source" | "rejectedGesture"
        )
    }) {
        return Err("attention request contains an unknown field".to_string());
    }
    let viewport = object
        .get("viewport")
        .and_then(JsonValue::as_object)
        .ok_or_else(|| "attention `viewport` must be a JSON object".to_string())?;
    if viewport
        .keys()
        .any(|key| !matches!(key.as_str(), "x" | "y" | "w" | "h"))
    {
        return Err("attention viewport contains an unknown field".to_string());
    }
    let number = |name: &str| {
        viewport
            .get(name)
            .and_then(JsonValue::as_f64)
            .ok_or_else(|| format!("attention viewport `{name}` must be a number"))
    };
    let viewport =
        atlas::ViewportRect::new(number("x")?, number("y")?, number("w")?, number("h")?)?;
    let dragging = match object.get("dragging") {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::String(id)) if !id.trim().is_empty() && id.chars().count() <= 160 => {
            Some(id.clone())
        }
        Some(JsonValue::String(_)) => {
            return Err("attention `dragging` must be a non-empty object id".to_string());
        }
        Some(_) => return Err("attention `dragging` must be a string or null".to_string()),
    };
    // An older page that does not send the field is reporting the behaviour it
    // still has, which is the default.
    let camera = match object.get("camera") {
        None | Some(JsonValue::Null) => CameraPolicy::default(),
        Some(JsonValue::String(value)) => CameraPolicy::parse(value)
            .ok_or_else(|| "attention `camera` must be follow, ask, or hold".to_string())?,
        Some(_) => return Err("attention `camera` must be a string".to_string()),
    };
    let zoom = match object.get("zoom") {
        None | Some(JsonValue::Null) => None,
        Some(value) => {
            let zoom = value
                .as_f64()
                .filter(|zoom| zoom.is_finite() && *zoom > 0.0)
                .ok_or_else(|| "attention `zoom` must be a positive number".to_string())?;
            Some(zoom)
        }
    };
    let source = match object.get("source") {
        None | Some(JsonValue::Null) => AttentionSource::default(),
        Some(JsonValue::String(value)) => AttentionSource::parse(value)
            .ok_or_else(|| "attention `source` must be selection or composer".to_string())?,
        Some(_) => return Err("attention `source` must be a string".to_string()),
    };
    let rejected_gesture = match object.get("rejectedGesture") {
        None | Some(JsonValue::Null) => None,
        Some(JsonValue::Object(rejected)) => {
            if rejected
                .keys()
                .any(|key| !matches!(key.as_str(), "key" | "target" | "reason"))
            {
                return Err("attention rejectedGesture contains an unknown field".to_string());
            }
            let key = rejected
                .get("key")
                .and_then(JsonValue::as_str)
                .filter(|key| matches!(*key, "?" | "!" | "*"))
                .ok_or_else(|| "attention rejectedGesture `key` must be ?, !, or *".to_string())?
                .to_string();
            let target = match rejected.get("target") {
                None | Some(JsonValue::Null) => None,
                Some(JsonValue::String(target))
                    if !target.trim().is_empty() && target.chars().count() <= 160 =>
                {
                    Some(target.clone())
                }
                Some(_) => {
                    return Err(
                        "attention rejectedGesture `target` must be a short id or null".to_string(),
                    );
                }
            };
            let reason = rejected
                .get("reason")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|reason| !reason.is_empty() && reason.chars().count() <= 240)
                .ok_or_else(|| {
                    "attention rejectedGesture `reason` must be 1 to 240 characters".to_string()
                })?
                .to_string();
            Some(RejectedGestureDraft {
                key,
                target,
                reason,
            })
        }
        Some(_) => {
            return Err("attention `rejectedGesture` must be an object".to_string());
        }
    };
    Ok((viewport, dragging, camera, zoom, source, rejected_gesture))
}

fn quoted_label(label: &str) -> String {
    json!(label).to_string()
}

fn labels_for(atlas: &atlas::Atlas, ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| atlas.node(id))
        .map(|node| quoted_label(&node.label))
        .collect()
}

fn human_attention_section(
    atlas: &atlas::Atlas,
    selection: Option<&SemanticAttention>,
    attention: Option<&HumanAttention>,
    rejected_gesture: Option<&RejectedGesture>,
) -> String {
    let mut lines = Vec::new();
    if let Some(selection) = selection {
        let selection_kind =
            if attention.is_some_and(|attention| attention.source == AttentionSource::Composer) {
                "Pointing (composer; self-reported; participantId is body-claimed)"
            } else {
                "Selected (self-reported; participantId is body-claimed)"
            };
        // One gesture, one sentence, count first. A marquee over three cards
        // is a single act of attention; reporting it as three lines would tell
        // the agent about three events that never happened, and reporting only
        // the first member would answer a question about the set with a fact
        // about one card.
        if selection.count() == 1 {
            lines.push(format!(
                "- {selection_kind}: {}.",
                quoted_label(&selection.target.label)
            ));
        } else {
            lines.push(format!(
                "- {selection_kind}: {} things, in the order they were selected: {}.",
                selection.count(),
                selection
                    .targets()
                    .into_iter()
                    .map(|target| quoted_label(&target.label))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(attention) = attention {
        let (left, top, right, bottom) = attention.viewport.bounds();
        let zoom = attention
            .zoom
            .map(|zoom| format!("zoom {zoom:.2}x"))
            .unwrap_or_else(|| "zoom not reported".to_string());
        lines.push(format!(
            "- Viewport observation #{} (age {} ms): world bounds ({left:.1},{top:.1}) to ({right:.1},{bottom:.1}), {zoom}.",
            attention.sequence,
            attention.observed_at.elapsed().as_millis()
        ));
        let reading = atlas::viewport_reading(atlas, attention.viewport);
        let inside = labels_for(atlas, &reading.inside);
        let partial = labels_for(atlas, &reading.partial);
        if inside.is_empty() && partial.is_empty() {
            lines.push(format!(
                "- Viewport: looking at an empty region from ({left:.1},{top:.1}) to ({right:.1},{bottom:.1}); no node intersects it."
            ));
        } else {
            if !inside.is_empty() {
                lines.push(format!(
                    "- Viewport: looking at the region holding {}.",
                    inside.join(", ")
                ));
            }
            if !partial.is_empty() {
                lines.push(format!(
                    "- Viewport edge: {} {} only partially in view.",
                    partial.join(", "),
                    if partial.len() == 1 { "is" } else { "are" }
                ));
            }
        }
        if let Some(node) = attention.dragging.as_deref().and_then(|id| atlas.node(id)) {
            lines.push(format!(
                "- Drag: currently dragging {}.",
                quoted_label(&node.label)
            ));
        }
        lines.push(format!("- {}", attention.camera.reading()));
    }
    if let Some(rejected) = rejected_gesture {
        let target = rejected
            .target
            .as_deref()
            .map(|target| format!(" for target {}", quoted_label(target)))
            .unwrap_or_else(|| " with no selected target".to_string());
        lines.push(format!(
            "- Rejected gesture {}{target} (age {} ms): {}. No mark was attached.",
            quoted_label(&rejected.key),
            rejected.observed_at.elapsed().as_millis(),
            rejected.reason
        ));
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("\nWHERE THE HUMAN IS\n{}\n", lines.join("\n"))
    }
}

// ── the live view both of us look at ──────────────────────────────────────

impl SurfaceState for AtlasState {
    fn activity_state_revision(&self) -> Result<Vec<ag_ui_surface::ActivityStateRevision>, String> {
        Ok(vec![ag_ui_surface::ActivityStateRevision::new(
            "document",
            self.document_revision()?,
        )])
    }

    fn backing(&self) -> StateBacking {
        StateBacking::Crdt
    }

    fn semantic_targets(&self) -> Option<&SemanticTargetService> {
        self.attention_host.get().map(Arc::as_ref)
    }

    fn describe(&self) -> Result<String, String> {
        Ok(self.read()?.describe())
    }

    fn snapshot(&self) -> Result<StateSnapshot, String> {
        let body = {
            let scene = self.scene.lock();
            let update = scene
                .encode_full()
                .map_err(|error| format!("the atlas is unavailable: {error}"))?;
            json!(base64::engine::general_purpose::STANDARD.encode(update))
        };
        Ok(StateSnapshot {
            backing: StateBacking::Crdt,
            body,
            chrome: Some(json!({
                "doc_schema_version": atlas::DOC_SCHEMA_VERSION,
            })),
        })
    }

    /// Deixis: the human clicks a node, then says "explain this".
    fn resolve(&self, id: &str) -> Result<Option<String>, String> {
        let atlas = self.read()?;
        if let Some(node) = atlas.node(id) {
            let source = if node.path.is_empty() {
                String::new()
            } else {
                format!(", drawn from {}", node.source_ref())
            };
            Ok(Some(format!(
                "the node \"{}\" on the shared atlas (tone {}, status {}{source})",
                node.label, node.tone, node.status
            )))
        } else {
            Ok(atlas.shape(id).map(|shape| {
                let name = if shape.label.is_empty() {
                    format!("{} {id}", shape.form)
                } else {
                    format!("{} \"{}\"", shape.form, shape.label)
                };
                format!(
                    "the {name} on the shared atlas, drawn by {}",
                    shape.created_by
                )
            }))
        }
    }

    fn semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        if target.extension_id != "atlas" {
            return Ok(None);
        }
        let atlas = self.read()?;
        if let Some(node) = atlas.node(&target.target_id) {
            let source = if node.path.is_empty() {
                String::new()
            } else {
                format!(", drawn from {}", node.source_ref())
            };
            Ok(Some(
                SemanticTarget::new(
                    target.clone(),
                    node.label.clone(),
                    format!(
                        "the node \"{}\" on the shared atlas (tone {}, status {}{source})",
                        node.label, node.tone, node.status
                    ),
                )
                .spatial(true),
            ))
        } else if let Some(constraint) = atlas.constraint(&target.target_id) {
            let label = constraint_literal(constraint);
            Ok(Some(
                SemanticTarget::new(
                    target.clone(),
                    label.clone(),
                    format!(
                        "the constraint {label} on the shared atlas, declared by {}",
                        constraint.created_by
                    ),
                )
                .spatial(true),
            ))
        } else {
            Ok(atlas.shape(&target.target_id).map(|shape| {
                let label = if shape.label.is_empty() {
                    format!("{} {}", shape.form, shape.id)
                } else {
                    shape.label.clone()
                };
                SemanticTarget::new(
                    target.clone(),
                    label.clone(),
                    format!(
                        "the {} \"{}\" on the shared atlas, drawn by {}",
                        shape.form, label, shape.created_by
                    ),
                )
                .spatial(true)
            }))
        }
    }

    fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
        let scene = self.scene.lock();
        let hello = greeting(&scene).map_err(|error| error.to_string())?;
        let full = scene.encode_full().map_err(|error| error.to_string())?;
        Ok(vec![
            encode_sync(&hello),
            encode_sync(&update_message(&full)),
        ])
    }

    fn ws_receive(&self, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let replies = match decode_frame(data).map_err(|error| format!("bad frame: {error}"))? {
            Frame::Sync(payload) => {
                let _commit = self.begin_commit("human");
                let scene = self.scene.lock();
                handle_payload(&scene, payload)
                    .map_err(|error| format!("bad sync payload from the browser: {error}"))?
            }
            // No bulk-geometry channel on this surface.
            _ => Vec::new(),
        };
        self.persist(false);
        Ok(replies.iter().map(|reply| encode_sync(reply)).collect())
    }
}

// ── the agent's vocabulary ────────────────────────────────────────────────

fn string(args: &JsonValue, key: &str) -> String {
    args.get(key)
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string()
}

fn optional_string(args: &JsonValue, key: &str) -> Option<String> {
    args.get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn optional_number(args: &JsonValue, key: &str) -> Option<f64> {
    args.get(key).and_then(JsonValue::as_f64)
}

fn number_quad(args: &JsonValue, key: &str) -> Result<[f64; 4], String> {
    let values = args
        .get(key)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{key} must be an array of four numbers"))?;
    if values.len() != 4 {
        return Err(format!("{key} must contain exactly four numbers"));
    }
    let mut out = [0.0; 4];
    for (index, value) in values.iter().enumerate() {
        out[index] = value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("{key}[{index}] must be a finite number"))?;
    }
    Ok(out)
}

fn optional_u64(args: &JsonValue, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be a non-negative integer when supplied")),
    }
}

fn wait_seconds(args: &JsonValue) -> Result<u64, String> {
    let seconds = optional_u64(args, "seconds")?.unwrap_or(DEFAULT_WAIT_SECONDS);
    if !(1..=MAX_WAIT_SECONDS).contains(&seconds) {
        return Err(format!("seconds must be between 1 and {MAX_WAIT_SECONDS}"));
    }
    Ok(seconds)
}

fn segment_motion_program_schema() -> JsonValue {
    let keyframe = json!({
        "type": "object",
        "properties": {
            "at": { "type": "number", "minimum": 0, "maximum": 1 },
            "x": { "type": "number", "minimum": -4000, "maximum": 4000 },
            "y": { "type": "number", "minimum": -4000, "maximum": 4000 },
            "rotate": { "type": "number", "minimum": -7200, "maximum": 7200 },
            "scale_x": { "type": "number", "minimum": 0.01, "maximum": 100 },
            "scale_y": { "type": "number", "minimum": 0.01, "maximum": 100 },
            "opacity": { "type": "number", "minimum": 0, "maximum": 1 }
        },
        "required": ["at"],
        "additionalProperties": false
    });
    let track = json!({
        "type": "object",
        "properties": {
            "label": { "type": "string", "minLength": 1, "maxLength": 160 },
            "target_ids": {
                "type": "array",
                "minItems": 1,
                "maxItems": 32,
                "items": { "type": "string", "minLength": 1 }
            },
            "duration_ms": { "type": "integer", "minimum": 50, "maximum": 120000 },
            "delay_ms": { "type": "integer", "minimum": 0, "maximum": 60000 },
            "stagger_ms": { "type": "integer", "minimum": 0, "maximum": 60000 },
            "loop": { "type": "boolean" },
            "alternate": { "type": "boolean" },
            "curve": {
                "type": "array",
                "prefixItems": [
                    { "type": "number", "minimum": 0, "maximum": 1 },
                    { "type": "number", "minimum": -10, "maximum": 10 },
                    { "type": "number", "minimum": 0, "maximum": 1 },
                    { "type": "number", "minimum": -10, "maximum": 10 }
                ],
                "minItems": 4,
                "maxItems": 4
            },
            "origin": {
                "type": "array",
                "items": { "type": "number", "minimum": 0, "maximum": 1 },
                "minItems": 2,
                "maxItems": 2
            },
            "keyframes": {
                "type": "array",
                "minItems": 2,
                "maxItems": 24,
                "description": "Ordered frames. The first at must be 0 and the last at must be 1.",
                "items": keyframe
            }
        },
        "required": ["label", "target_ids", "duration_ms", "keyframes"],
        "additionalProperties": false
    });
    json!({
        "type": "object",
        "description": "A bounded 2D keyframe program. Targets keep their saved Atlas positions while the browser renders the motion.",
        "properties": {
            "schema": { "type": "string", "const": atlas::SEGMENT_MOTION_SCHEMA },
            "label": { "type": "string", "minLength": 1, "maxLength": 160 },
            "child_compositing": {
                "type": "string",
                "enum": atlas::SEGMENT_CHILD_COMPOSITING,
                "description": "cutout removes moving descendants from the owner image; overlay preserves the clean owner image beneath animated infographic regions"
            },
            "tracks": {
                "type": "array",
                "minItems": 1,
                "maxItems": 16,
                "items": track
            }
        },
        "required": ["schema", "label", "tracks"],
        "additionalProperties": false
    })
}

fn explanation_flow_schema() -> JsonValue {
    let target_ids = json!({
        "type": "array",
        "maxItems": 32,
        "uniqueItems": true,
        "items": { "type": "string", "minLength": 1, "maxLength": 160 }
    });
    let evidence = json!({
        "type": "object",
        "properties": {
            "target_id": { "type": "string", "minLength": 1, "maxLength": 160 },
            "detail": { "type": "string", "minLength": 1, "maxLength": 1200 }
        },
        "required": ["target_id", "detail"],
        "additionalProperties": false
    });
    let action = json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": atlas::EXPLANATION_ACTION_KINDS },
            "target_id": { "type": "string", "minLength": 1, "maxLength": 160 },
            "cue_span": {
                "type": "array",
                "prefixItems": [
                    { "type": "integer", "minimum": 0 },
                    { "type": "integer", "minimum": 1 }
                ],
                "minItems": 2,
                "maxItems": 2
            }
        },
        "required": ["kind", "target_id"],
        "additionalProperties": false
    });
    let transition = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 160 },
            "label": { "type": "string", "minLength": 1, "maxLength": 140 },
            "control": {
                "type": "string",
                "enum": ["button"],
                "description": "Set to button only when this label is a complete learner answer. Clicking it commits the transition immediately and sends structured interaction context to the agent. Omit it for grading branches that require a free response."
            },
            "next": {
                "anyOf": [
                    { "type": "string", "minLength": 1, "maxLength": 160 },
                    { "type": "null" }
                ]
            },
            "target_ids": target_ids,
            "phrases": {
                "type": "array",
                "maxItems": 32,
                "uniqueItems": true,
                "items": { "type": "string", "minLength": 1, "maxLength": 140 }
            }
        },
        "required": ["id", "label"],
        "additionalProperties": false
    });
    let beat = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 160 },
            "title": { "type": "string", "minLength": 1, "maxLength": 140 },
            "intent": { "type": "string", "minLength": 1, "maxLength": 1200 },
            "cue": { "type": "string", "minLength": 1, "maxLength": 1200 },
            "evidence": {
                "type": "array",
                "minItems": 1,
                "maxItems": 16,
                "items": evidence
            },
            "actions": {
                "type": "array",
                "minItems": 1,
                "maxItems": 16,
                "items": action
            },
            "advance": {
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": atlas::EXPLANATION_ADVANCE_MODES },
                    "prompt": { "type": "string", "maxLength": 1200 },
                    "transitions": {
                        "type": "array",
                        "maxItems": 8,
                        "items": transition
                    }
                },
                "required": ["mode"],
                "additionalProperties": false
            }
        },
        "required": ["id", "title", "intent", "cue", "evidence", "actions", "advance"],
        "additionalProperties": false
    });
    json!({
        "type": "object",
        "properties": {
            "schema": { "type": "string", "const": atlas::EXPLANATION_FLOW_SCHEMA },
            "id": { "type": "string", "minLength": 1, "maxLength": 160 },
            "title": { "type": "string", "minLength": 1, "maxLength": 140 },
            "goal": { "type": "string", "minLength": 1, "maxLength": 4000 },
            "about": { "type": "array", "maxItems": 64, "items": { "type": "string" }, "description": "Existing semantic subjects this exploration is about. These remain on the blueprint." },
            "scene_ids": { "type": "array", "maxItems": 512, "items": { "type": "string" }, "description": "Explicit objects owned by this exploration, hidden from the blueprint and shown inside its workspace." },
            "start": { "type": "string", "minLength": 1, "maxLength": 160 },
            "beats": {
                "type": "array",
                "minItems": 1,
                "maxItems": 32,
                "items": beat
            }
        },
        "required": ["schema", "id", "title", "goal", "start", "beats"],
        "additionalProperties": false
    })
}

/// Read one node object out of a batch argument into the core's patch type.
fn node_patch(value: &JsonValue) -> atlas::NodePatch {
    atlas::NodePatch {
        id: optional_string(value, "id"),
        label: optional_string(value, "label"),
        path: optional_string(value, "path"),
        lines: optional_string(value, "lines"),
        note: optional_string(value, "note"),
        tone: optional_string(value, "tone"),
        status: optional_string(value, "status"),
        x: optional_number(value, "x"),
        y: optional_number(value, "y"),
        w: optional_number(value, "w"),
        // An empty string is a real request here, "move this back to the top
        // level", and it stays distinguishable from the absent case because
        // `optional_string` keys on the field being present, not on its length.
        parent: optional_string(value, "parent"),
        // Same "present, possibly empty" reading as `parent`: an empty colour
        // takes the card back out of every family.
        color: optional_string(value, "color"),
        emphasis: optional_string(value, "emphasis"),
        size: optional_string(value, "size"),
        kind: optional_string(value, "kind"),
    }
}

fn shape_patch(value: &JsonValue) -> atlas::ShapePatch {
    atlas::ShapePatch {
        id: optional_string(value, "id"),
        form: optional_string(value, "form"),
        x: optional_number(value, "x"),
        y: optional_number(value, "y"),
        w: optional_number(value, "w"),
        h: optional_number(value, "h"),
        points: optional_string(value, "points"),
        from: optional_string(value, "from"),
        to: optional_string(value, "to"),
        head: optional_string(value, "head"),
        ink: optional_string(value, "ink"),
        fill: optional_string(value, "fill"),
        label: optional_string(value, "label"),
        stroke_width: optional_number(value, "stroke_width"),
        stroke_style: optional_string(value, "stroke_style"),
        opacity: optional_number(value, "opacity"),
        roundness: optional_string(value, "roundness"),
        font_size: optional_number(value, "font_size"),
        angle: optional_number(value, "angle"),
        groups: optional_string(value, "groups"),
        reference: optional_string(value, "ref"),
        frame: optional_string(value, "frame"),
    }
}

#[derive(Clone, Debug)]
struct ConstraintInput {
    request_context: String,
    op: String,
    members: Vec<String>,
    axis: Option<String>,
    text: Option<String>,
    salience: Option<String>,
}

impl ConstraintInput {
    fn context(&self) -> &str {
        &self.request_context
    }
}

fn constraint_input(value: &JsonValue, index: usize) -> Result<ConstraintInput, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("constraints[{index}] must be an object"))?;
    let op = object
        .get("op")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("constraints[{index}] needs a string op"))?
        .to_string();
    let members = object
        .get("members")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("constraints[{index}] needs a members array"))?
        .iter()
        .enumerate()
        .map(|(member_index, member)| {
            member.as_str().map(str::to_string).ok_or_else(|| {
                format!("constraints[{index}].members[{member_index}] must be a string")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let optional = |key: &str| match object.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("constraints[{index}].{key} must be a string when supplied")),
    };
    Ok(ConstraintInput {
        request_context: format!("constraints[{index}] op={op:?} members={members:?}"),
        op,
        members,
        axis: optional("axis")?,
        text: optional("text")?,
        salience: optional("salience")?,
    })
}

fn constraint_inputs(args: &JsonValue) -> Result<Vec<ConstraintInput>, String> {
    let constraints = args
        .get("constraints")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "constraints must be an array".to_string())?;
    if constraints.is_empty() {
        return Err("constraints must contain at least one constraint".to_string());
    }
    constraints
        .iter()
        .enumerate()
        .map(|(index, value)| constraint_input(value, index))
        .collect()
}

fn string_ids(args: &JsonValue, key: &str) -> Result<Vec<String>, String> {
    let values = args
        .get(key)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("{key} must be an array"))?;
    if values.is_empty() {
        return Err(format!("{key} must contain at least one id"));
    }
    let ids = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{key}[{index}] must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
    if unique.len() != ids.len() {
        return Err(format!("{key} cannot contain duplicate ids"));
    }
    Ok(ids)
}

fn resolve_constraint_member(projection: &atlas::Atlas, member: &str) -> Result<String, String> {
    if projection.node(member).is_some()
        || projection.shape(member).is_some()
        || projection.constraint(member).is_some()
        || projection.edges.iter().any(|edge| edge.id == member)
        || projection.marks.iter().any(|mark| mark.id == member)
    {
        return Ok(member.to_string());
    }

    let mut matches = projection
        .nodes
        .iter()
        .filter(|node| node.label == member)
        .map(|node| node.id.clone())
        .chain(
            projection
                .shapes
                .iter()
                .filter(|shape| !shape.label.is_empty() && shape.label == member)
                .map(|shape| shape.id.clone()),
        )
        .collect::<Vec<_>>();
    matches.sort();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Ok(member.to_string()),
        _ => Err(format!(
            "member title {member:?} is ambiguous; it matches {}",
            matches.join(", ")
        )),
    }
}

fn resolve_constraint_input(
    projection: &atlas::Atlas,
    input: &ConstraintInput,
) -> Result<ConstraintInput, String> {
    let mut resolved = input.clone();
    resolved.members = input
        .members
        .iter()
        .map(|member| resolve_constraint_member(projection, member))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{} refused: {error}", input.context()))?;
    Ok(resolved)
}

fn constraint_literal(constraint: &atlas::Constraint) -> String {
    match constraint.op.as_str() {
        "sequence" => format!(
            "{} sequence[{}]: {}",
            constraint.id,
            constraint.axis.as_deref().unwrap_or("x"),
            constraint.members.join(" -> ")
        ),
        "group" => format!("{} group: {}", constraint.id, constraint.members.join(", ")),
        "attaches" => format!(
            "{} attaches: {}",
            constraint.id,
            constraint.members.join(" <-> ")
        ),
        "voids" => format!("{} voids: {}", constraint.id, constraint.members.join(", ")),
        "labels" => format!(
            "{} labels: {} {:?}",
            constraint.id,
            constraint.members.join(", "),
            constraint.text.as_deref().unwrap_or("")
        ),
        _ => format!(
            "{} {}: {}",
            constraint.id,
            constraint.op,
            constraint.members.join(", ")
        ),
    }
}

fn constraint_state_literal(state: &atlas::ConstraintState) -> String {
    if state.status == "unsat" {
        format!("constraint_state=UNSAT core=[{}]", state.core.join(", "))
    } else {
        "constraint_state=SAT".to_string()
    }
}

fn constrain(state: &AtlasState, inputs: Vec<ConstraintInput>) -> Result<String, String> {
    let author = state.agent_author();
    let ids = {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        let projection = atlas::read(&scene)?;
        let resolved = inputs
            .iter()
            .map(|input| resolve_constraint_input(&projection, input))
            .collect::<Result<Vec<_>, _>>()?;

        let encoded = scene
            .encode_full()
            .map_err(|error| format!("could not validate constraint batch: {error}"))?;
        let mut probe = Scene::from_state(&encoded)
            .map_err(|error| format!("could not validate constraint batch: {error}"))?;
        for input in &resolved {
            atlas::create_constraint(
                &mut probe,
                &input.op,
                &input.members,
                input.axis.as_deref(),
                input.text.as_deref(),
                input.salience.as_deref(),
                &author,
            )
            .map_err(|error| format!("{} refused: {error}", input.context()))?;
        }

        let mut ids = Vec::with_capacity(resolved.len());
        for input in &resolved {
            let id = atlas::create_constraint(
                &mut scene,
                &input.op,
                &input.members,
                input.axis.as_deref(),
                input.text.as_deref(),
                input.salience.as_deref(),
                &author,
            )
            .map_err(|error| format!("{} refused: {error}", input.context()))?;
            ids.push(id);
        }
        ids
    };
    state.persist(false);

    let projection = state.read()?;
    let mut stored = Vec::with_capacity(ids.len());
    for id in &ids {
        let constraint = projection
            .constraint(id)
            .ok_or_else(|| format!("constraint {id} vanished during the write"))?;
        stored.push(format!("- {}", constraint_literal(constraint)));
    }
    Ok(format!(
        "stored {} constraint(s):\n{}\n{}",
        stored.len(),
        stored.join("\n"),
        constraint_state_literal(&projection.constraint_state)
    ))
}

fn unconstrain(state: &AtlasState, ids: Vec<String>) -> Result<String, String> {
    let author = state.agent_author();
    let removed = {
        let _commit = state.begin_commit(author.as_str());
        let scene = state.scene.lock();
        let projection = atlas::read(&scene)?;
        let constraints =
            ids.iter()
                .enumerate()
                .map(|(index, id)| {
                    projection.constraint(id).cloned().ok_or_else(|| {
                        format!("ids[{index}] {id:?} is not a constraint on the atlas")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
        for constraint in &constraints {
            atlas::remove(&scene, &constraint.id)?;
        }
        constraints
    };
    state.persist(true);

    Ok(removed
        .iter()
        .map(|constraint| {
            format!(
                "removed {} created by {}",
                constraint_literal(constraint),
                constraint.created_by
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

// ── agent ink argument parsing and dispatch ────────────────────────────
//
struct VariableCreateInput {
    name: String,
    value: f64,
    state: String,
    represents: Option<(String, String)>,
    unit: Option<String>,
}

fn variable_create_input(args: &JsonValue) -> Result<VariableCreateInput, String> {
    let name = string(args, "name");
    let value =
        optional_number(args, "value").ok_or_else(|| "value must be a number".to_string())?;
    let state = string(args, "state");
    let represents_object = optional_string(args, "represents_object");
    let represents_property = optional_string(args, "represents_property");
    let represents =
        match (represents_object, represents_property) {
            (None, None) => None,
            (Some(object), Some(property)) => Some((object, property)),
            _ => return Err(
                "represents_object and represents_property must be given together or not at all"
                    .to_string(),
            ),
        };
    let unit = optional_string(args, "unit");
    Ok(VariableCreateInput {
        name,
        value,
        state,
        represents,
        unit,
    })
}

fn create_agent_ink_variable(
    state: &AtlasState,
    input: VariableCreateInput,
) -> Result<String, String> {
    let author = state.agent_author();
    {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::create_variable(
            &mut scene,
            &input.name,
            input.value,
            &input.state,
            input
                .represents
                .as_ref()
                .map(|(object, property)| (object.as_str(), property.as_str())),
            input.unit.as_deref(),
            &author,
        )
        .map_err(|error| format!("variable {:?} refused: {error}", input.name))?;
    }
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "created variable {:?}\n{}",
        input.name,
        agent_ink_readback(&atlas)
    ))
}

struct VariableMeasureCreateInput {
    name: String,
    measure_kind: String,
    objects: Vec<String>,
}

fn variable_measure_create_input(args: &JsonValue) -> Result<VariableMeasureCreateInput, String> {
    let name = string(args, "name");
    let measure_kind = string(args, "measure_kind");
    let objects = args
        .get("objects")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "objects must be an array".to_string())?
        .iter()
        .enumerate()
        .map(|(index, object)| {
            object
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("objects[{index}] must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(VariableMeasureCreateInput {
        name,
        measure_kind,
        objects,
    })
}

/// The host-side half of `atlas_variable_measure_create`: a measured
/// variable's value is never supplied by the caller, only derived from
/// `objects`' current geometry (see
/// `same_page_atlas_core::agent_ink::create_measured_variable`), so this
/// mirrors `create_agent_ink_variable` minus the `value`/`represents`/
/// `unit` fields that call takes and this one has no use for.
fn create_measured_agent_ink_variable(
    state: &AtlasState,
    input: VariableMeasureCreateInput,
) -> Result<String, String> {
    let author = state.agent_author();
    {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        let objects = input.objects.iter().map(String::as_str).collect::<Vec<_>>();
        atlas::agent_ink::create_measured_variable(
            &mut scene,
            &input.name,
            &input.measure_kind,
            &objects,
            &author,
        )
        .map_err(|error| format!("measured variable {:?} refused: {error}", input.name))?;
    }
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "created measured variable {:?}\n{}",
        input.name,
        agent_ink_readback(&atlas)
    ))
}

fn variable_set_input(args: &JsonValue) -> Result<(String, f64), String> {
    let name = string(args, "name");
    let value =
        optional_number(args, "value").ok_or_else(|| "value must be a number".to_string())?;
    Ok((name, value))
}

fn set_agent_ink_variable(state: &AtlasState, name: &str, value: f64) -> Result<String, String> {
    let author = state.agent_author();
    {
        let _commit = state.begin_commit(author.as_str());
        let scene = state.scene.lock();
        atlas::agent_ink::set_variable_value(&scene, name, value, &author)
            .map_err(|error| format!("could not set variable {name:?}: {error}"))?;
    }
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "set {name:?} to {value}\n{}",
        agent_ink_readback(&atlas)
    ))
}

fn variable_capture_input(args: &JsonValue) -> Result<(String, String, f64), String> {
    let object = string(args, "object");
    let property = string(args, "property");
    let value =
        optional_number(args, "value").ok_or_else(|| "value must be a number".to_string())?;
    Ok((object, property, value))
}

/// The host-side half of a human drag: `object`/`property` name a shape or
/// node field exactly the way `represents_object`/`represents_property` do
/// on `atlas_variable_create`, and `value` is wherever the drag left it.
/// This is `same_page_atlas_core::agent_ink::capture_represented_property`
/// plus the persistence and read-back every other agent-ink tool already
/// does; it does not itself run `atlas_solve` afterward.
fn capture_agent_ink_variable(
    state: &AtlasState,
    object: &str,
    property: &str,
    value: f64,
) -> Result<String, String> {
    let author = state.agent_author();
    let captured = {
        let _commit = state.begin_commit(author.as_str());
        let scene = state.scene.lock();
        atlas::agent_ink::capture_represented_property(&scene, object, property, value, &author)
            .map_err(|error| format!("could not capture {object:?}.{property:?}: {error}"))?
    };
    state.persist(false);
    let atlas = state.read()?;
    Ok(match captured {
        Some(name) => format!(
            "captured {object:?}.{property:?} into variable {name:?}\n{}",
            agent_ink_readback(&atlas)
        ),
        None => format!(
            "no agent-ink variable represents {object:?}.{property:?}; nothing captured\n{}",
            agent_ink_readback(&atlas)
        ),
    })
}

struct RelationCreateInput {
    name: String,
    op: String,
    members: Vec<String>,
    m: Option<f64>,
    b: Option<f64>,
    expression: Option<String>,
}

fn relation_create_input(args: &JsonValue) -> Result<RelationCreateInput, String> {
    let name = string(args, "name");
    let op = string(args, "op");
    let members = args
        .get("members")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "members must be an array".to_string())?
        .iter()
        .enumerate()
        .map(|(index, member)| {
            member
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("members[{index}] must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RelationCreateInput {
        name,
        op,
        members,
        m: optional_number(args, "m"),
        b: optional_number(args, "b"),
        expression: optional_string(args, "expression"),
    })
}

fn create_agent_ink_relation(
    state: &AtlasState,
    input: RelationCreateInput,
) -> Result<String, String> {
    let author = state.agent_author();
    let members = input.members.iter().map(String::as_str).collect::<Vec<_>>();
    {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::create_relation(
            &mut scene,
            &input.name,
            &input.op,
            &members,
            input.m,
            input.b,
            input.expression.as_deref(),
            &author,
        )
        .map_err(|error| format!("relation {:?} refused: {error}", input.name))?;
    }
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "created relation {:?}\n{}",
        input.name,
        agent_ink_readback(&atlas)
    ))
}

const AGENT_INK_MODES: &[&str] = &["learning", "alignment"];

fn agent_ink_mode_input(args: &JsonValue) -> Result<atlas::agent_ink::AgentInkMode, String> {
    match string(args, "mode").as_str() {
        "learning" => Ok(atlas::agent_ink::AgentInkMode::Learning),
        "alignment" => Ok(atlas::agent_ink::AgentInkMode::Alignment),
        other => Err(format!(
            "mode must be one of {}; got {other:?}",
            AGENT_INK_MODES.join(", ")
        )),
    }
}

fn set_agent_ink_mode(
    state: &AtlasState,
    mode: atlas::agent_ink::AgentInkMode,
) -> Result<String, String> {
    let author = state.agent_author();
    {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::set_agent_ink_mode(&mut scene, mode, &author)
            .map_err(|error| format!("could not set agent-ink mode: {error}"))?;
    }
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "agent-ink mode is now {:?}\n{}",
        mode.as_str(),
        agent_ink_readback(&atlas)
    ))
}

/// Promote the agent-ink timeline from learning mode into alignment mode.
///
/// Signed `Author::Human` directly rather than through `agent_author()`:
/// this tool is `.human_only()` (see `actions()` below), so every call that
/// reaches this function already came from a human caller by the time
/// `Caller::may_call` let it through `turn_loop::dispatch_tool` — there is
/// no named-participant concept for a person here the way `agent_author()`
/// has for an attached agent, so the plain byline is the honest one.
fn promote_agent_ink_to_alignment(state: &AtlasState) -> Result<String, String> {
    let index_variable = agent_ink_step_variable(state)?;
    let report = {
        let _commit = state.begin_commit(Author::Human.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::promote_to_alignment(&mut scene, &index_variable, &Author::Human)
            .map_err(|error| format!("could not promote the agent-ink timeline: {error}"))?
    };
    state.persist(false);
    let atlas = state.read()?;
    let steps = report
        .steps
        .iter()
        .map(|step| format!("{} ({})", step.index, step.status.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "promoted the agent-ink timeline to alignment mode; {} step(s) re-solved and \
         re-recorded under alignment semantics, promoted by {:?}: {}\n{}",
        report.steps.len(),
        report.promoted_by,
        steps,
        agent_ink_readback(&atlas)
    ))
}

fn run_agent_ink_solve(state: &AtlasState) -> Result<String, String> {
    let author = state.agent_author();
    {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::solve(&mut scene, &author)?;
    }
    state.persist(false);
    let atlas = state.read()?;
    Ok(agent_ink_readback(&atlas))
}

fn step_index_input(args: &JsonValue) -> Result<u32, String> {
    let index =
        optional_number(args, "index").ok_or_else(|| "index must be a whole number".to_string())?;
    if index < 0.0 || index.fract() != 0.0 || index > f64::from(u32::MAX) {
        return Err("index must be a whole number, 0 or greater".to_string());
    }
    Ok(index as u32)
}

fn record_agent_ink_step(state: &AtlasState) -> Result<String, String> {
    let author = state.agent_author();
    let index_variable = agent_ink_step_variable(state)?;
    let stored = {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::record_current_step(&mut scene, &index_variable, &author)?
    };
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "recorded step {}\n{}",
        stored.index,
        agent_ink_readback(&atlas)
    ))
}

fn advance_agent_ink_step(state: &AtlasState) -> Result<String, String> {
    let author = state.agent_author();
    let index_variable = agent_ink_step_variable(state)?;
    let stored = {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::advance_step(&mut scene, &index_variable, &author)?
    };
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "advanced to step {}\n{}",
        stored.index,
        agent_ink_readback(&atlas)
    ))
}

fn scrub_agent_ink_step(state: &AtlasState, index: u32) -> Result<String, String> {
    let author = state.agent_author();
    let index_variable = agent_ink_step_variable(state)?;
    let stored = {
        let _commit = state.begin_commit(author.as_str());
        let mut scene = state.scene.lock();
        atlas::agent_ink::scrub_step(&mut scene, &index_variable, index, &author)?
    };
    state.persist(false);
    let atlas = state.read()?;
    Ok(format!(
        "restored step {}\n{}",
        stored.index,
        agent_ink_readback(&atlas)
    ))
}

/// The existing host snapshot object carried by a `STATE_SNAPSHOT` event.
/// Reconstructing its CRDT body goes through the same projection as a live
/// `atlas_read`, so the diff does not need a second JSON-to-Atlas adapter.
#[derive(serde::Deserialize)]
struct AtlasDiffSnapshot {
    backing: String,
    body: JsonValue,
    chrome: JsonValue,
}

fn atlas_diff_snapshot(args: &JsonValue, key: &str) -> Result<atlas::Atlas, String> {
    let value = args
        .get(key)
        .ok_or_else(|| format!("{key} snapshot is required"))?;
    let value = match value.as_str() {
        Some(encoded) => serde_json::from_str(encoded)
            .map_err(|error| format!("{key} snapshot is not valid StateSnapshot JSON: {error}"))?,
        None => value.clone(),
    };
    let snapshot: AtlasDiffSnapshot = serde_json::from_value(value)
        .map_err(|error| format!("{key} snapshot is not a host StateSnapshot object: {error}"))?;
    if snapshot.backing != "crdt" {
        return Err(format!(
            "{key} snapshot backing must be \"crdt\", got {:?}",
            snapshot.backing
        ));
    }
    let schema = snapshot
        .chrome
        .get("doc_schema_version")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| format!("{key} snapshot has no Atlas document schema version"))?;
    if schema != u64::from(atlas::DOC_SCHEMA_VERSION) {
        return Err(format!(
            "{key} snapshot uses Atlas document schema {schema}, but this host requires {}",
            atlas::DOC_SCHEMA_VERSION
        ));
    }
    let body = snapshot
        .body
        .as_str()
        .ok_or_else(|| format!("{key} snapshot body must be a base64 string"))?;
    let update = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|error| format!("{key} snapshot body is not valid base64: {error}"))?;
    let scene = Scene::from_state(&update)
        .map_err(|error| format!("{key} snapshot is not a valid Atlas CRDT state: {error}"))?;
    atlas::read(&scene).map_err(|error| format!("{key} snapshot cannot be projected: {error}"))
}

/// Build the citable-export document for one alignment-mode step: exactly
/// what `same_page_atlas_core::agent_ink::citable_step` reports, wrapped
/// with the two facts only the host knows about — which document this is
/// (the state file `citable_step` itself has no notion of) and a `source`
/// citation in the same `{file, lines, sha256}` shape Govern's own compiled
/// obligations use throughout `specs/obligations-v0.1.json` and this crate's
/// own `cement.rs` already emits for cemented assertions. Reusing that shape
/// here is not the same as integrating with Govern's receipt machinery —
/// see this module's doc comment and the engineering report for why a
/// agent-ink step does not fit Govern's `{meta, actions[]}` action-receipt
/// convention, and what a real connection would still need.
fn citable_agent_ink_step_document(state: &AtlasState, index: u32) -> Result<JsonValue, String> {
    let citable = {
        let scene = state.scene.lock();
        atlas::agent_ink::citable_step(&scene, index)?
    };
    let document = state.state_path.display().to_string();
    let step = serde_json::to_value(&citable)
        .map_err(|error| format!("could not serialize citable step {index}: {error}"))?;
    Ok(json!({
        "schema_version": 1,
        "kind": "same-page-atlas-agent-ink-step-citable",
        "document": document,
        "step": step,
        "source": {
            "file": document,
            "lines": format!("/agent_ink/step/{index}"),
            "sha256": citable.citable_id,
        },
    }))
}

fn export_citable_agent_ink_step(state: &AtlasState, index: u32) -> Result<String, String> {
    let document = citable_agent_ink_step_document(state, index)?;
    serde_json::to_string_pretty(&document)
        .map_err(|error| format!("could not render citable step {index}: {error}"))
}

/// Pull the settled VARIABLES section back out of `Atlas::describe()`.
///
/// `agent_ink::describe` itself is `pub(crate)` inside
/// `same_page_atlas_core`, so it cannot be called from this crate. Rather
/// than re-implement its formatting here (and risk it drifting from the
/// settled semantics), this slices the same text the model already reads in
/// `atlas_read`, between the "VARIABLES" header agent_ink writes and
/// whichever of the DRAWING or LAYOUT sections that always follow it in
/// `Atlas::describe`. Empty when there are no agent-ink variables, which is
/// exactly when `agent_ink::describe` itself returns "".
fn agent_ink_readback(atlas: &atlas::Atlas) -> String {
    let full = atlas.describe();
    let Some(header) = full.find("\nVARIABLES\n") else {
        return String::new();
    };
    let start = header + 1;
    let rest = &full[start..];
    let end = ["\nDRAWING", "\nLAYOUT\n"]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .min()
        .unwrap_or(rest.len());
    rest[..end].trim_end().to_string()
}

fn agent_ink_step_variable(state: &AtlasState) -> Result<String, String> {
    let atlas = state.read()?;
    atlas::agent_ink::step_index_variable(&atlas)?
        .map(|variable| variable.name.clone())
        .ok_or_else(|| "no variable is in the scrubbing state".to_string())
}

/// The `/atlas/agent-ink` route's `step` summary: the current value of the
/// unique scrubbing variable (0 when it does not exist yet) and how many steps have
/// been stored contiguously from step 0. `advance_step` only ever fills the
/// next unused index, so a contiguous run from 0 is what the timeline looks
/// like unless something scrubbed to a step that was never recorded, which
/// `scrub_step` itself refuses.
fn agent_ink_step_summary(scene: &Scene, atlas: &atlas::Atlas) -> Result<(i64, u32), String> {
    let index = atlas::agent_ink::step_index_variable(atlas)?
        .map(|variable| variable.value as i64)
        .unwrap_or(0);
    let mut count = 0u32;
    while count < atlas::agent_ink::MAX_STORED_STEPS
        && atlas::agent_ink::stored_step(scene, count).is_ok()
    {
        count += 1;
    }
    Ok((index, count))
}

fn verify_cement_sources(
    state: &AtlasState,
    atlas: &atlas::Atlas,
    ids: &[String],
) -> Result<(), String> {
    for claim in atlas.claims.iter().filter(|claim| {
        ids.contains(&claim.about)
            && claim.live()
            && claim.verdict == "accepted"
            && claim.basis == "verified"
    }) {
        state
            .verify_source(&claim.path, &claim.lines, &claim.revision)
            .map_err(|error| {
                format!(
                    "cannot cement verified claim {} on {:?}; its source does not hold: {error}",
                    claim.id,
                    atlas
                        .node(&claim.about)
                        .map(|node| node.label.as_str())
                        .unwrap_or(claim.about.as_str())
                )
            })?;
    }
    Ok(())
}

fn propose_cement_decision(
    state: &AtlasState,
    ids: &[String],
    draft_id: &str,
    human_preview: bool,
) -> Result<String, String> {
    let (atlas, revision) = state.read_with_revision()?;
    verify_cement_sources(state, &atlas, ids)?;
    let cemented_at = crate::timestamp::now_iso()?;
    let bundle = cement::propose_reviewed_at(
        &atlas,
        ids,
        draft_id,
        state.repo()?.root(),
        &revision,
        &cemented_at,
    )?;
    let preview = bundle.preview()?;
    if human_preview {
        let mut previews = state.decision_previews.lock();
        if previews.len() >= 16 {
            previews.pop_first();
        }
        previews.insert(bundle.review_token().into(), bundle);
    }
    Ok(preview)
}

fn cement_decision(
    state: &AtlasState,
    ids: &[String],
    directory: &std::path::Path,
    draft_id: &str,
    review_token: &str,
) -> Result<String, String> {
    let target_directory = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        state.repo()?.root().join(directory)
    };
    let directory = target_directory.as_path();
    let bundle = state
        .decision_previews
        .lock()
        .remove(review_token)
        .ok_or("a current human preview is required before cementing")?;
    let (atlas, revision) = state.read_with_revision()?;
    if bundle.reviewed_revision() != revision || bundle.reviewed_draft() != draft_id {
        return Err("the page changed after the human preview; review it again".into());
    }
    let content = atlas::claims::cement_content(&atlas, ids)?;
    verify_cement_sources(state, &atlas, ids)?;
    cement::propose_reviewed_at(
        &atlas,
        ids,
        draft_id,
        state.repo()?.root(),
        &revision,
        bundle.review_time(),
    )?
    .require_freshness(review_token)?;
    let written = bundle.write_to_new_directory(directory)?;
    let adr_path = bundle.adr_relative_path().to_path_buf();
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/samepage-g8");
    let adoption = std::process::Command::new("python3")
        .arg(adapter)
        .arg(directory)
        .arg("--repo")
        .arg(state.repo()?.root())
        .output()
        .map_err(|e| format!("could not run G8 adoption: {e}"))?;
    if !adoption.status.success() {
        let detail = String::from_utf8_lossy(&adoption.stderr);
        return Err(format!(
            "G8 adoption failed. The reviewed bundle remains at {} for inspection. {detail}",
            directory.display()
        ));
    }
    let result: JsonValue = serde_json::from_slice(&adoption.stdout)
        .map_err(|e| format!("G8 adoption completed but its report is unreadable: {e}"))?;
    let commit = state.begin_commit(Author::Human.as_str());
    let scene = state.scene.lock();
    let recorded = (|| {
        let current_revision = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(scene.state_vector_v1().map_err(|e| e.to_string())?);
        if current_revision != revision {
            return Err("the page changed while G8 was adopting the reviewed decision".to_string());
        }
        atlas::decision::reviewed(&atlas::read(&scene)?, draft_id, ids)?;
        bundle.verify_adoption_files(state.repo()?.root(), &result)?;
        atlas::claims::record_cemented_gate_checked(
            &scene,
            ids,
            &path_text(&adr_path),
            &revision,
            &content,
            result["status"]
                .as_str()
                .ok_or("G8 adoption did not report its status")?,
        )
    })();
    if let Err(error) = recorded {
        drop(scene);
        drop(commit);
        let rollback = std::process::Command::new("python3")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/samepage-g8"))
            .arg(directory)
            .arg("--repo")
            .arg(state.repo()?.root())
            .arg("--rollback")
            .output()
            .map_err(|e| format!("{error}; G8 rollback could not run: {e}"))?;
        return Err(format!(
            "{error}. G8 rollback {}. Retained the review bundle at {}. {}",
            if rollback.status.success() {
                "completed"
            } else {
                "needs reconciliation"
            },
            directory.display(),
            String::from_utf8_lossy(&rollback.stderr)
        ));
    }
    drop(scene);
    drop(commit);
    state.persist(true);
    Ok(format!(
        "Decision recorded as {}. G8 checks are active{}; full results are in {}/g8-adoption.json. Structural checks do not prove behavior.",
        written[0].display(),
        if result["g8_exit_code"] == 0 {
            " and passing"
        } else {
            ", with outstanding failures blocking enforcement"
        },
        directory.display()
    ))
}

fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn actions(state: &Arc<AtlasState>) -> Vec<ToolDef> {
    let tones = atlas::TONES
        .iter()
        .map(|tone| JsonValue::from(*tone))
        .collect::<Vec<_>>();
    let statuses = atlas::STATUSES
        .iter()
        .map(|status| JsonValue::from(*status))
        .collect::<Vec<_>>();
    let forms = atlas::FORMS
        .iter()
        .map(|form| JsonValue::from(*form))
        .collect::<Vec<_>>();
    let inks = atlas::INKS
        .iter()
        .map(|ink| JsonValue::from(*ink))
        .collect::<Vec<_>>();
    let heads = atlas::HEADS
        .iter()
        .map(|head| JsonValue::from(*head))
        .collect::<Vec<_>>();
    let fills = std::iter::once("none")
        .chain(atlas::INKS.iter().copied())
        .map(JsonValue::from)
        .collect::<Vec<_>>();
    let stroke_styles = atlas::STROKE_STYLES
        .iter()
        .map(|style| JsonValue::from(*style))
        .collect::<Vec<_>>();
    let roundness = atlas::ROUNDNESS
        .iter()
        .map(|value| JsonValue::from(*value))
        .collect::<Vec<_>>();
    let constraint_ops = atlas::CONSTRAINT_OPS
        .iter()
        .map(|op| JsonValue::from(*op))
        .collect::<Vec<_>>();
    let constraint_axes = atlas::CONSTRAINT_AXES
        .iter()
        .map(|axis| JsonValue::from(*axis))
        .collect::<Vec<_>>();
    let constraint_saliences = atlas::CONSTRAINT_SALIENCES
        .iter()
        .map(|salience| JsonValue::from(*salience))
        .collect::<Vec<_>>();
    let node_kinds = atlas::NODE_KINDS
        .iter()
        .map(|kind| JsonValue::from(*kind))
        .collect::<Vec<_>>();
    let emphases = atlas::EMPHASES
        .iter()
        .map(|emphasis| JsonValue::from(*emphasis))
        .collect::<Vec<_>>();
    let sizes = atlas::SIZES
        .iter()
        .map(|size| JsonValue::from(*size))
        .collect::<Vec<_>>();
    let diagram_kinds = atlas::DIAGRAM_KINDS
        .iter()
        .map(|kind| JsonValue::from(*kind))
        .collect::<Vec<_>>();
    // Empty is a real value here, "no category", so it is in the enum rather
    // than being a documented exception the schema would reject.
    let node_colors = std::iter::once(JsonValue::from(""))
        .chain(atlas::INKS.iter().map(|ink| JsonValue::from(*ink)))
        .collect::<Vec<_>>();
    // One node's fields, shared by the single-node and batch actions so the
    // two can never describe different shapes of the same thing.
    let node_properties = json!({
        "id": { "type": "string", "description": "existing node id; omit to create" },
        "label": { "type": "string", "description": "short title, required when creating" },
        "path": { "type": "string", "description": "repository-relative source this node is about" },
        "lines": { "type": "string", "description": "N or N-M within `path`" },
        "note": { "type": "string", "description": "what you claim about it, in your own words" },
        "tone": { "type": "string", "enum": tones.clone() },
        "status": { "type": "string", "enum": statuses.clone() },
        // Positions are the one thing the model has historically got wrong,
        // because it was handed two bare numbers and no notion of how big a
        // card is. Say what the units are, how tall a node gets, and what
        // happens if it declines to choose.
        "x": {
            "type": "number",
            "description": "left edge, world units, x grows right. Omit x and y together and the atlas grids the node into the first free slot, prefer that unless you are deliberately composing a shape."
        },
        "y": {
            "type": "number",
            "description": "top edge, world units, y grows DOWN. A card is `w` wide and as tall as its own text: about 80px bare, 230 to 300px with a two-sentence note and a source, more for a longer note. Leave a 320px gap of empty space between rows of noted cards and a 300px gap between columns (so a column pitch of about 530px at the default width), and read the LAYOUT section of atlas_read afterwards to see what it actually came out as."
        },
        "w": { "type": "number", "description": "card width, 120-640, default 232. Wider cards are shorter." },
        "parent": {
            "type": "string",
            "description": "id of the node this one goes INSIDE. Real containment, not a drawn box and not a group constraint: atlas_read lists this node indented under its container, links across levels read as 'X (inside PARENT) -> Y', and deleting the container moves this node up one level instead of deleting it. Containment stays a tree, so a parent that is already inside this node is refused. Pass an empty string to move it back to the top level."
        },
        "kind": {
            "type": "string",
            "enum": node_kinds.clone(),
            "description": "what this card IS, as a form the human can tell apart at a glance. Default rect. Shape is yours to assign meaning to and to say out loud: the usual reading is rect for a step, diamond for a decision or branch, ellipse for a boundary or endpoint. The atlas stores and renders what you pick and never decides what it means."
        },
        "emphasis": {
            "type": "string",
            "enum": emphases.clone(),
            "description": "how much this card matters, as visual weight. Default normal. Use strong for the few things the argument turns on and muted for context the reader may skip. Emphasis is the answer to 'make the riskiest part stand out'; do not encode importance by making a card physically bigger, that is `size`."
        },
        "size": {
            "type": "string",
            "enum": sizes.clone(),
            "description": "importance as a size class, default normal. Separate from `emphasis` on purpose: emphasis is weight, size is footprint. Make one anchor hero and use primary sparingly. This is the authored claim, not the geometry; `w` is what the card actually got."
        },
        "color": {
            "type": "string",
            "enum": node_colors.clone(),
            "description": "category colour from the shared palette. There is no default: a card with no colour belongs to no family, and an empty string puts it back that way. Colour is for grouping cards that are the SAME KIND OF THING, and the taxonomy is yours; say what your colours mean when you present the picture, because the atlas reads the name back to you and never invents a meaning for it."
        }
    });
    let batch_node_properties = {
        let mut properties = node_properties.clone();
        if let Some(object) = properties.as_object_mut() {
            object.insert(
                "ref".to_string(),
                json!({
                    "type": "string",
                    "description": "a name you pick for this node so `links` in the same call can point at it before it has an id"
                }),
            );
        }
        properties
    };

    // The base vocabulary is always advertised. Agent-ink is appended below,
    // gated on the runtime flag, so `tools/list` and `tools/call` see nothing
    // beyond the base tools above unless `AGUI_AGENT_INK` opted in — with
    // the flag off this function returns byte-identical output to before B4.
    let mut defs = vec![
        page_validation::validate_tool(state),
        ToolDef::new(
            "atlas_decisions_read",
            "Read the project's indexed architectural decisions, including rationale, tradeoffs, and superseded records.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            { let state = state.clone(); move |_| { let state = state.clone(); Effect::Query(Box::new(move |_| cement::decision_history(state.repo()?.root()))) } },
        ).human_only(),
        ToolDef::new(
            "atlas_decision_draft",
            "Record an immutable decision proposal for human review. draft requires ids, title, decision, rationale, alternatives [{option, reason_not_chosen}], tradeoffs, consequences, checks [{id, requirement, check, evidence_files}], and optional not_enforced. check is a native G8 rust_enum_shape (file, enum_name, expected_variants, expected_serde_rename_all) or cargo_metadata_no_dep (denied [{kind:exact,value}], scope_crates, build_config:default). These constrain structure, not behavior. Set conversational context first. The human reviews and cements the draft; agents cannot approve it.",
            json!({"type":"object","properties":{"draft":{"type":"object"}},"required":["draft"],"additionalProperties":false}),
            {
                let state = state.clone();
                move |args| {
                    let input = match serde_json::from_value::<atlas::decision::DraftInput>(args["draft"].clone()) {
                        Ok(value) => value, Err(error) => return Effect::Reject(error.to_string()),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let author = state.agent_author();
                        let id = {
                            let _commit = state.begin_commit(author.as_str());
                            atlas::decision::save(&mut state.scene.lock(), &input, &author)?
                        };
                        state.persist(false);
                        Ok(Some(format!("decision draft {id} is ready for human review; no agreement or enforcement was recorded")))
                    }))
                }
            },
        ).agent_only(),
        ToolDef::new(
            "atlas_decision_preview",
            "Preview the exact decision artifacts, evidence pins, and G8 root specification change for the human. Returns an review_token; cement refuses stale approval.",
            json!({"type":"object","properties":{"ids":{"type":"array","items":{"type":"string"}},"draft_id":{"type":"string"}},"required":["ids","draft_id"],"additionalProperties":false}),
            {
                let state = state.clone();
                move |args| {
                    let ids = match string_ids(args,"ids") { Ok(ids) => ids, Err(error) => return Effect::Reject(error) };
                    let draft_id = string(args,"draft_id");
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| propose_cement_decision(&state,&ids,&draft_id,true)))
                }
            },
        ).human_only(),
        ToolDef::new(
            "atlas_session",
            "Start here. Identify this shared page, its current question and altitude, stable subject ids, unanswered human marks, and decision history. Read atlas_read for full semantics, inspect the real browser for visual meaning, then use await_input for all communication lanes. Never infer human agreement from silence or an agent reply.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            {
                let state = state.clone();
                move |_| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        let (page, revision) = state.read_with_revision()?;
                        serde_json::to_string_pretty(&json!({
                            "kind": "same-page-understanding",
                            "repository": state.repo()?.root(),
                            "document_revision": revision,
                            "context": page.context,
                            "subjects": page.nodes.iter().map(|node| json!({"id":node.id,"label":node.label,"status":node.status,"understanding":atlas::claims::challenge(&page,&node.id)})).collect::<Vec<_>>(),
                            "unanswered": page.marks.iter().filter(|mark| mark.answer.is_empty()).collect::<Vec<_>>(),
                            "decisions": page.nodes.iter().filter(|node| !node.cemented.is_empty()).map(|node| json!({"id":node.id,"label":node.label,"adr":node.cemented.lines().next()})).collect::<Vec<_>>(),
                            "next": ["atlas_read", "atlas_review", "atlas_context_set", "atlas_explanation_define", "await_input"],
                            "visual_read": "Use dev/samepage see with this server URL. A semantic snapshot alone does not prove readability.",
                            "agreement": "Only explicit human verdicts settle claims. A question, response, and agreement are separate events."
                        })).map_err(|error| error.to_string())
                    }))
                }
            },
        ).agent_only(),
        ToolDef::new(
            "atlas_context_set",
            "Set the shared conversational question, altitude, subjects, and assumptions. Read atlas_session first and pass every context.heads id as replaces. Altitude means scope of the question, independent of camera zoom. Concurrent changes need explicit reconciliation. This records your proposed scope, never the human's agreement.",
            json!({"type":"object","properties":{
                "question":{"type":"string"},
                "altitude":{"type":"string","enum":["purpose","system","component","implementation"]},
                "subjects":{"type":"array","items":{"type":"string"}},
                "assumptions":{"type":"string"},
                "replaces":{"type":"array","items":{"type":"string"}}
            },"required":["question","altitude","replaces"],"additionalProperties":false}),
            {
                let state = state.clone();
                move |args| {
                    let input = match serde_json::from_value::<atlas::context::ContextInput>(args.clone()) {
                        Ok(input) => input,
                        Err(error) => return Effect::Reject(error.to_string()),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            atlas::context::set(&mut state.scene.lock(), &input, &author)?
                        };
                        state.persist(false);
                        Ok(Some(format!("shared context {id} recorded; previous questions keep their original scope")))
                    }))
                }
            },
        ).agent_only(),
        ToolDef::new(
            "atlas_review",
            "Review the shared architecture against readable working-tree sources. Returns project facts measured by Git, the current graph and its semantic revision, shared proposal comparisons and affected components, evidence gaps, claim verdicts, authored connections, and recorded decision trade-offs. Readability is not verification of prose or runtime behavior. Does not mutate the page or accept a decision.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            {
                let state = state.clone();
                move |_| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        let (atlas, revision) = state.read_with_revision()?;
                        Ok(crate::review::inspect(state.repo()?.as_ref(), &atlas, &revision).to_string())
                    }))
                }
            },
        ).agent_only(),
        ToolDef::new(
            "atlas_architecture_propose",
            "Record a shared relationship proposal without changing the current map. Read atlas_review architecture.current_revision first. New proposals capture the current graph. To revise a proposal, pass its previous id and baseline_revision; concurrent alternatives remain separate revisions. remove and add contain exact {from,to,label} relationships. Proposals are drafts, never approval or implementation.",
            json!({"type":"object","properties":{
                "title":{"type":"string","minLength":1,"maxLength":160},
                "baseline_revision":{"type":"string"},"previous":{"type":"string"},
                "remove":{"type":"array","items":{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"},"label":{"type":"string"}},"required":["from","to","label"],"additionalProperties":false}},
                "add":{"type":"array","items":{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"},"label":{"type":"string"}},"required":["from","to","label"],"additionalProperties":false}}
            },"required":["title","baseline_revision"],"additionalProperties":false}),
            {
                let state = state.clone();
                move |args| {
                    let input = match serde_json::from_value::<atlas::architecture::ProposalInput>(args.clone()) {
                        Ok(input) => input,
                        Err(error) => return Effect::Reject(error.to_string()),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            atlas::architecture::propose(&mut state.scene.lock(), &input, &author)?
                        };
                        state.persist(false);
                        Ok(Some(format!("architecture proposal {id} recorded; current relationships unchanged. Read atlas_review to compare.")))
                    }))
                }
            },
        ).agent_only(),
        ToolDef::new(
            "atlas_read",
            "Read the shared atlas: every node, link and human mark, with who \
             last edited each one. Call this FIRST in a turn and again after \
             the human says they changed something, they can move, relabel, \
             and flag anything you draw. After your first read it ends with \
             CHANGED SINCE YOUR LAST READ, which is the part to act on: it \
             says what their edits now MEAN, a stroke that used to be \
             decoration and now groups two cards, an arrow that now connects \
             something else, rather than making you diff two long documents \
             in your head. The optional WHERE THE HUMAN IS section is \
             live-only, lease-bound context, not a history. Viewport, \
             selection, and drag observations expire within tens of seconds. \
             If the section is absent, live context is currently absent; the \
             channel is not broken.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |surface| {
                        let me = state.agent_byline();
                        state.read_back(surface, &me)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_cement_decision_propose",
            "Preview a recorded decision draft and its explicit checks without writing. Pass draft_id from atlas_decision_draft. Every selected node must be agreed and settled. Only the human can activate the checks and publish the ADR.",
            json!({
                "type": "object",
                "properties": {
                    "draft_id": {"type":"string"},
                    "ids": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": { "type": "string" },
                        "description": "selected Atlas node ids"
                    }
                },
                "required": ["ids", "draft_id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let ids = match string_ids(args, "ids") {
                        Ok(ids) => ids,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    let draft_id = string(args, "draft_id");
                    Effect::Query(Box::new(move |_| propose_cement_decision(&state, &ids, &draft_id, false)))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_cement_decision",
            "Cement the human-reviewed draft and activate its checks in the repository's G8 specification and lock. Requires the exact preview review_token and draft_id. Publishes and indexes the ADR, retains the review bundle, and records whether the activated gate is passing or blocked. Structural checks do not prove behavior. Refuses stale review or unaccepted claims.",
            json!({
                "type": "object",
                "properties": {
                    "directory": {
                        "type": "string",
                        "minLength": 1,
                        "description": "new output directory for the ADR, obligations fragment, and receipt"
                    },
                    "draft_id": {"type":"string"},
                    "review_token": {"type":"string"},
                    "ids": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": { "type": "string" },
                        "description": "selected Atlas node ids"
                    }
                },
                "required": ["directory", "ids", "draft_id", "review_token"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let directory = string(args, "directory");
                    if directory.trim().is_empty() {
                        return Effect::Reject("`directory` is empty".to_string());
                    }
                    let ids = match string_ids(args, "ids") {
                        Ok(ids) => ids,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    let draft_id = string(args, "draft_id");
                    let review_token = string(args, "review_token");
                    Effect::Mutate(Box::new(move |_| {
                        cement_decision(&state, &ids, std::path::Path::new(&directory), &draft_id, &review_token).map(Some)
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "atlas_diff",
            "Compare two snapshots of one Atlas document and return the core's difference sentences, both document fingerprints, and the receipt over all of them. `before` and `after` accept the exact host snapshot object carried in a `STATE_SNAPSHOT` event, either as a JSON string or as its parsed object. Object ids must come from the same document; independently drawn documents are not comparable.",
            json!({
                "type": "object",
                "properties": {
                    "before": {
                        "anyOf": [{ "type": "object" }, { "type": "string" }],
                        "description": "earlier STATE_SNAPSHOT event's snapshot object"
                    },
                    "after": {
                        "anyOf": [{ "type": "object" }, { "type": "string" }],
                        "description": "later STATE_SNAPSHOT event's snapshot object"
                    }
                },
                "required": ["before", "after"],
                "additionalProperties": false
            }),
            move |args: &JsonValue| {
                let before = match atlas_diff_snapshot(args, "before") {
                    Ok(snapshot) => snapshot,
                    Err(error) => return Effect::Reject(error),
                };
                let after = match atlas_diff_snapshot(args, "after") {
                    Ok(snapshot) => snapshot,
                    Err(error) => return Effect::Reject(error),
                };
                let rendered = match serde_json::to_string_pretty(&before.diff_receipt(&after)) {
                    Ok(rendered) => rendered,
                    Err(error) => {
                        return Effect::Reject(format!(
                            "could not serialize the Atlas diff receipt: {error}"
                        ))
                    }
                };
                Effect::Query(Box::new(move |_| Ok(rendered)))
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_explanation_define",
            "Define or replace a named durable interactive exploration. Distinct ids coexist; replacing the same id preserves its object identity and increments its revision. Set about to existing subjects and scene_ids to the exploration's exclusive visual objects, which the browser separates from the blueprint. Each beat binds a cue to stable evidence, visual actions, and explicit next transitions. A transition with control=button advances immediately and reaches the agent as structured interaction context. Omit control when the learner must explain in their own words. Use trace-path on an arrow or line for directional motion that repeats while the step is active. Use replay-motion for a prepared segment motion program; use point or reveal for attention. Prepare expensive assets first. Graph structure, targets, scope ownership, reachability, and the no-cycle rule are validated before anything is stored.",
            json!({
                "type": "object",
                "properties": { "flow": explanation_flow_schema() },
                "required": ["flow"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let definition = match args
                        .get("flow")
                        .cloned()
                        .ok_or_else(|| "flow is required".to_string())
                        .and_then(|value| {
                            serde_json::from_value::<atlas::ExplanationFlowDefinition>(value)
                                .map_err(|error| format!("invalid explanation flow: {error}"))
                        })
                    {
                        Ok(definition) => definition,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let flow = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::define_explanation_flow(&mut scene, &definition, &author)?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "explanation {:?} stored at beat {:?}, status={}, revision={}; the browser and atlas_read now project the same flow",
                            flow.definition.title,
                            flow.state.current_beat,
                            flow.state.status,
                            flow.state.revision,
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_explanation_advance",
            "Advance one named transition from the exact flow revision you read. Resolve the learner's utterance together with the live selected target before choosing transition_id. Send both as evidence. The shared core refuses a stale revision, an unknown transition, an unsupported selected target, or a missing next-beat target without changing the flow.",
            json!({
                "type": "object",
                "properties": {
                    "flow_id": { "type": "string", "minLength": 1, "maxLength": 160 },
                    "expected_revision": { "type": "integer", "minimum": 1 },
                    "transition_id": { "type": "string", "minLength": 1, "maxLength": 160 },
                    "response": { "type": "string", "maxLength": 1200 },
                    "selected_target_ids": {
                        "type": "array",
                        "maxItems": 32,
                        "uniqueItems": true,
                        "items": { "type": "string", "minLength": 1, "maxLength": 160 }
                    }
                },
                "required": ["flow_id", "expected_revision", "transition_id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let flow_id = string(args, "flow_id");
                    let transition_id = string(args, "transition_id");
                    let response = string(args, "response");
                    let expected_revision = match args
                        .get("expected_revision")
                        .and_then(JsonValue::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                    {
                        Some(value) if value > 0 => value,
                        _ => {
                            return Effect::Reject(
                                "expected_revision must be a positive u32".to_string(),
                            );
                        }
                    };
                    let selected_target_ids = match args.get("selected_target_ids") {
                        None => Vec::new(),
                        Some(JsonValue::Array(values)) => {
                            let parsed = values
                                .iter()
                                .enumerate()
                                .map(|(index, value)| {
                                    value.as_str().map(str::to_string).ok_or_else(|| {
                                        format!("selected_target_ids[{index}] must be a string")
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>();
                            match parsed {
                                Ok(values) => values,
                                Err(error) => return Effect::Reject(error),
                            }
                        }
                        Some(_) => {
                            return Effect::Reject(
                                "selected_target_ids must be an array".to_string(),
                            );
                        }
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let next = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let scene = state.scene.lock();
                            atlas::advance_explanation_flow(
                                &scene,
                                &flow_id,
                                expected_revision,
                                &transition_id,
                                &response,
                                &selected_target_ids,
                                &author,
                            )?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "explanation {flow_id:?} advanced via {transition_id:?} to beat {:?}, status={}, revision={}",
                            next.current_beat, next.status, next.revision
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_explanation_control",
            "Pause, resume, restart, or stop the active explanation from the exact revision you read. This changes only the explanation cursor. It never unloads model assets, changes the infographic, or rewrites segment motion.",
            json!({
                "type": "object",
                "properties": {
                    "flow_id": { "type": "string", "minLength": 1, "maxLength": 160 },
                    "expected_revision": { "type": "integer", "minimum": 1 },
                    "command": { "type": "string", "enum": ["pause", "resume", "restart", "stop"] }
                },
                "required": ["flow_id", "expected_revision", "command"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let flow_id = string(args, "flow_id");
                    let command = string(args, "command");
                    let expected_revision = match args
                        .get("expected_revision")
                        .and_then(JsonValue::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                    {
                        Some(value) if value > 0 => value,
                        _ => {
                            return Effect::Reject(
                                "expected_revision must be a positive u32".to_string(),
                            );
                        }
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let next = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let scene = state.scene.lock();
                            atlas::control_explanation_flow(
                                &scene,
                                &flow_id,
                                expected_revision,
                                &command,
                                &author,
                            )?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "explanation {flow_id:?} is now {}, beat {:?}, revision={}",
                            next.status, next.current_beat, next.revision
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_constrain",
            "Declare exact structural relations on existing atlas objects. Use group for membership, sequence for ordered flow on x or y, attaches for a pair that must stay related, voids for an excluded object, and labels for explicit text attached to one object. Members may be stable ids or unique visible titles. Refusals beat guesses: an invalid or ambiguous batch stores nothing. Declare salience as fore or back. The reply echoes every stored relation and the whole document constraint state. If that state is UNSAT, repair the constraints before changing the picture.",
            json!({
                "type": "object",
                "properties": {
                    "constraints": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": constraint_ops },
                                "members": {
                                    "type": "array",
                                    "minItems": 1,
                                    "items": { "type": "string" },
                                    "description": "Stable object ids or unique visible node or shape titles. Order is meaningful for sequence."
                                },
                                "axis": { "type": "string", "enum": constraint_axes, "description": "Sequence axis. Defaults to x." },
                                "text": { "type": "string", "description": "Visible text for labels." },
                                "salience": { "type": "string", "enum": constraint_saliences, "description": "fore draws at full weight; back draws behind the cards. Defaults to back." }
                            },
                            "required": ["op", "members"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["constraints"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let inputs = match constraint_inputs(args) {
                        Ok(inputs) => inputs,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        constrain(state.as_ref(), inputs).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_unconstrain",
            "Remove one or more declared constraints by stable id. A constraint is a claim, not its author's property, so you may remove another author's constraint. The reply names the original author of every removed claim.",
            json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "minItems": 1,
                        "uniqueItems": true,
                        "items": { "type": "string" }
                    }
                },
                "required": ["ids"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let ids = match string_ids(args, "ids") {
                        Ok(ids) => ids,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        unconstrain(state.as_ref(), ids).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "await_atlas",
            "Wait until somebody else changes the shared atlas, then read it. Blocks until a \
             caller with a different byline commits a node, link, drawing, removal, answer, or \
             human mark, and returns the same read-back as atlas_read. Attention pans alone do \
             not wake it, but current attention is included when a document change does. Use it \
             after atlas_read and your own edits to stay on the page without asking again. \
             Returns after `seconds` (default 60, max 600) with no change if the atlas stayed \
             quiet; call it again to keep waiting.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_WAIT_SECONDS,
                        "description": "How long to wait before returning with no change."
                    }
                }
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let seconds = match wait_seconds(args) {
                        Ok(value) => value,
                        Err(error) => return Effect::Reject(error),
                    };
                    // Capture the byline before constructing the future.
                    // `note_caller` may name somebody else before it wakes.
                    let me = state.agent_byline();
                    let state = state.clone();
                    Effect::AsyncQuery(Box::pin(async move {
                        let deadline =
                            tokio::time::Instant::now() + Duration::from_secs(seconds);
                        loop {
                            // Register before checking so a commit between the
                            // check and the await cannot be lost.
                            let woken = state.changed.notified();
                            tokio::pin!(woken);
                            woken.as_mut().enable();
                            state.validation_failure()?;
                            if state.unread_from_others(&me) {
                                return state.read_back(state.as_ref(), &me);
                            }
                            tokio::select! {
                                _ = woken.as_mut() => {}
                                _ = tokio::time::sleep_until(deadline) => {
                                    return Ok(format!(
                                        "Waited {seconds}s and the atlas did not change. \
                                         Nobody else has committed anything since your last read. \
                                         Call await_atlas again to keep waiting."
                                    ));
                                }
                            }
                        }
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_diagram",
            "Draw the picture with `atlas_sketch` first. This tool authors a typed hierarchy or state machine in the node register in one call. `kind` defaults to hierarchy for compatibility. A hierarchy may declare nested `containers`; `group` assigns a node to a container id and still creates an implicit top-level container when no declaration exists. A state_machine requires a title, one event per transition, and accepts initial and terminal state roles. `data_flow` and `user_flow` are reserved and refused. The shared layout engine recursively packs hierarchy boxes and places cyclic machines on a ring with distinct transition routes. Use source-backed cards when the claims themselves are the point, each with its own `path` and `lines` verified before anything lands. A card is a paragraph; a `note` grows it and that height participates in layout. Links stay attached when either card moves. FORM CARRIES MEANING and it is yours to choose and say out loud: weight is importance (`emphasis` strong for what the argument turns on, muted for context), enclosure is ownership (`group` puts members inside a real container node that owns them; atlas_read nests them, links across levels read as \"X (inside PARENT) -> Y\", deleting the container moves its members up one level, containment is a tree and a cycle is refused), colour is category (`color`), `shape` is what a card IS, and `size` makes one anchor hero and a few primary. `shape`, `color`, `emphasis` and `size` are stored on the node itself and come back in atlas_read; none of them creates a drawn shape. The atlas stores and renders what you pick and never decides what it means, so name your scheme when you present the picture.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": diagram_kinds, "description": "diagram semantics. hierarchy is the default and supports nested containers. state_machine requires a title and transition events. data_flow and user_flow are reserved and refused this board" },
                    "containers": {
                        "type": "array",
                        "description": "hierarchy containers in any order. A parent names another container id in this call or an existing Atlas node. State machines refuse this field",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "stable container name used by parent and group" },
                                "label": { "type": "string", "description": "visible container title" },
                                "parent": { "type": "string", "description": "optional owning container id or existing Atlas node id" }
                            },
                            "required": ["id", "label"],
                            "additionalProperties": false
                        }
                    },
                    "nodes": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "stable name used by edges" },
                                "label": { "type": "string", "description": "visible words; defaults to id" },
                                "path": { "type": "string", "description": "repository-relative source this node is about; verified for the whole batch before anything lands" },
                                "lines": { "type": "string", "description": "N or N-M within path" },
                                "note": { "type": "string", "description": "the source-backed claim in your own words; its visible height is included in diagram packing" },
                                "shape": { "type": "string", "enum": ["rect", "rectangle", "ellipse", "diamond"], "description": "what the card IS, stored on the node as its `kind` and rendered. The usual reading is rect for a step, diamond for a decision, ellipse for a boundary; the meaning is yours to state" },
                                "color": { "type": "string", "description": "category colour, a named Atlas ink or a hex that snaps to the nearest one. Stored on the node and read back as the NAME. Use it to group cards that are the same kind of thing, and say what your colours mean" },
                                "filled": { "type": "boolean", "description": "older spelling of emphasis: true means strong. Saying both `filled` and `emphasis` with different meanings is refused rather than guessed" },
                                "size": { "type": "string", "enum": ["normal", "primary", "hero"], "description": "importance as footprint, stored on the node and used by layout. Use hero once for the argument's anchor and primary sparingly. For \"make this stand out\" without resizing it, use `emphasis`" },
                                "group": { "type": "string", "description": "enclosure is ownership: nodes sharing a group are placed inside one container node of that name. Containment is real, so the read-back nests them and deleting the container moves them up rather than removing them" },
                                "emphasis": { "type": "string", "enum": ["normal", "strong", "muted"], "description": "weight is importance: strong for the few things the argument turns on, muted for context the reader may skip. Default normal" },
                                "initial": { "type": "boolean", "description": "marks this state as the state_machine entry point" },
                                "terminal": { "type": "boolean", "description": "marks this state as ending a state_machine; an outgoing transition becomes a PROBLEM" }
                            },
                            "additionalProperties": false
                        }
                    },
                    "edges": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string" },
                                "to": { "type": "string" },
                                "label": { "type": "string" },
                                "event": { "type": "string", "description": "required state_machine transition event" },
                                "guard": { "type": "string", "description": "optional state_machine transition condition" },
                                "arrow": { "type": "boolean", "description": "node-register links are directed; false is refused, use atlas_sketch for an undirected annotation" }
                            },
                            "required": ["from", "to"],
                            "additionalProperties": false
                        }
                    },
                    "direction": { "type": "string", "enum": ["right", "down"], "description": "flow direction; default right" },
                    "title": { "type": "string", "description": "names the diagram. Required for state_machine, where it becomes the machine container title" },
                    "origin_x": { "type": "number", "description": "optional left edge. By default the diagram lands to the right of existing content" },
                    "origin_y": { "type": "number", "description": "optional top edge; default 120" }
                },
                "required": ["nodes"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let args = args.clone();
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| state.draw_diagram(&args).map(Some)))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_draw",
            "Draw the picture with `atlas_sketch` first. This tool puts \
             several source-backed cards and the links between them into the \
             node register in ONE call at positions you choose; prefer it to \
             repeated `atlas_place` calls. Give a card a `ref` and links in \
             the same call can point at it before it has an id. Every `path` \
             is verified before anything lands, and a link that points at \
             nothing rejects the whole write. Cards are for claims the human \
             is meant to agree with or dispute; a `frame` drawn with \
             `atlas_sketch` can hold them. Use `atlas_place` afterwards to \
             refine one card.",
            json!({
                "type": "object",
                "properties": {
                    "nodes": {
                        "type": "array",
                        "description": "nodes to create or update, in the order you want them placed",
                        "items": {
                            "type": "object",
                            "properties": batch_node_properties,
                            "additionalProperties": false
                        }
                    },
                    "links": {
                        "type": "array",
                        "description": "relationships; `from`/`to` are either an existing node id or a `ref` from this call",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string" },
                                "to": { "type": "string" },
                                "label": { "type": "string", "description": "how they relate: 'depends on', 'proves', ..." }
                            },
                            "required": ["from", "to"],
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let nodes: Vec<atlas::NodeDraft> = args
                        .get("nodes")
                        .and_then(JsonValue::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| atlas::NodeDraft {
                                    reference: optional_string(item, "ref"),
                                    patch: node_patch(item),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let links: Vec<atlas::LinkDraft> = args
                        .get("links")
                        .and_then(JsonValue::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| atlas::LinkDraft {
                                    from: string(item, "from"),
                                    to: string(item, "to"),
                                    label: string(item, "label"),
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    // Every source reference in the batch is verified before
                    // any of it is written, so one stale line range cannot
                    // leave half a subgraph on the human's screen.
                    for (index, node) in nodes.iter().enumerate() {
                        if let Some(path) = &node.patch.path {
                            if let Err(error) = state
                                .verify_source(
                                    path,
                                    node.patch.lines.as_deref().unwrap_or_default(),
                                    "",
                                )
                            {
                                return Effect::Reject(format!(
                                    "nodes[{index}]: that source reference does not hold: {error}"
                                ));
                            }
                        }
                    }

                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let drawn = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::draw(&mut scene, &nodes, &links, &author)?
                        };
                        state.persist(false);
                        let atlas = state.read()?;
                        // Report the real ids, keyed by the caller's own
                        // references, so the next turn can address what it
                        // just drew without another read.
                        let placed = drawn
                            .nodes
                            .iter()
                            .map(|(reference, id)| {
                                let label = atlas
                                    .node(id)
                                    .map(|node| node.label.as_str())
                                    .unwrap_or("?");
                                match reference {
                                    Some(reference) => format!("{reference} -> {id} \"{label}\""),
                                    None => format!("{id} \"{label}\""),
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        Ok(Some(format!(
                            "drew {} node(s) and {} link(s) on the shared page: {placed}. The human can move or flag any of them.",
                            drawn.nodes.len(),
                            drawn.edges.len()
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_place",
            "Draw the picture with `atlas_sketch` first. This tool authors or \
             updates ONE card in the node register: a claim with a verified \
             `path` and a verdict, something the human can agree with, \
             dispute, or flag. Use `atlas_draw` for several connected cards or \
             `atlas_diagram` for a laid-out set. Omit `id` to create and pass \
             it to update; only supplied fields change, so a note can change \
             without moving a card the human dragged. A missing file or line \
             range is rejected. Set `parent` to put this card inside another \
             one: containment is first class in the node register, so the \
             read-back nests it under its container and a cycle is refused \
             rather than stored. FORM CARRIES MEANING and the meaning is \
             yours: weight is importance (`emphasis`), enclosure is ownership \
             (`parent`), colour is category (`color`), and `kind` is what the \
             card is. The atlas stores and renders your choice and never \
             decides what it means, so say what your scheme means when you \
             present the picture.",
            json!({
                "type": "object",
                "properties": node_properties,
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let patch = node_patch(args);
                    if let Some(path) = &patch.path {
                        if let Err(error) = state
                            .verify_source(path, patch.lines.as_deref().unwrap_or_default(), "")
                        {
                            return Effect::Reject(format!(
                                "that source reference does not hold: {error}"
                            ));
                        }
                    }
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::place_node(&mut scene, &patch, &author)?
                        };
                        state.persist(false);
                        let atlas = state.read()?;
                        let node = atlas
                            .node(&id)
                            .ok_or_else(|| "node vanished during the write".to_string())?;
                        Ok(Some(format!(
                            "node {id} \"{}\" is on the shared page at ({:.0},{:.0}); the human can move or flag it",
                            node.label, node.x, node.y
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_link",
            "Author a labelled semantic relationship in the node register \
             between two existing nodes. Use this when the relationship is \
             part of the record between two claims and should read back as a \
             labelled link. A drawn `atlas_sketch` arrow is the right tool for \
             a visual relationship between shapes. Calling `atlas_link` again \
             for the same pair relabels the existing link.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "label": { "type": "string", "description": "how they relate: 'depends on', 'proves', ..." }
                },
                "required": ["from", "to"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let (from, to, label) = (
                        string(args, "from"),
                        string(args, "to"),
                        string(args, "label"),
                    );
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::link(&mut scene, &from, &to, &label, &author)?
                        };
                        state.persist(false);
                        Ok(Some(format!("link {id} drawn")))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_sketch",
            "Draw the picture. This is the primary way to put an idea on the \
             shared page, and a picture is a few big shapes with short labels \
             that a human reads at a glance, not a wall of paragraphs. RULES: \
             1. Put the whole picture down in ONE call, in paint order. 2. One \
             region per idea, whitespace between regions, nothing touching \
             anything it does not mean to touch. 3. A heading is a `text` \
             with `font_size` 28 or 36 and a `w` wide enough for one line; \
             the default is 13px wrapped at 240px and reads as a caption. 4. \
             Every `label` is at most 140 characters, a `text` included; a \
             paragraph is several short `text` lines or a card. A box's \
             `label` is the words INSIDE it, wrapped to its width, so keep it \
             to a few words and size the box for them. Give a shape a `ref` \
             and a later shape in the same call can bind `from`/`to` to it or \
             sit in it as its `frame`, so frames and boxes first, arrows \
             after, one call. 5. Read \
             the reply: it says what each shape turned out to MEAN (ENCLOSES, \
             POINTS-AT, CROSSES-OUT, SAYS NEAR) and lists the PROBLEMS \
             measured from the page that involve what you drew, boxes \
             overlapping, a box clipping a card, texts or strokes stacked on \
             each other, ink across a card it says nothing about. `atlas_read` \
             LAYOUT repeats them. Fix every one before you say you are done; a \
             picture the human cannot read has explained nothing. VOCABULARY: \
             rect, ellipse, diamond, line, arrow, ink (freehand), text, and \
             frame, in Excalidraw's terms, at positions you choose (world \
             units, y grows down). A heavy `stroke_width` is the main path, \
             `dashed` is tentative, low `opacity` is past or background, one \
             colour is one category, and a `frame` is a named region for a \
             before-and-after, a stage, or a legend; frames do not nest. An \
             arrow given `from` and `to` binds to those shapes or cards and \
             keeps touching them when the human drags either end. A batch is \
             checked shape by shape: the shapes before a refused one stay on \
             the page and the error says so. CARDS are the footnotes of a \
             picture, not the picture. A card is a claim with a verified \
             source that the human can agree with or dispute: `atlas_place` \
             for one, `atlas_draw` or `atlas_diagram` for several with links, \
             `atlas_link` for a relationship that belongs in the record. Leave \
             a card its room (232px wide by default, about 80px bare and 230 \
             to 300px with a two-sentence note and a source; LAYOUT in \
             `atlas_read` has the real box) or draw the frame it will sit in. The \
             human draws with the same vocabulary and can promote any \
             labelled shape into a card in place, so sketch now and promote \
             later what earns a source.",
            json!({
                "type": "object",
                "properties": {
                    "shapes": {
                        "type": "array",
                        "description": "shapes to create or update, in paint order",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "existing shape id; omit to create" },
                                "ref": { "type": "string", "description": "a name you give this shape so a later shape in the SAME call can bind `from`/`to` to it before it has an id. Draw the boxes first, then the arrows that join them, all in one call." },
                                "form": { "type": "string", "enum": forms.clone() },
                                "x": { "type": "number", "description": "left edge for a box form; origin the `points` are relative to otherwise. World units, y grows DOWN." },
                                "y": { "type": "number" },
                                "w": { "type": "number", "description": "rect/ellipse/diamond: width. text: the width its words wrap at." },
                                "h": { "type": "number", "description": "rect/ellipse/diamond only, a text is as tall as its own words, measured by the browser" },
                                "points": {
                                    "type": "string",
                                    "description": "\"x,y x,y ...\" relative to (x,y), for ink/line/arrow. Omit on an arrow that has `from` and `to`; if you give both, the first and last points are replaced by the bound ends and the middle points stay as waypoints."
                                },
                                "from": { "type": "string", "description": "node or bindable shape id this connector starts at, or a `ref` given to an earlier shape in this call; the endpoint follows that object" },
                                "to": { "type": "string", "description": "node or bindable shape id this connector ends at, or a `ref` from earlier in this call" },
                                "head": { "type": "string", "enum": heads.clone() },
                                "ink": { "type": "string", "enum": inks.clone() },
                                "fill": { "type": "string", "enum": fills.clone(), "description": "one of the ink colours, or `none`" },
                                "label": { "type": "string", "description": "at most 140 characters. On a rect/ellipse/diamond these are the words INSIDE the box, wrapped to its width at 14px, so keep it to a few words and size the box for them: roughly (w - 16) / 7 characters per line and 18px per line. On a `text` it is the whole shape and is required. On a `frame` it is the frame's name, drawn above it. On a line or arrow it is drawn beside the middle." },
                                "stroke_width": { "type": "number", "description": "world px, 0.5 to 12; default 2. Excalidraw's 1 / 2 / 4 are thin / regular / bold. Thickness is meaning: a heavy stroke reads as the main path." },
                                "stroke_style": { "type": "string", "enum": stroke_styles.clone(), "description": "dashed reads as tentative or proposed; dotted as faint or optional" },
                                "opacity": { "type": "number", "description": "0 to 100; default 100. Fade what is background, context, or a previous state." },
                                "roundness": { "type": "string", "enum": roundness.clone(), "description": "rect/frame corners, line/arrow joins. rect defaults to round." },
                                "font_size": { "type": "number", "description": "px for a `text` or a caption; 6 to 120, 0 = unset. Excalidraw's steps are 16 / 20 / 28 / 36." },
                                "angle": { "type": "number", "description": "radians clockwise about the centre; sized forms only (rect/ellipse/diamond/text/frame). A stroke is rotated through its points." },
                                "groups": { "type": "string", "description": "whitespace-separated group ids, innermost last. Shapes sharing a group id move together for the human and are read back as one. Empty string leaves every group." },
                                "frame": { "type": "string", "description": "id of a `frame` shape this shape sits in, or the `ref` of a frame drawn earlier in this call. A frame is a named region that holds drawn shapes (before/after panels, stages, a legend). Frames do not nest. Empty string leaves the frame." }
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["shapes"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let shapes: Vec<atlas::ShapePatch> = args
                        .get("shapes")
                        .and_then(JsonValue::as_array)
                        .map(|items| items.iter().map(shape_patch).collect())
                        .unwrap_or_default();
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let drawn = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::sketch(&mut scene, &shapes, &author)?
                        };
                        state.persist(false);
                        // Report what each shape ended up MEANING, not that it
                        // was written. A shape the agent thought would circle
                        // three nodes and actually circles one is a mistake it
                        // can only fix if the write tells it so.
                        let atlas = state.read()?;
                        let readings = atlas::readings(&atlas);
                        let summary = drawn
                            .iter()
                            .map(|id| {
                                match readings.iter().find(|reading| &reading.shape == id) {
                                    Some(reading) if reading.targets.is_empty() => {
                                        format!("{id} (touches no node)")
                                    }
                                    Some(reading) => {
                                        let labels = reading
                                            .targets
                                            .iter()
                                            .map(|target| {
                                                atlas
                                                    .node(target)
                                                    .map(|node| node.label.clone())
                                                    .unwrap_or_else(|| target.clone())
                                            })
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        format!("{id} {} {labels}", reading.relation)
                                    }
                                    None => id.clone(),
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        // The geometry the agent has no eyes for, narrowed to
                        // the shapes it just drew: a box that landed on a
                        // card is unreadable on screen right now, and the
                        // next atlas_read is too late to hear about it.
                        let problems = atlas::layout(&atlas)
                            .problems
                            .into_iter()
                            .filter(|problem| problem.ids.iter().any(|id| drawn.contains(id)))
                            .map(|problem| problem.detail)
                            .collect::<Vec<_>>();
                        let problems = if problems.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " PROBLEMS ({}) measured from the page, fix before moving on: {}.",
                                problems.len(),
                                problems.join("; ")
                            )
                        };
                        Ok(Some(format!(
                            "drew {} shape(s): {summary}.{problems} The human sees these on the map and can draw back.",
                            drawn.len()
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_import",
            "Put an existing `.excalidraw`, `.architecture.json`, or \
             `.workflow.json` diagram from this project onto the atlas. \
             Excalidraw lands as editable shapes. Archify architecture and \
             workflow JSON lands as typed nodes, container frames, relations, \
             and one basis-labelled claim per source node. The shape vocabulary \
             here IS Excalidraw's, so a drawing \
             somebody already made comes across as real, editable shapes \
             rather than a picture, and lands where the human can then move, \
             relabel and draw on it. Only the drawn layer arrives: a card is a \
             claim about a place in this repository and an Excalidraw \
             rectangle is not, so rectangles stay rectangles instead of being \
             promoted into claims nobody made. Anything that cannot come \
             across is named in the reply rather than dropped quietly.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "repository-relative path to a .excalidraw, .architecture.json, or .workflow.json file" },
                    "dx": { "type": "number", "description": "shift everything right by this much on the way in; use it to land a diagram beside the map instead of on top of it" },
                    "dy": { "type": "number" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let path = string(args, "path");
                    let dx = optional_number(args, "dx").unwrap_or_default();
                    let dy = optional_number(args, "dy").unwrap_or_default();
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        state.import_file(&path, dx, dy).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_oracle_import",
            "Mint one stable Atlas object from oracle semantics, then let the real browser MobileSAM worker derive its appearance. This is the positive control: supply the meaningful label, tags, OCR associations, and source-pixel box. The host, not the model, mints the id. The only accepted source is the checked-in Visual Instinct infographic, and missing or wrong model artifacts fail instead of falling back.",
            json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 16 },
                    "ocr": { "type": "array", "items": { "type": "string" }, "maxItems": 16 },
                    "prompt_box": {
                        "type": "array",
                        "prefixItems": [
                            { "type": "number" }, { "type": "number" },
                            { "type": "number" }, { "type": "number" }
                        ],
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "[left, top, right, bottom] in the 1536x1024 source infographic"
                    },
                    "x": { "type": "number", "description": "Atlas world left edge" },
                    "y": { "type": "number", "description": "Atlas world top edge" },
                    "w": { "type": "number", "minimum": 64, "maximum": 2000 }
                },
                "required": ["label", "tags", "ocr", "prompt_box", "x", "y", "w"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let proposal: atlas::SegmentProposal = match serde_json::from_value(args.clone()) {
                        Ok(proposal) => proposal,
                        Err(error) => return Effect::Reject(format!("invalid segment proposal: {error}")),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::propose_segment(&mut scene, &proposal, &author)?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "segment {id} was minted by Atlas at generation 1; MobileSAM appearance is pending browser-worker materialization"
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_agent_import",
            "Mint one stable Atlas object from semantics produced by a real multimodal agent, then let the unchanged browser MobileSAM worker derive its appearance. The semantic receipt is participant-claimed because Atlas cannot attest to an external model invocation, but the host binds it to the pinned image digest and exact label, tags, OCR associations, and source-pixel box. Any mock, fallback, output mismatch, or missing provenance is rejected before an id is minted.",
            json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 16 },
                    "ocr": { "type": "array", "items": { "type": "string" }, "maxItems": 16 },
                    "prompt_box": {
                        "type": "array",
                        "prefixItems": [
                            { "type": "number" }, { "type": "number" },
                            { "type": "number" }, { "type": "number" }
                        ],
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "multimodal-agent box [left, top, right, bottom] in the 1536x1024 pinned source"
                    },
                    "x": { "type": "number", "description": "Atlas world left edge" },
                    "y": { "type": "number", "description": "Atlas world top edge" },
                    "w": { "type": "number", "minimum": 64, "maximum": 2000 },
                    "semantic_receipt": {
                        "type": "object",
                        "properties": {
                            "schema": { "type": "string", "const": atlas::SEGMENT_SEMANTIC_RECEIPT_SCHEMA },
                            "provider": { "type": "string", "minLength": 1, "maxLength": 120 },
                            "model": { "type": "string", "minLength": 1, "maxLength": 120 },
                            "request_id": { "type": "string", "minLength": 1, "maxLength": 200 },
                            "execution_location": { "type": "string", "const": "external-multimodal-agent" },
                            "trust": { "type": "string", "const": "participant-claimed" },
                            "source_sha256": { "type": "string", "const": atlas::SEGMENT_SOURCE_SHA256 },
                            "source_size": {
                                "type": "array",
                                "prefixItems": [
                                    { "type": "integer", "const": atlas::SEGMENT_SOURCE_WIDTH },
                                    { "type": "integer", "const": atlas::SEGMENT_SOURCE_HEIGHT }
                                ],
                                "minItems": 2,
                                "maxItems": 2
                            },
                            "input_mime_type": { "type": "string", "const": "image/png" },
                            "output_sha256": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
                            "fallback_used": { "type": "boolean", "const": false },
                            "mock_used": { "type": "boolean", "const": false }
                        },
                        "required": [
                            "schema", "provider", "model", "request_id",
                            "execution_location", "trust", "source_sha256", "source_size",
                            "input_mime_type", "output_sha256", "fallback_used", "mock_used"
                        ],
                        "additionalProperties": false
                    }
                },
                "required": [
                    "label", "tags", "ocr", "prompt_box", "x", "y", "w",
                    "semantic_receipt"
                ],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let proposal: atlas::AgentSegmentProposal =
                        match serde_json::from_value(args.clone()) {
                            Ok(proposal) => proposal,
                            Err(error) => {
                                return Effect::Reject(format!(
                                    "invalid multimodal segment proposal: {error}"
                                ));
                            }
                        };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::propose_agent_segment(
                                &mut scene,
                                &proposal.proposal,
                                &proposal.semantic_receipt,
                                &author,
                            )?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "segment {id} was minted from receipt-bound multimodal semantics at generation 1; MobileSAM appearance is pending browser-worker materialization"
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_move",
            "Move an accepted segment by its Atlas object id. Motion is a separate operation, so changing animation never requires the agent to resend or guess a saved position. The same id remains authoritative across every appearance generation.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                },
                "required": ["id", "x", "y"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let x = optional_number(args, "x").unwrap_or(f64::NAN);
                    let y = optional_number(args, "y").unwrap_or(f64::NAN);
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let updated = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::read(&scene)?
                                .shape(&id)
                                .filter(|shape| shape.form == "segment")
                                .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
                            atlas::place_shape(
                                &mut scene,
                                &atlas::ShapePatch {
                                    id: Some(id.clone()),
                                    x: Some(x),
                                    y: Some(y),
                                    ..atlas::ShapePatch::default()
                                },
                                &author,
                            )?;
                            atlas::read(&scene)?
                                .shape(&id)
                                .cloned()
                                .ok_or_else(|| format!("segment {id} vanished during the move"))?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "segment {} moved to ({:.0},{:.0}); motion was unchanged, and identity and generation {} were preserved",
                            updated.id, updated.x, updated.y, updated.segment_generation
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_reparent",
            "Nest an existing materialized mask part beneath another part of the same root object without changing its world position, mask identity, or appearance generation. Use this to express anatomy such as body -> foot -> claws. Root objects, cross-object moves, cycles, and hierarchies deeper than the bounded product limit are refused.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "parent_id": { "type": "string" }
                },
                "required": ["id", "parent_id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let parent_id = string(args, "parent_id");
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let updated = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::set_segment_parent(&mut scene, &id, &parent_id, &author)?;
                            atlas::read(&scene)?
                                .shape(&id)
                                .cloned()
                                .ok_or_else(|| format!("segment {id} vanished during reparenting"))?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "segment {} is now nested under {}; world position and appearance generation {} were preserved",
                            updated.id, updated.segment_parent_id, updated.segment_generation
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_motion",
            "Set or clear expressive 2D motion without changing saved layout. A materialized segment may target itself and any materialized descendant in its bounded part hierarchy. Descendants inherit ancestor motion and may add their own exact track, so a foot can carry its claws while each claw taps. Build translation, rotation, scale, opacity, timing, stagger, easing, looping, and alternating behavior from keyframes. Use exact ids from atlas_read. This action stores shared state, but it does not prove what a browser visibly rendered.",
            json!({
                "type": "object",
                "properties": {
                    "owner_id": { "type": "string" },
                    "motion": {
                        "anyOf": [
                            { "type": "null", "description": "clear the owner's motion" },
                            segment_motion_program_schema()
                        ]
                    }
                },
                "required": ["owner_id", "motion"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let owner_id = string(args, "owner_id");
                    let motion = match args.get("motion") {
                        Some(JsonValue::Null) => None,
                        Some(value) => match serde_json::from_value::<atlas::SegmentMotionProgram>(
                            value.clone(),
                        ) {
                            Ok(program) => Some(program),
                            Err(error) => {
                                return Effect::Reject(format!(
                                    "invalid segment motion program: {error}"
                                ));
                            }
                        },
                        None => return Effect::Reject("motion is required".to_string()),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let author = state.agent_author();
                        let target_count = {
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::set_segment_motion(
                                &mut scene,
                                &owner_id,
                                motion.as_ref(),
                                &author,
                            )?
                        };
                        state.persist(false);
                        let detail = motion.as_ref().map_or_else(
                            || format!("motion cleared from segment {owner_id}"),
                            |program| {
                                format!(
                                    "motion {:?} stored on segment {owner_id} for {target_count} target(s)",
                                    program.label
                                )
                            },
                        );
                        Ok(Some(format!(
                            "{detail}; saved positions were unchanged. This confirms shared state only. Inspect a real browser before claiming visible motion."
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_wings",
            "Request two real MobileSAM wing masks for an existing segment. Atlas preserves the parent object id and stores the left and right source-pixel boxes in the shared document. The browser worker reuses one WebGPU encoder result and runs one decoder pass per wing. Missing model artifacts, mock output, and fallback output are rejected.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "left_box": {
                        "type": "array",
                        "items": { "type": "number" },
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "left wing [left, top, right, bottom] in the 1536x1024 source"
                    },
                    "right_box": {
                        "type": "array",
                        "items": { "type": "number" },
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "right wing [left, top, right, bottom] in the 1536x1024 source"
                    }
                },
                "required": ["id", "left_box", "right_box"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let left_box = match number_quad(args, "left_box") {
                        Ok(values) => values,
                        Err(error) => return Effect::Reject(error),
                    };
                    let right_box = match number_quad(args, "right_box") {
                        Ok(values) => values,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let generation = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::request_segment_wings(
                                &mut scene,
                                &id,
                                left_box,
                                right_box,
                                &author,
                            )?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "segment {id} kept its Atlas identity and now awaits real MobileSAM left and right wing masks at generation {generation}"
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_segment_regenerate",
            "Request a new MobileSAM appearance generation for an existing Atlas segment id after applying a real pixel occlusion. The object id, semantic fields, transform, and provenance remain authoritative. The browser worker must attach the matching generation before status becomes materialized.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "prompt_box": {
                        "type": "array",
                        "items": { "type": "number" },
                        "minItems": 4,
                        "maxItems": 4
                    },
                    "occlusion_box": {
                        "type": "array",
                        "items": { "type": "number" },
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "opaque source-pixel patch applied before MobileSAM encoding"
                    }
                },
                "required": ["id", "prompt_box", "occlusion_box"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let prompt_box = match number_quad(args, "prompt_box") {
                        Ok(values) => values,
                        Err(error) => return Effect::Reject(error),
                    };
                    let occlusion_box = match number_quad(args, "occlusion_box") {
                        Ok(values) => values,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let generation = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            atlas::regenerate_segment(
                                &mut scene,
                                &id,
                                prompt_box,
                                occlusion_box,
                                &author,
                            )?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "segment {id} kept its Atlas identity and now awaits MobileSAM generation {generation} over the occluded source"
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_answer",
            "Answer one of the human's marks in place, so the question and \
             your reply stay attached to the thing they are about instead of \
             scrolling away in the transcript.",
            json!({
                "type": "object",
                "properties": {
                    "mark_id": { "type": "string" },
                    "answer": { "type": "string" }
                },
                "required": ["mark_id", "answer"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let (mark_id, answer) = (string(args, "mark_id"), string(args, "answer"));
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let scene = state.scene.lock();
                            atlas::answer_mark(&scene, &mark_id, &answer, &author)?;
                        }
                        state.persist(false);
                        Ok(Some(format!("answered {mark_id} on the page")))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_claim",
            "State one thing you understand about a node, with the basis for \
             it, so the human can accept or reject it in place. This is how \
             you answer a challenge mark: one claim per thing you believe, \
             never prose. `basis` is how you know: `verified` needs `path` and \
             `lines` that the host checks; `inferred` may cite a path; \
             `assumed` and `unknown` carry no source. Pass `id` to restate an \
             existing claim; an accepted claim you restate goes back to open. \
             A verified claim may pin the full commit in `revision`; the host \
             then reads that exact path and range with git. You never write a \
             verdict.",
            json!({
                "type": "object",
                "properties": {
                    "about": { "type": "string", "description": "node id the claim is about" },
                    "text": { "type": "string" },
                    "basis": { "type": "string", "enum": atlas::BASES },
                    "path": { "type": "string", "description": "repository-relative source, required for verified" },
                    "lines": { "type": "string", "description": "`12-48` or `12`, required for verified" },
                    "revision": { "type": "string", "description": "optional full 40-hex commit for a verified source" },
                    "id": { "type": "string", "description": "claim id to revise instead of creating" }
                },
                "required": ["about", "text", "basis"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let about = string(args, "about");
                    let text = string(args, "text");
                    let basis = string(args, "basis");
                    let path = string(args, "path");
                    let lines = string(args, "lines");
                    let revision = string(args, "revision");
                    let id = optional_string(args, "id");
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        if !path.trim().is_empty() {
                            state
                                .verify_source(&path, &lines, &revision)
                                .map_err(|error| format!("claim source does not hold: {error}"))?;
                        }
                        let claim_id = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let mut scene = state.scene.lock();
                            match &id {
                                Some(id) => {
                                    atlas::claims::revise_claim_at(
                                        &scene,
                                        id,
                                        &text,
                                        &basis,
                                        &path,
                                        &lines,
                                        &revision,
                                        &author,
                                    )?;
                                    id.clone()
                                }
                                None => atlas::claims::claim_at(
                                    &mut scene,
                                    &about,
                                    &text,
                                    &basis,
                                    &path,
                                    &lines,
                                    &revision,
                                    &author,
                                )?,
                            }
                        };
                        state.persist(false);
                        let atlas = state.read()?;
                        let summary = atlas::claims::summary_line(&atlas, &about)
                            .unwrap_or_else(|| format!("{about:?} carries no live claim"));
                        Ok(Some(format!("claim {claim_id} stands on the page; {summary}")))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_withdraw_claim",
            "Stop standing behind one of your claims. It stays on the page \
             struck through and stops counting toward the node being under \
             challenge. Use it when the human rejected a claim and you cannot \
             restate it with a better basis.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let about = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let scene = state.scene.lock();
                            let about = atlas::read(&scene)?
                                .claim(&id)
                                .map(|claim| claim.about.clone())
                                .ok_or_else(|| format!("no claim {id:?} on the atlas"))?;
                            atlas::withdraw_claim(&scene, &id, &author)?;
                            about
                        };
                        state.persist(false);
                        let atlas = state.read()?;
                        let summary = atlas::claims::summary_line(&atlas, &about)
                            .unwrap_or_else(|| format!("{about:?} carries no live claim"));
                        Ok(Some(format!("withdrew claim {id}; {summary}")))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_point",
            "Compatibility alias for the host semantic pointer. Point at an \
             Atlas node or drawing shape by stable id; the host resolves its semantics and the \
             browser derives the overlay without selectors or coordinates.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "phrase": { "type": "string", "description": "what you want them to notice" }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
            {
                move |args: &JsonValue| {
                    let (id, phrase) = (string(args, "id"), string(args, "phrase"));
                    Effect::Mutate(Box::new(move |surface| {
                        let service = surface.semantic_targets().ok_or_else(|| {
                            "semantic target host service is unavailable".to_string()
                        })?;
                        let resolved = service.attend(
                            service.calling_participant(),
                            AttentionMode::Pointer,
                            SemanticTargetRef::new("atlas", id),
                            Some(phrase),
                        )?;
                        Ok(Some(format!(
                            "pointing at \"{}\", the host highlighted its registered target",
                            resolved.label
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_remove",
            "Remove one node, link or mark. Removing a node also removes the \
             links and marks attached to it. Removing a container does NOT \
             remove what was inside it: each member moves up one level, into \
             the container above when there is one and to the top level \
             otherwise, and the next `atlas_read` names each member and where \
             it landed.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let removed = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let scene = state.scene.lock();
                            atlas::remove(&scene, &id)?
                        };
                        state.persist(true);
                        Ok(Some(format!("removed {removed} object(s)")))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "repo_list",
            "List one directory of the project this atlas is about. Use it to \
             find real files before you claim anything about them.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "repository-relative directory; empty for the root" } },
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let path = string(args, "path");
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        let entries = state.repo()?.list(&path)?;
                        if entries.is_empty() {
                            return Ok(format!("{path:?} is empty"));
                        }
                        Ok(entries
                            .iter()
                            .map(|entry| {
                                if entry.is_dir {
                                    format!("{}/", entry.name)
                                } else {
                                    entry.name.clone()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n"))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "repo_read",
            "Read real text from the project, optionally narrowed to a line \
             range. This is the same reader the human's source panel uses, so \
             what you quote and what they see are the same bytes.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "lines": { "type": "string", "description": "N or N-M; omit for the whole file" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let (path, lines) = (string(args, "path"), string(args, "lines"));
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        let excerpt = state.repo()?.read(&path, &lines)?;
                        let mut out = format!("{}:{}\n", excerpt.path, excerpt.lines);
                        for (offset, line) in excerpt.text.lines().enumerate() {
                            out.push_str(&format!("{:>5} {line}\n", excerpt.first_line + offset));
                        }
                        if excerpt.truncated {
                            out.push_str("… (truncated by the surface)\n");
                        }
                        Ok(out)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_map_mode_get",
            "Read the atlas's active Map mode: `explain` (claimed cards only, \
             nothing verified), `map` (every card renders its computed trust \
             tier; extractor lanes nothing claims draw themselves as \
             undeclared), or `cement` (export the agreed Map to G8 \
             obligations). Shared state: whatever a human last set in the \
             header, or another agent last set here, is what this reads.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| Ok(state.atlas_mode())))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_map_mode_set",
            "Switch the atlas's active Map mode. See `atlas_map_mode_get` for \
             what each of `explain`, `map`, and `cement` means. Switching \
             never deletes anything: it only changes what renders.",
            json!({
                "type": "object",
                "properties": { "mode": { "type": "string", "enum": tier::MODES } },
                "required": ["mode"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let mode = string(args, "mode");
                    let state = state.clone();
                    // A mode switch touches the sidecar tier registry, not
                    // the CRDT scene, so this is a query effect: no document
                    // update fires, and none should.
                    Effect::Query(Box::new(move |_| {
                        state.set_atlas_mode(&mode)?;
                        Ok(format!("mode is now {mode}"))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_claim_lane",
            "Claim an extractor-found lane for an existing card: ADDS the \
             lane's evidence file and line to that card's binding set (a \
             card is not one file; 'API server' can bind a port and vouch \
             for the sidecar it spawns at once), and pins a fresh hash on \
             that binding so drift can be detected from here on. This is the \
             only way an `undeclared` card (a lane `GET /atlas/lanes` found \
             with no claiming card) becomes `verified`, without disturbing \
             any binding the card already had. The lane must still be \
             evident in the current scan; a stale lane id is refused rather \
             than silently bound. Does not touch the CRDT scene: this is \
             bookkeeping about which lanes a card vouches for, not a draw.",
            json!({
                "type": "object",
                "properties": {
                    "card_id": { "type": "string", "description": "an existing node id to bind" },
                    "lane_id": { "type": "string", "description": "a lane_id from the current GET /atlas/lanes" }
                },
                "required": ["card_id", "lane_id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let card_id = string(args, "card_id");
                    let lane_id = string(args, "lane_id");
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        state.claim_lane(&card_id, &lane_id)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_map_cement",
            "Export the current Map to G8 obligations at the project root: \
             one obligation per verified or proposed card, plus one removal \
             obligation per card struck by a live `replaces` edge. Every \
             obligation this writes starts red on purpose (unratified, \
             unattested); that is the human's next step with the `g8` CLI, \
             not something this tool does for them. Returns the path written \
             and which cards got an obligation.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    // Writes a file at the project root and the sidecar
                    // registry; the CRDT scene itself is untouched.
                    Effect::Query(Box::new(move |_| {
                        let result = state.cement_map()?;
                        Ok(result.to_string())
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_remove_lane",
            "Cement a removal obligation for an extractor-found lane instead \
             of claiming it: asserts, in G8, that this exact code must no \
             longer exist. The lane's undeclared card turns into a removal \
             gate, red (`REMOVE unmet`) until the code is actually deleted \
             and G8 confirms it, then green (`REMOVE met`). This is the \
             second of the two actions an undeclared card offers, alongside \
             `atlas_claim_lane`. `reason` is prose for the obligation's own \
             note field, e.g. why this lane should not exist.",
            json!({
                "type": "object",
                "properties": {
                    "lane_id": { "type": "string", "description": "a lane_id from the current GET /atlas/lanes" },
                    "reason": { "type": "string", "description": "why this lane should be removed" }
                },
                "required": ["lane_id"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let lane_id = string(args, "lane_id");
                    let reason = string(args, "reason");
                    let state = state.clone();
                    // Writes a file at the project root and the sidecar
                    // registry; the CRDT scene itself is untouched.
                    Effect::Query(Box::new(move |_| {
                        let result = state.remove_lane(&lane_id, &reason)?;
                        Ok(result.to_string())
                    }))
                }
            },
        )
        .agent_only(),
    ];

    if state.agent_ink_enabled() {
        defs.extend(agent_ink_actions(state));
    }
    page_validation::validate_writes(&mut defs, state);
    defs
}

// ── agent ink actions ──────────────────────────────────────────────────
//
// Everything below is gated by `AGUI_AGENT_INK` (default off) and reaches
// the numeric layer in `same_page_atlas_core::agent_ink` purely through its
// public API: nothing under `core/src/agent_ink/` is touched by this file.

/// Every refusal below is the real message the settled layer returned,
/// echoed the same way `constrain` echoes a refused constraint: never
/// swallowed and never replaced with a generic failure.
fn agent_ink_actions(state: &Arc<AtlasState>) -> Vec<ToolDef> {
    // "measured" is a real member of VAR_STATES — the read-back and the
    // wire format both need to say it — but it is not a state
    // `atlas_variable_create` can ever produce (a measured variable has no
    // caller-supplied value to create it with; see
    // `atlas_variable_measure_create` below), so it is left out of this
    // tool's own enum rather than advertised as a choice that always fails.
    let var_states = atlas::VAR_STATES
        .iter()
        .filter(|state| **state != "measured")
        .map(|state| JsonValue::from(*state))
        .collect::<Vec<_>>();
    let measure_kinds = atlas::agent_ink::MEASURE_KINDS
        .iter()
        .map(|kind| JsonValue::from(*kind))
        .collect::<Vec<_>>();
    let relation_ops = atlas::agent_ink::RELATION_OPS
        .iter()
        .map(|op| JsonValue::from(*op))
        .collect::<Vec<_>>();

    let agent_ink_modes = AGENT_INK_MODES
        .iter()
        .map(|mode| JsonValue::from(*mode))
        .collect::<Vec<_>>();

    vec![
        ToolDef::new(
            "atlas_mode_set",
            "Set the agent-ink layer's document-wide mode: \"learning\" (the \
             default), where a step is a re-derivable frame in an \
             explanation, or \"alignment\", where a step is a commitment and \
             a pinned edit made between steps survives into the next one. \
             This is refused once any step has been recorded — flipping the \
             mode of a document that already has a timeline would turn a \
             brainstorm sketch into a signed record, or the reverse, without \
             anyone re-examining what was captured under the old rules. \
             Call atlas_variables_read to see the current mode; \
             atlas_mode_promote_to_alignment is the tool that promotes an \
             existing learning-mode timeline into alignment mode, and it is \
             human-only.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "mode": { "type": "string", "enum": agent_ink_modes }
                },
                "required": ["mode"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let mode = match agent_ink_mode_input(args) {
                        Ok(mode) => mode,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        set_agent_ink_mode(state.as_ref(), mode).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_mode_promote_to_alignment",
            "Promote this document's agent-ink timeline from learning mode \
             into alignment mode. This is a human act, on purpose: it is the \
             only tool in this catalog an agent cannot call at all, because \
             promoting a timeline is the moment a sketch becomes something \
             this surface will treat as a signed record, and that moment \
             needs a human standing there. Every stored step is re-solved \
             from its own recorded values and re-recorded under alignment \
             semantics — not merely relabeled — and if any step cannot be \
             re-solved to a converged status, the WHOLE promotion is \
             refused, naming which steps failed and why, rather than \
             promoting a partially-checked timeline. This is refused \
             outright if the document is already in alignment mode, or has \
             no recorded step yet (atlas_mode_set covers that case \
             directly). It is one-way: atlas_mode_set still refuses to \
             change the mode of a document with a step in it, in either \
             direction, so promotion cannot be undone by asking for the \
             opposite mode. The promoted document records that it was \
             promoted, from which mode, and by whom, in atlas_variables_read \
             and in every atlas_step_export_citable export going forward.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        promote_agent_ink_to_alignment(state.as_ref()).map(Some)
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "atlas_variable_create",
            "Create a named numeric variable in the agent-ink layer. `state` \
             decides who may change its value afterward: pinned and scrubbing \
             are yours or the human's to set directly with atlas_variable_set, \
             free is solver-owned and only changes from atlas_solve, and \
             derived follows a linear relation you declare separately with \
             atlas_relation_create. `represents_object`/`represents_property` \
             optionally tie the number to an existing atlas node's field; \
             give both or neither. Names are document-wide and case-sensitive; \
             a duplicate is refused rather than silently reused.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string" },
                    "value": { "type": "number" },
                    "state": { "type": "string", "enum": var_states },
                    "represents_object": { "type": "string" },
                    "represents_property": { "type": "string" },
                    "unit": { "type": "string" }
                },
                "required": ["name", "value", "state"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let inputs = match variable_create_input(args) {
                        Ok(inputs) => inputs,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        create_agent_ink_variable(state.as_ref(), inputs).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_state_bind",
            "Bind one agent-ink variable to a state machine as its current \
             state. The machine may be its stable id or its unique visible \
             title. State order is the order atlas_read gives for the \
             machine, with 0 selecting the first state. Optionally pass an \
             exact state name to set a pinned or scrubbing variable while \
             binding it. Fractional, negative, and out-of-range values are \
             refused, as is a state name the machine does not contain or a \
             second variable bound to the same machine. The reply reads the \
             resulting state back in domain terms and names the transition \
             when exactly one connects the prior state to the current one.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "machine": { "type": "string" },
                    "variable": { "type": "string" },
                    "state": { "type": "string" }
                },
                "required": ["machine", "variable"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let machine = string(args, "machine");
                    let variable = string(args, "variable");
                    let requested_state = optional_string(args, "state");
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let sentence = {
                            let author = state.agent_author();
                            let _commit = state.begin_commit(author.as_str());
                            let scene = state.scene.lock();
                            atlas::living_ink::bind_state(
                                &scene,
                                &machine,
                                &variable,
                                requested_state.as_deref(),
                                &author,
                            )?
                        };
                        state.persist(false);
                        Ok(Some(format!(
                            "bound variable {variable:?} to state machine {machine:?}\n{sentence}"
                        )))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_variable_measure_create",
            "Create a named numeric variable whose value is MEASURED from \
             the drawing rather than declared: it reports a geometric fact \
             about existing shapes or nodes and stays correct as they move, \
             instead of holding whatever number a caller last wrote. There \
             is no `value` to give it and no `represents` binding — \
             `measure_kind` and `objects` say what to measure and about \
             which existing shape or node ids. `gap` and `center_distance` \
             each need exactly 2 objects: `gap` is the clear distance \
             between their nearest edges (zero if they touch or overlap), \
             `center_distance` is the distance between their centres. \
             `width`/`height` take 1 or more objects and report the \
             bounding box spanning all of them. `angle` takes 1 object (a \
             line or arrow shape with at least 2 points, measured along its \
             own endpoints) or 2 (the bearing between their centres), in \
             radians. The state is always `measured`: it can be related to \
             other variables through atlas_relation_create like any other \
             (gap/center_distance/width/height carry unit `px`, angle \
             carries `rad`, so relating one to a physical length is refused \
             the same way any px-vs-length relation already is), but never \
             written directly with atlas_variable_set, and never captured \
             from a drag — dragging one of its objects changes what it \
             reports on the very next read, with nothing else to do. If an \
             object it names is later deleted, atlas_variables_read reports \
             it as DANGLING rather than a fabricated number.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string" },
                    "measure_kind": { "type": "string", "enum": measure_kinds },
                    "objects": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["name", "measure_kind", "objects"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let inputs = match variable_measure_create_input(args) {
                        Ok(inputs) => inputs,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        create_measured_agent_ink_variable(state.as_ref(), inputs).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_variable_set",
            "Write a new value into an existing pinned or scrubbing variable. \
             A free variable is solver-owned and a derived variable follows \
             its linear relation, so both are refused here with the real \
             reason rather than a silent no-op; run atlas_solve or change the \
             relation instead.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string" },
                    "value": { "type": "number" }
                },
                "required": ["name", "value"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let (name, value) = match variable_set_input(args) {
                        Ok(input) => input,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        set_agent_ink_variable(state.as_ref(), &name, value).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_variable_capture",
            "Read a shape or node property's current value back into the \
             agent-ink variable bound to it by represents_object/\
             represents_property, exactly the way a human drag is meant to \
             be captured. `object`/`property` name the binding; `value` is \
             wherever the property is now. Nothing represents an unbound \
             object/property pair, so that case is reported, not refused. A \
             binding onto a free or derived variable IS refused, by name, \
             the same way atlas_variable_set refuses a direct write, because \
             that value is solver-owned. This never re-solves by itself; \
             call atlas_solve afterward to propagate the captured value.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "object": { "type": "string" },
                    "property": { "type": "string" },
                    "value": { "type": "number" }
                },
                "required": ["object", "property", "value"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let (object, property, value) = match variable_capture_input(args) {
                        Ok(input) => input,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        capture_agent_ink_variable(state.as_ref(), &object, &property, value)
                            .map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_relation_create",
            "Declare a named numeric relation over existing agent-ink \
             variables, enforced by atlas_solve. `distance` takes \
             [ax, ay, bx, by, target]; `angle` takes \
             [ax, ay, pivot_x, pivot_y, bx, by, target]; `equal` and `linear` \
             take two members; `formula` takes one or more. For `linear`, the \
             first member is the dependent quantity and the second is the \
             source in `dependent = m * source + b`; `m` and `b` are required \
             for `linear` and refused for every other op. Declaring a linear \
             relation also makes its dependent variable derived, so it must \
             start out free. `formula` follows the same dependent-first \
             convention: the first member is the quantity `expression` \
             predicts and every other member is an input the expression may \
             reference by name; the solver drives \
             `expression(other members) - first member` to zero, so unlike \
             `linear` the first member is not forced into a derived state. \
             `expression` supports +, -, *, /, unary minus, parentheses, `^`, \
             and the functions exp, ln, log10, sqrt, abs, sin, cos, tan; it \
             is required for `formula` and refused for every other op, and \
             any variable it references that is not one of this relation's \
             other members is refused by name at creation time. A variable \
             name with a space in it (agent-ink names routinely have one, \
             e.g. \"barrier width\") cannot be written bare — wrap it in \
             backticks inside the expression, e.g. \
             `120 * exp(-0.03 * `barrier width`)`; a bare name never needs \
             this. The read-back shows `expression` exactly as written, \
             backticks included.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string" },
                    "op": { "type": "string", "enum": relation_ops },
                    "members": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "type": "string" }
                    },
                    "m": { "type": "number" },
                    "b": { "type": "number" },
                    "expression": { "type": "string" }
                },
                "required": ["name", "op", "members"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let inputs = match relation_create_input(args) {
                        Ok(inputs) => inputs,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        create_agent_ink_relation(state.as_ref(), inputs).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_solve",
            "Run the numeric solver over every free and derived variable \
             against every declared relation, then report the real outcome: \
             converged, exhausted (no values committed), or relaxed (some \
             relations dropped to reach a stationary point), plus any replica \
             divergence recorded against another seat's solve. This never \
             reports converged when it did not; read the returned text for \
             the actual status before trusting the numbers.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        run_agent_ink_solve(state.as_ref()).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_step_record",
            "Persist the current solve at the value held by the document's \
             unique scrubbing variable. The variable's name is not special. \
             This is the step nothing else can create: atlas_solve \
             does not by itself put a point on the timeline, and \
             atlas_step_advance can only move forward from a step that is \
             already recorded. Run atlas_solve first. This refuses to \
             record before the document has a solve state — then call this \
             once to seed step 0 (or whatever the index currently is) before \
             the first atlas_step_advance or atlas_step_scrub. Re-recording \
             the exact same values and status the step already has is a \
             no-op; recording something different for an already-stored step \
             is refused.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        record_agent_ink_step(state.as_ref()).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_step_advance",
            "Advance the agent-ink timeline by one step on the document's \
             unique scrubbing variable: replay the previously recorded step \
             as the warm start, move the index forward by one, and solve. If the next \
             step was already recorded, this replays it instead of solving \
             again. The current step must already be recorded with \
             atlas_step_record before it can be advanced past.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        advance_agent_ink_step(state.as_ref()).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_step_scrub",
            "Restore a previously recorded agent-ink step (recorded by \
             atlas_step_record or atlas_step_advance) byte for byte, on the \
             document's unique scrubbing variable. This replays exactly what was stored; \
             it never re-invokes the solver, so it cannot change a step's \
             recorded status or values.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "index": { "type": "integer", "minimum": 0 }
                },
                "required": ["index"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let index = match step_index_input(args) {
                        Ok(index) => index,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        scrub_agent_ink_step(state.as_ref(), index).map(Some)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_step_export_citable",
            "Export a recorded agent-ink step as a citable record: a stable, \
             checkable identity, its exact variable values and units, its full \
             solve outcome (including an exhausted or relaxed status and any \
             dropped relations — never filtered out), the document's mode, \
             who first recorded it, and — when the document was promoted \
             rather than born in alignment mode — what it was promoted from \
             and by whom. This is refused for any step recorded while the \
             document is still in learning mode, where a step is a \
             re-derivable frame rather than a signed commitment; call \
             atlas_mode_promote_to_alignment first. The identity is a \
             content hash, not an id or a cursor, so an outside caller can \
             re-derive it from a fresh export and confirm the cited step has \
             not moved.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "index": { "type": "integer", "minimum": 0 }
                },
                "required": ["index"]
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let index = match step_index_input(args) {
                        Ok(index) => index,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        export_citable_agent_ink_step(state.as_ref(), index)
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_variables_read",
            "Read every agent-ink variable and the current solve status, \
             exactly as it appears in atlas_read's VARIABLES section: SOLVE \
             NOT RUN before the first solve, SOLVE EXHAUSTED or SOLVE RELAXED \
             with the real diagnostics when the solver did not fully \
             converge, and REPLICA DIVERGENCE when another seat's solve \
             disagreed. Call this instead of atlas_read when you only need \
             the numeric layer.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        let atlas = state.read()?;
                        let text = agent_ink_readback(&atlas);
                        if text.is_empty() {
                            Ok("no agent-ink variables yet".to_string())
                        } else {
                            Ok(text)
                        }
                    }))
                }
            },
        )
        .agent_only(),
    ]
}

// ── the extension ─────────────────────────────────────────────────────────

pub struct AtlasExtension {
    state: Arc<AtlasState>,
    actions: Vec<ToolDef>,
}

impl AtlasExtension {
    pub fn new(state: Arc<AtlasState>) -> Self {
        let actions = actions(&state);
        Self { state, actions }
    }
}

impl Extension for AtlasExtension {
    fn id(&self) -> &str {
        "atlas"
    }

    /// Remember who is writing, so the map records *which* agent drew a node
    /// rather than only that "an agent" did.
    fn note_caller(&self, actor: &ag_ui_surface::Actor) {
        *self.state.actor_label.lock() = actor.label.clone();
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn state(&self) -> &dyn SurfaceState {
        self.state.as_ref()
    }

    fn actions(&self) -> &[ToolDef] {
        &self.actions
    }

    fn client_module(&self) -> Option<ClientModule> {
        let mut module =
            ClientModule::lazy("atlas", "0.1.0", "/extensions/atlas/index.js", "atlas-view")
                .wasm(WasmMount {
                    pkg_name: "same_page_atlas_web".to_string(),
                    js: "/pkg/same_page_atlas_web.js".to_string(),
                    wasm: "/pkg/same_page_atlas_web_bg.wasm".to_string(),
                })
                .capability("dom")
                .capability("wasm")
                .capability("webgpu")
                .capability("crdt-sync");
        // Cement and promotion are the two human-only actions. Cement is
        // always available because claims are base Atlas state. Promotion is
        // declared only when the optional agent-ink vocabulary is registered.
        module = module
            .action("atlas_cement_decision")
            .action("atlas_decision_preview")
            .action("atlas_decisions_read");
        if self.state.agent_ink_enabled() {
            module = module.action("atlas_mode_promote_to_alignment");
        }
        Some(module)
    }

    fn capabilities(&self) -> Vec<&str> {
        vec!["dom", "wasm", "webgpu", "crdt-sync"]
    }

    /// Atlas is *about* a repository: `repo_list`, `repo_read`, `atlas_import`
    /// and every source-backed node's verification read through it. Before this
    /// was declared, `main` constructed a `Repo` and handed it to
    /// `AtlasState::open`, so `agui.app.toml` described an extension with no
    /// filesystem reach at all.
    fn requires_services(&self) -> Vec<ServiceRequirement> {
        vec![ServiceRequirement::new(
            "repository",
            "reads and verifies the source a node claims to be about",
        )]
    }

    fn bind_services(&self, registry: &ServiceRegistry) -> Result<(), String> {
        self.state
            .bind_repo(registry.resolve::<Repo>("repository")?)
    }

    fn bind_semantic_targets(&self, service: &Arc<SemanticTargetService>) {
        // Set once. A second install would mean two attention hosts, and the
        // read-back would then depend on which one answered first.
        let bound = self.state.attention_host.set(service.clone()).is_ok();
        tracing::info!(bound, "atlas bound the attention host");
    }

    fn focus_events(&self) -> &[&str] {
        &[FOCUS_EVENT]
    }

    fn semantic_targets(&self) -> bool {
        true
    }

    fn binary_transport(&self) -> bool {
        true
    }

    fn routes(&self) -> Vec<RouteDef> {
        let validation_read_state = self.state.clone();
        let validation_post_state = self.state.clone();
        let source_state = self.state.clone();
        let files_state = self.state.clone();
        let review_state = self.state.clone();
        let describe_state = self.state.clone();
        let export_state = self.state.clone();
        let attention_state = self.state.clone();
        let segment_state = self.state.clone();
        let agent_ink_state = self.state.clone();
        let agent_ink_citable_state = self.state.clone();
        vec![
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/validation",
                handler: Box::new(move |_| {
                    let state = validation_read_state.clone();
                    Box::pin(async move {
                        match state.validation_report() {
                            Ok(report) => RouteResponse::json(200, report),
                            Err(error) => RouteResponse::json(500, json!({"error": error})),
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Post,
                path: "/atlas/validation",
                handler: Box::new(move |request: RouteRequest| {
                    let state = validation_post_state.clone();
                    Box::pin(async move {
                        match state.record_validation(request.body) {
                            Ok(report) => RouteResponse::json(200, report),
                            Err(error) => RouteResponse::json(400, json!({"error": error})),
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/review",
                handler: Box::new(move |_request: RouteRequest| {
                    let state = review_state.clone();
                    Box::pin(async move {
                        let result = (|| -> Result<JsonValue, String> {
                            let (atlas, revision) = state.read_with_revision()?;
                            let repo = state.repo()?.clone();
                            let mut tiers = state.tiers.lock();
                            Ok(crate::review::inspect_with_tiers(
                                &repo,
                                &atlas,
                                &revision,
                                &mut tiers,
                                &state.tiers_path,
                            ))
                        })();
                        match result {
                            Ok(review) => {
                                RouteResponse::json(200, json!({"ok":true,"review":review}))
                            }
                            Err(error) => {
                                RouteResponse::json(400, json!({"ok":false,"error":error}))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/lanes",
                handler: Box::new({
                    let state = self.state.clone();
                    move |_request: RouteRequest| {
                        let state = state.clone();
                        Box::pin(async move {
                            match state.lanes_report() {
                                Ok(report) => RouteResponse::json(200, report),
                                Err(error) => {
                                    RouteResponse::json(400, json!({"ok":false,"error":error}))
                                }
                            }
                        })
                    }
                }),
            },
            RouteDef {
                method: HttpMethod::Post,
                // The browser's half of `atlas_claim_lane`: the MCP tool is
                // agent-only, but a human looking at an undeclared card in
                // Map mode needs the same action, not a request relayed
                // through an agent. Both paths call the identical
                // `AtlasState::claim_lane`.
                path: "/atlas/lanes/claim",
                handler: Box::new({
                    let state = self.state.clone();
                    move |request: RouteRequest| {
                        let state = state.clone();
                        Box::pin(async move {
                            let body = request.body.unwrap_or(JsonValue::Null);
                            let card_id = body["card_id"].as_str().unwrap_or("").to_string();
                            let lane_id = body["lane_id"].as_str().unwrap_or("").to_string();
                            match state.claim_lane(&card_id, &lane_id) {
                                Ok(summary) => {
                                    RouteResponse::json(200, json!({"ok":true,"summary":summary}))
                                }
                                Err(error) => {
                                    RouteResponse::json(400, json!({"ok":false,"error":error}))
                                }
                            }
                        })
                    }
                }),
            },
            RouteDef {
                method: HttpMethod::Post,
                // The browser's half of `atlas_remove_lane`: the MCP tool is
                // agent-only, but a human deciding an undeclared lane should
                // not exist needs the same action available directly.
                path: "/atlas/lanes/remove",
                handler: Box::new({
                    let state = self.state.clone();
                    move |request: RouteRequest| {
                        let state = state.clone();
                        Box::pin(async move {
                            let body = request.body.unwrap_or(JsonValue::Null);
                            let lane_id = body["lane_id"].as_str().unwrap_or("").to_string();
                            let reason = body["reason"].as_str().unwrap_or("").to_string();
                            match state.remove_lane(&lane_id, &reason) {
                                Ok(result) => RouteResponse::json(200, json!({"ok":true,"result":result})),
                                Err(error) => {
                                    RouteResponse::json(400, json!({"ok":false,"error":error}))
                                }
                            }
                        })
                    }
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/mode",
                handler: Box::new({
                    let state = self.state.clone();
                    move |_request: RouteRequest| {
                        let state = state.clone();
                        Box::pin(async move {
                            let obligations_count = state
                                .repo()
                                .ok()
                                .map(|repo| repo.root().join("specs/obligations-v0.1.json"))
                                .and_then(|path| std::fs::read(path).ok())
                                .and_then(|bytes| serde_json::from_slice::<JsonValue>(&bytes).ok())
                                .and_then(|document| {
                                    document["obligations"].as_array().map(Vec::len)
                                })
                                .unwrap_or(0);
                            RouteResponse::json(
                                200,
                                json!({"ok":true,"mode":state.atlas_mode(),"obligations_count":obligations_count}),
                            )
                        })
                    }
                }),
            },
            RouteDef {
                method: HttpMethod::Post,
                path: "/atlas/mode",
                handler: Box::new({
                    let state = self.state.clone();
                    move |request: RouteRequest| {
                        let state = state.clone();
                        Box::pin(async move {
                            let mode = request
                                .body
                                .as_ref()
                                .and_then(|body| body["mode"].as_str())
                                .unwrap_or("")
                                .to_string();
                            match state.set_atlas_mode(&mode) {
                                Ok(()) => RouteResponse::json(200, json!({"ok":true,"mode":mode})),
                                Err(error) => {
                                    RouteResponse::json(400, json!({"ok":false,"error":error}))
                                }
                            }
                        })
                    }
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/segments",
                handler: Box::new(move |_request: RouteRequest| {
                    let state = segment_state.clone();
                    Box::pin(async move {
                        match state.read() {
                            Ok(atlas) => RouteResponse::json(
                                200,
                                json!({
                                    "ok": true,
                                    "segments": atlas
                                        .shapes
                                        .into_iter()
                                        .filter(|shape| shape.form == "segment")
                                        .collect::<Vec<_>>(),
                                }),
                            ),
                            Err(error) => {
                                RouteResponse::json(500, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                // Export is a read, so it is a route rather than an action:
                // anything that can reach the page can have the drawing.
                // *Import* is not here on purpose, it is a write, and the
                // human's writes go through their own replica so they land as
                // theirs. The agent's half is the `atlas_import` action.
                path: "/atlas/excalidraw",
                handler: Box::new(move |_request: RouteRequest| {
                    let state = export_state.clone();
                    Box::pin(async move {
                        match state.read() {
                            Ok(atlas) => RouteResponse::json(
                                200,
                                json!({
                                    "ok": true,
                                    "document": atlas::excalidraw::to_excalidraw(&atlas),
                                }),
                            ),
                            Err(error) => {
                                RouteResponse::json(500, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/source",
                handler: Box::new(move |request: RouteRequest| {
                    let state = source_state.clone();
                    Box::pin(async move {
                        let path = request.query.get("path").cloned().unwrap_or_default();
                        let lines = request.query.get("lines").cloned().unwrap_or_default();
                        match state.repo().and_then(|repo| repo.read(&path, &lines)) {
                            Ok(excerpt) => {
                                let (entry_points, entry_points_error) =
                                    match state.repo().and_then(|repo| repo.entry_points(&path)) {
                                        Ok(paths) => (paths, None),
                                        Err(error) => (Vec::new(), Some(error)),
                                    };
                                RouteResponse::json(
                                    200,
                                    json!({
                                        "ok": true,
                                        "path": excerpt.path,
                                        "lines": excerpt.lines,
                                        "first_line": excerpt.first_line,
                                        "total_lines": excerpt.total_lines,
                                        "text": excerpt.text,
                                        "truncated": excerpt.truncated,
                                        "entry_points": entry_points,
                                        "entry_points_error": entry_points_error,
                                    }),
                                )
                            }
                            Err(error) => {
                                RouteResponse::json(400, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/files",
                handler: Box::new(move |request: RouteRequest| {
                    let state = files_state.clone();
                    Box::pin(async move {
                        let path = request.query.get("path").cloned().unwrap_or_default();
                        match state.repo().and_then(|repo| repo.list(&path)) {
                            Ok(entries) => RouteResponse::json(
                                200,
                                json!({"ok": true, "path": path, "entries": entries}),
                            ),
                            Err(error) => {
                                RouteResponse::json(400, json!({"ok": false, "error": error}))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Post,
                path: "/atlas/attention",
                handler: Box::new(move |request: RouteRequest| {
                    let state = attention_state.clone();
                    Box::pin(async move {
                        match state.record_attention(request.body) {
                            Ok(sequence) => RouteResponse::json(
                                200,
                                json!({ "ok": true, "sequence": sequence }),
                            ),
                            Err(error) => {
                                RouteResponse::json(400, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                path: "/atlas/describe",
                handler: Box::new(move |_request: RouteRequest| {
                    let state = describe_state.clone();
                    Box::pin(async move {
                        match state.read() {
                            // Exactly what `atlas_read` hands the model, so the
                            // browser can diff it against its own replica's
                            // read-back and show whether the two agree.
                            Ok(atlas) => RouteResponse::json(
                                200,
                                json!({ "ok": true, "text": atlas.describe() }),
                            ),
                            Err(error) => {
                                RouteResponse::json(500, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                // The browser has no other way to learn whether the running
                // process was booted with `AGUI_AGENT_INK` on: the flag is
                // read once at process startup (see `main.rs`), so both seats
                // toggle their panel together only if they ask the server
                // rather than guessing from build-time configuration.
                path: "/atlas/agent-ink",
                handler: Box::new(move |_request: RouteRequest| {
                    let state = agent_ink_state.clone();
                    Box::pin(async move {
                        if !state.agent_ink_enabled() {
                            return RouteResponse::json(200, json!({ "enabled": false }));
                        }
                        let scene = state.scene.lock();
                        match atlas::read(&scene) {
                            Ok(atlas) => match agent_ink_step_summary(&scene, &atlas) {
                                Ok((index, count)) => RouteResponse::json(
                                    200,
                                    json!({
                                        "enabled": true,
                                        "readback": agent_ink_readback(&atlas),
                                        "variables": atlas.variables,
                                        "step": { "index": index, "count": count },
                                    }),
                                ),
                                Err(error) => {
                                    RouteResponse::json(500, json!({ "ok": false, "error": error }))
                                }
                            },
                            Err(error) => {
                                RouteResponse::json(500, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
            RouteDef {
                method: HttpMethod::Get,
                // Additive alongside `/atlas/agent-ink`'s JSON contract: a
                // human or an external tool can fetch one step's citable
                // export the same way an agent calls `atlas_step_export_citable`,
                // without needing MCP. `?index=N` selects the step; a missing
                // or malformed index is a 400, not a guess at step 0.
                path: "/atlas/agent-ink/citable",
                handler: Box::new(move |request: RouteRequest| {
                    let state = agent_ink_citable_state.clone();
                    Box::pin(async move {
                        if !state.agent_ink_enabled() {
                            return RouteResponse::json(200, json!({ "enabled": false }));
                        }
                        let index = match request
                            .query
                            .get("index")
                            .map(|value| value.parse::<u32>())
                        {
                            Some(Ok(index)) => index,
                            Some(Err(_)) => {
                                return RouteResponse::json(
                                    400,
                                    json!({ "ok": false, "error": "index must be a whole number, 0 or greater" }),
                                );
                            }
                            None => {
                                return RouteResponse::json(
                                    400,
                                    json!({ "ok": false, "error": "index query parameter is required" }),
                                );
                            }
                        };
                        match citable_agent_ink_step_document(state.as_ref(), index) {
                            Ok(document) => RouteResponse::json(
                                200,
                                json!({ "ok": true, "enabled": true, "citable": document }),
                            ),
                            Err(error) => {
                                RouteResponse::json(400, json!({ "ok": false, "error": error }))
                            }
                        }
                    })
                }),
            },
        ]
    }
}

/// Cement is the base Atlas catalog's one human action. Every other base action
/// is agent-only; optional agent-ink adds its own human promotion action.
#[allow(dead_code)]
fn assert_base_action_audiences(defs: &[ToolDef]) -> bool {
    defs.iter().all(|def| {
        if matches!(
            def.name.as_str(),
            "atlas_cement_decision" | "atlas_decision_preview" | "atlas_decisions_read"
        ) {
            def.audience == ActionAudience::Human
        } else {
            def.audience == ActionAudience::Agent
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{BoardExtension, BoardState};
    use ag_ui_surface::{AsyncQueryEffect, Surface};
    use same_page_atlas_core::camera;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::broadcast;

    fn state(dir: &std::path::Path) -> (Arc<AtlasState>, broadcast::Receiver<Vec<u8>>) {
        state_rooted(dir, std::path::Path::new(env!("CARGO_MANIFEST_DIR")), false)
    }

    /// The same, with `AGUI_AGENT_INK` on, for the B4 flag tests.
    fn state_with_agent_ink(
        dir: &std::path::Path,
    ) -> (Arc<AtlasState>, broadcast::Receiver<Vec<u8>>) {
        state_rooted(dir, std::path::Path::new(env!("CARGO_MANIFEST_DIR")), true)
    }

    /// The same, over a repository root the test controls, so a test that
    /// needs a file to exist can make one without writing into the source tree.
    fn state_rooted(
        dir: &std::path::Path,
        root: &std::path::Path,
        agent_ink: bool,
    ) -> (Arc<AtlasState>, broadcast::Receiver<Vec<u8>>) {
        let (ws_tx, ws_rx) = broadcast::channel(32);
        let (sse_tx, _) = broadcast::channel(32);
        let transport = Transport {
            ws_tx,
            sse_tx,
            history: Arc::new(Mutex::new(VecDeque::new())),
            awaiting: Arc::new(AtomicBool::new(false)),
            transcript_replay_lock: Arc::new(Mutex::new(())),
        };
        let repo = Arc::new(Repo::open(root).expect("repo"));
        let state = AtlasState::open(transport, dir.join("atlas.json"), agent_ink).expect("state");
        // The host binds this during composition; a unit test constructing the
        // state directly binds it the same way rather than through a
        // constructor argument that no longer exists.
        state.bind_repo(repo).expect("bind the repository service");
        (state, ws_rx)
    }

    #[test]
    fn page_validation_returns_post_write_errors_with_applied_ids() {
        let dir = temp_dir("validation-post-write");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let first = place(&state, &defs, "first", 0.0, 0.0);
        let second = place(&state, &defs, "second", 400.0, 0.0);
        let error = call(
            &state,
            &defs,
            "atlas_place",
            json!({"id": second, "x": 100.0}),
        )
        .expect_err("overlap must be a tool error");
        let receipt: JsonValue = serde_json::from_str(&error).unwrap();
        assert_eq!(receipt["operation_applied"], true);
        assert_eq!(receipt["validation"]["status"], "failed");
        assert!(error.contains(&first) && error.contains(&second));
        assert_eq!(state.read().unwrap().nodes.len(), 2);
        assert!(call(&state, &defs, "atlas_validate", json!({})).is_err());
        call(
            &state,
            &defs,
            "atlas_place",
            json!({"id": second, "x": 400.0}),
        )
        .unwrap();
        assert_eq!(state.validation_report().unwrap()["status"], "pending");
    }

    #[tokio::test]
    async fn page_validation_browser_exception_wakes_unified_model_wait_without_document_write() {
        let dir = temp_dir("validation-browser-wake");
        let (state, _ws) = state(&dir);
        let board = BoardState::open(
            state.transport.clone(),
            dir.join("board.json"),
            state.clone(),
        )
        .unwrap();
        let extension = BoardExtension::new(board);
        let action = extension
            .actions()
            .iter()
            .find(|action| action.name == "await_input")
            .unwrap();
        let Effect::AsyncQuery(waiting) = (action.apply)(&json!({"seconds": 30})) else {
            panic!("async waiter")
        };
        let waiting = tokio::spawn(waiting);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiting.is_finished());
        let before = state.scene.lock().encode_full().unwrap();
        state
            .record_validation(Some(json!({
                "client_id": "browser", "sequence": 1, "revision": null,
                "errors": [{"code": "browser_exception", "message": "renderer failed"}],
            })))
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("renderer failed") && error.contains("PAGE_VALIDATION_FAILED"));
        assert_eq!(before, state.scene.lock().encode_full().unwrap());
    }

    #[tokio::test]
    async fn page_validation_late_measurement_wakes_own_writer_with_overlap() {
        let dir = temp_dir("validation-measurement-wake");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        let first = place(&state, extension.actions(), "growing", 0.0, 0.0);
        place(&state, extension.actions(), "below", 0.0, 240.0);
        call(&state, extension.actions(), "atlas_read", json!({})).unwrap();
        let waiting = tokio::spawn(wait_effect(&extension, 30));
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiting.is_finished());
        atlas::measure_node(&state.scene.lock(), &first, 320.0).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("card_overlap"));
    }

    #[test]
    fn activity_revision_tracks_real_atlas_edits_and_stays_stable_on_read() {
        let dir = temp_dir("activity-revision");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        let before = state.activity_state_revision().expect("revision");
        place(&state, extension.actions(), "revision proof", 100.0, 100.0);
        let after = state.activity_state_revision().expect("revision");
        assert_ne!(before, after);
        call(&state, extension.actions(), "atlas_read", json!({})).expect("read");
        assert_eq!(after, state.activity_state_revision().expect("revision"));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("same-page-atlas-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("same-page-atlas lives under the workspace examples directory")
            .to_path_buf()
    }

    fn run(effect: Effect, state: &Arc<AtlasState>) -> Result<Option<String>, String> {
        match effect {
            Effect::Mutate(apply) => apply(state.as_ref()),
            Effect::Query(apply) => apply(state.as_ref()).map(Some),
            Effect::Reject(error) => Err(error),
            _ => panic!("unexpected effect"),
        }
    }

    fn call(
        state: &Arc<AtlasState>,
        defs: &[ToolDef],
        name: &str,
        args: JsonValue,
    ) -> Result<Option<String>, String> {
        let def = defs.iter().find(|def| def.name == name).expect("action");
        run((def.apply)(&args), state)
    }

    fn materialized_human_segment(
        state: &Arc<AtlasState>,
        label: &str,
        parent_id: &str,
        x: f64,
    ) -> String {
        let proposal = atlas::SegmentProposal {
            label: label.to_string(),
            tags: vec!["test-part".to_string()],
            ocr: Vec::new(),
            prompt_box: [500.0, 180.0, 1040.0, 900.0],
            x,
            y: 160.0,
            w: if parent_id.is_empty() { 420.0 } else { 24.0 },
            animation: "none".to_string(),
            parent_id: parent_id.to_string(),
        };
        let mut runs = Vec::new();
        runs.extend_from_slice(&0u32.to_le_bytes());
        runs.extend_from_slice(&4u32.to_le_bytes());
        let mut result = atlas::SegmentMaterialization {
            generation: 1,
            model_id: atlas::SEGMENT_MODEL_ID.to_string(),
            encoder_sha256: atlas::SEGMENT_ENCODER_SHA256.to_string(),
            decoder_sha256: atlas::SEGMENT_DECODER_SHA256.to_string(),
            predicted_iou: 0.91,
            mask_encoding: atlas::SEGMENT_MASK_ENCODING.to_string(),
            mask_width: 2,
            mask_height: 2,
            mask_runs: base64::engine::general_purpose::STANDARD.encode(runs),
            mask_source_box: proposal.prompt_box,
            parts: Vec::new(),
            receipt: json!({
                "modelId": atlas::SEGMENT_MODEL_ID,
                "backendRequested": "webgpu",
                "executionLocation": "browser-worker",
                "sourceSha256": atlas::SEGMENT_SOURCE_SHA256,
                "encoderSha256": atlas::SEGMENT_ENCODER_SHA256,
                "decoderSha256": atlas::SEGMENT_DECODER_SHA256,
                "stableObjectId": "not-yet-bound",
                "generation": 1,
                "sourceSize": [atlas::SEGMENT_SOURCE_WIDTH, atlas::SEGMENT_SOURCE_HEIGHT],
                "maskSourceBox": proposal.prompt_box,
                "materializedMaskSize": [2, 2],
                "predictedIoU": 0.91,
                "predictedIoUContract": "raw-regression-output-not-probability",
                "predictedIoURawInUnitInterval": true,
                "occlusionApplied": false,
                "occlusionBox": null,
                "alternateProviderConfigured": false,
                "adapterBoundToRuntime": true,
                "fallbackUsed": false,
                "mockUsed": false
            }),
        };
        atlas::accept_human_segment(
            &mut state.scene.lock(),
            &proposal,
            &mut result,
            &Author::Human,
        )
        .expect("materialized human segment")
    }

    fn attention_body(x: f64, y: f64, w: f64, h: f64, dragging: JsonValue) -> JsonValue {
        json!({
            "viewport": { "x": x, "y": y, "w": w, "h": h },
            "dragging": dragging,
        })
    }

    async fn post_attention(state: &Arc<AtlasState>, body: JsonValue) -> RouteResponse {
        let extension = AtlasExtension::new(state.clone());
        let route = extension
            .routes()
            .into_iter()
            .find(|route| route.method == HttpMethod::Post && route.path == "/atlas/attention")
            .expect("attention route");
        (route.handler)(RouteRequest {
            query: std::collections::HashMap::new(),
            body: Some(body),
        })
        .await
    }

    /// A real attention host, wired to this state's own resolver.
    ///
    /// Deliberately the real `SemanticTargetService` rather than a stand-in:
    /// the thing under test IS the attention contract, and a contract proven
    /// against a fake is not proven.
    fn attention_host(state: &Arc<AtlasState>) -> Arc<SemanticTargetService> {
        let resolver = state.clone();
        let service = Arc::new(
            SemanticTargetService::with_resolver(
                ["atlas".to_string()],
                move |target| resolver.semantic_target(target),
                |_| {},
            )
            .expect("attention host"),
        );
        state
            .attention_host
            .set(service.clone())
            .map_err(|_| "attention host bound twice")
            .expect("bind the attention host once");
        service
    }

    fn human_seat() -> ag_ui_surface::Participant {
        ag_ui_surface::Participant::human("humanseat", "You")
    }

    fn select_set(service: &SemanticTargetService, ids: &[&str]) {
        service
            .attend_many(
                human_seat(),
                AttentionMode::Selection,
                ids.iter()
                    .map(|id| SemanticTargetRef::new("atlas", *id))
                    .collect(),
                None,
            )
            .expect("the human selects a set");
    }

    fn where_the_human_is(text: &str) -> &str {
        text.split("WHERE THE HUMAN IS")
            .nth(1)
            .unwrap_or_else(|| panic!("no human attention section in:\n{text}"))
            .split("CHANGED SINCE YOUR LAST READ")
            .next()
            .expect("section")
    }

    fn named_agent(label: &str) -> ag_ui_surface::Actor {
        ag_ui_surface::Actor {
            caller: ag_ui_surface::Caller::Agent,
            label: Some(label.to_string()),
            participant_id: Some(format!("test-{label}")),
            hue: None,
            responsible: None,
        }
    }

    fn wait_effect(extension: &AtlasExtension, seconds: u64) -> AsyncQueryEffect {
        let action = extension
            .actions()
            .iter()
            .find(|action| action.name == "await_atlas")
            .expect("await_atlas action");
        match (action.apply)(&json!({ "seconds": seconds })) {
            Effect::AsyncQuery(future) => future,
            _ => panic!("await_atlas should be an async query"),
        }
    }

    #[test]
    fn await_atlas_defaults_to_sixty_seconds_and_caps_at_six_hundred() {
        assert_eq!(wait_seconds(&json!({})).expect("default wait"), 60);
        assert_eq!(
            wait_seconds(&json!({ "seconds": 600 })).expect("maximum wait"),
            600
        );
        for invalid in [json!({ "seconds": 0 }), json!({ "seconds": 601 })] {
            let error = wait_seconds(&invalid).expect_err("out of range wait");
            assert!(error.contains("between 1 and 600"), "{error}");
        }
        assert!(wait_seconds(&json!({ "seconds": 1.5 })).is_err());
    }

    #[tokio::test]
    async fn await_atlas_wakes_for_a_second_callers_write_with_current_attention() {
        let dir = temp_dir("await-second-caller");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("First agent"));
        call(&state, extension.actions(), "atlas_read", json!({})).expect("initial read");

        let waiting = tokio::spawn(wait_effect(&extension, 30));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let response = post_attention(
            &state,
            attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "attention alone must not wake await_atlas"
        );

        extension.note_caller(&named_agent("Second agent"));
        place(
            &state,
            extension.actions(),
            "a second perspective",
            100.0,
            100.0,
        );

        let seen = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait should wake inside its 30 second budget")
            .expect("the wait task did not panic")
            .expect("the wait returns a read-back");
        assert!(seen.contains("NEW node"), "{seen}");
        assert!(seen.contains("a second perspective"), "{seen}");
        assert!(seen.contains("created_by=Second agent"), "{seen}");
        assert!(seen.contains("WHERE THE HUMAN IS"), "{seen}");
    }

    #[tokio::test]
    async fn await_atlas_wakes_when_the_human_marks_the_crdt() {
        let dir = temp_dir("await-human-mark");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("Waiting agent"));
        let target = place(
            &state,
            extension.actions(),
            "the open question",
            100.0,
            100.0,
        );
        call(&state, extension.actions(), "atlas_read", json!({})).expect("initial read");

        let waiting = tokio::spawn(wait_effect(&extension, 30));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        atlas::mark(
            &mut browser,
            &target,
            "?",
            "please unpack this",
            &Author::Human,
        )
        .expect("human mark");
        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("state vector"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts the human mark");

        let seen = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the mark should wake the waiter")
            .expect("the wait task did not panic")
            .expect("the wait returns a read-back");
        assert!(seen.contains("please unpack this"), "{seen}");
        assert!(seen.contains("NEW MARK"), "{seen}");
    }

    /// R1: a marquee over three cards reads back as ONE selection of three,
    /// naming all three, and `atlas_read` and `semantic_targets_read` agree.
    #[test]
    fn a_multi_select_reads_back_as_one_sentence_naming_every_member() {
        let dir = temp_dir("multi-select-read-back");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("Serving agent"));
        let service = attention_host(&state);

        let first = place(&state, extension.actions(), "ingest", 100.0, 100.0);
        let second = place(&state, extension.actions(), "queue", 400.0, 100.0);
        let third = place(&state, extension.actions(), "worker pool", 700.0, 100.0);
        let outside = place(&state, extension.actions(), "billing", 2000.0, 900.0);

        select_set(&service, &[&first, &second, &third]);

        let read = call(&state, extension.actions(), "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let section = where_the_human_is(&read);
        assert!(section.contains("Selected"), "{section}");
        assert!(section.contains("3 things"), "{section}");
        for label in ["ingest", "queue", "worker pool"] {
            assert!(section.contains(label), "{label} missing from:\n{section}");
        }
        assert!(
            !section.contains("billing"),
            "a card the marquee never touched must not be in the selection: {section}"
        );
        assert!(
            section.matches("Selected").count() == 1,
            "one gesture is one line, not one per member: {section}"
        );
        assert!(!outside.is_empty());

        // The two agent-readable surfaces must not disagree about the set.
        let attention = service.describe();
        assert!(attention.contains("over 3 in atlas"), "{attention}");
        for label in ["ingest", "queue", "worker pool"] {
            assert!(
                attention.contains(label),
                "{label} missing from:\n{attention}"
            );
        }
    }

    /// R2: shift-click drops one member and the count follows it down.
    #[test]
    fn dropping_one_member_reads_back_as_a_smaller_set() {
        let dir = temp_dir("multi-select-drop-one");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("Serving agent"));
        let service = attention_host(&state);

        let first = place(&state, extension.actions(), "ingest", 100.0, 100.0);
        let second = place(&state, extension.actions(), "queue", 400.0, 100.0);
        let third = place(&state, extension.actions(), "worker pool", 700.0, 100.0);

        select_set(&service, &[&first, &second, &third]);
        select_set(&service, &[&first, &third]);

        let read = call(&state, extension.actions(), "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let section = where_the_human_is(&read);
        assert!(section.contains("2 things"), "{section}");
        assert!(section.contains("ingest"), "{section}");
        assert!(section.contains("worker pool"), "{section}");
        assert!(
            !section.contains("queue"),
            "the dropped member must be gone from the read-back: {section}"
        );
    }

    /// A set survives one member being deleted; only an empty set clears.
    #[test]
    fn deleting_one_member_prunes_the_set_instead_of_blanking_it() {
        let dir = temp_dir("multi-select-delete-one");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("Serving agent"));
        let service = attention_host(&state);

        let first = place(&state, extension.actions(), "ingest", 100.0, 100.0);
        let second = place(&state, extension.actions(), "queue", 400.0, 100.0);
        let third = place(&state, extension.actions(), "worker pool", 700.0, 100.0);
        select_set(&service, &[&first, &second, &third]);

        call(
            &state,
            extension.actions(),
            "atlas_remove",
            json!({ "id": second }),
        )
        .expect("remove the middle member");

        let read = call(&state, extension.actions(), "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let section = where_the_human_is(&read);
        assert!(section.contains("2 things"), "{section}");
        assert!(
            !section.contains("queue"),
            "a deleted card must not stay in the selection: {section}"
        );

        // Every member gone means no selection at all, the sticky-attention
        // rule from the earlier round, now applied to a set.
        for id in [&first, &third] {
            call(
                &state,
                extension.actions(),
                "atlas_remove",
                json!({ "id": id }),
            )
            .expect("remove the rest");
        }
        let read = call(&state, extension.actions(), "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(
            !read.contains("Selected (self-reported"),
            "an empty set must report no selection at all:\n{read}"
        );
    }

    /// R3: a set drag moves every member and the human owns all of it.
    ///
    /// The client batches the whole set into one transaction, so this replays
    /// exactly that: three positions in one browser-side commit, merged as one
    /// frame. Every member must come back stamped `touched_by=human`.
    #[test]
    fn a_set_drag_moves_every_member_and_attributes_the_human() {
        let dir = temp_dir("set-drag-attribution");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("Authoring agent"));
        let defs = extension.actions();

        let first = place(&state, defs, "ingest", 100.0, 100.0);
        let second = place(&state, defs, "queue", 400.0, 100.0);
        let third = place(&state, defs, "worker pool", 700.0, 100.0);
        call(&state, defs, "atlas_read", json!({})).expect("initial read");
        for id in [&first, &second, &third] {
            let node = state.read().expect("read").node(id).cloned().expect("node");
            assert_eq!(
                node.touched_by, "Authoring agent",
                "the agent authored it, under its own byline"
            );
        }

        // One human gesture: every member's new position in one transaction.
        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        for (id, x) in [(&first, 140.0), (&second, 440.0), (&third, 740.0)] {
            atlas::place_node(
                &mut browser,
                &atlas::NodePatch {
                    id: Some(id.clone()),
                    x: Some(x),
                    y: Some(260.0),
                    ..Default::default()
                },
                &Author::Human,
            )
            .expect("the human drags the set");
        }
        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("sv"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts the set drag");

        let moved = state.read().expect("read");
        for (id, x) in [(&first, 140.0), (&second, 440.0), (&third, 740.0)] {
            let node = moved.node(id).cloned().expect("node");
            assert_eq!(node.x, x, "every member moved, not just the one grabbed");
            assert_eq!(node.y, 260.0);
            assert_eq!(
                node.touched_by, "human",
                "LAST-EDITED must attribute the human who dragged the set"
            );
        }

        let read_back = call(&state, defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(read_back.contains("LAST-EDITED-BY=human"), "{read_back}");
    }

    /// R1's wake half: one marquee wakes a parked agent ONCE, with the set.
    #[tokio::test]
    async fn the_unified_wait_wakes_once_for_a_whole_marquee() {
        let dir = temp_dir("unified-wait-marquee");
        let (state, _ws) = state(&dir);
        let atlas_extension = AtlasExtension::new(state.clone());
        let board = BoardState::open(
            state.transport.clone(),
            dir.join("board.json"),
            state.clone(),
        )
        .expect("board state");
        let board_extension = BoardExtension::new(board);
        let waiting_agent = named_agent("Serving agent");
        atlas_extension.note_caller(&waiting_agent);
        board_extension.note_caller(&waiting_agent);
        let service = attention_host(&state);

        let first = place(&state, atlas_extension.actions(), "ingest", 100.0, 100.0);
        let second = place(&state, atlas_extension.actions(), "queue", 400.0, 100.0);
        let third = place(
            &state,
            atlas_extension.actions(),
            "worker pool",
            700.0,
            100.0,
        );
        call(&state, atlas_extension.actions(), "atlas_read", json!({})).expect("initial read");

        let unified = board_extension
            .actions()
            .iter()
            .find(|action| action.name == "await_input")
            .expect("unified inbound wait")
            .clone();
        let Effect::AsyncQuery(waiting) = (unified.apply)(&json!({ "seconds": 30 })) else {
            panic!("await_input should be an async query");
        };
        let waiting = tokio::spawn(waiting);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "the unified wait must park while every lane is quiet"
        );

        select_set(&service, &[&first, &second, &third]);

        let seen = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("a marquee should wake the unified wait within five seconds")
            .expect("the wait task did not panic")
            .expect("the wait returns the Atlas read-back");
        let section = where_the_human_is(&seen);
        assert!(section.contains("3 things"), "{section}");
        for label in ["ingest", "queue", "worker pool"] {
            assert!(section.contains(label), "{label} missing from:\n{section}");
        }

        // Once, not three times. Parking again on the SAME gesture must sit
        // quiet: three separate wakes would be the page telling the agent
        // about three events the human never performed.
        let Effect::AsyncQuery(again) = (unified.apply)(&json!({ "seconds": 1 })) else {
            panic!("await_input should be an async query");
        };
        let quiet = tokio::time::timeout(Duration::from_secs(4), tokio::spawn(again))
            .await
            .expect("the second wait should time out cleanly")
            .expect("the wait task did not panic")
            .expect("a quiet timeout is not an error");
        assert!(
            quiet.contains("no Atlas, board, or chat input arrived"),
            "one gesture must wake the wait once: {quiet}"
        );
    }

    #[tokio::test]
    async fn unified_wait_wakes_for_a_human_mark_with_its_meaning() {
        let dir = temp_dir("unified-wait-human-mark");
        let (state, _ws) = state(&dir);
        let atlas_extension = AtlasExtension::new(state.clone());
        let board = BoardState::open(
            state.transport.clone(),
            dir.join("board.json"),
            state.clone(),
        )
        .expect("board state");
        let board_extension = BoardExtension::new(board);
        let waiting_agent = named_agent("Serving agent");
        atlas_extension.note_caller(&waiting_agent);
        board_extension.note_caller(&waiting_agent);

        let target = place(
            &state,
            atlas_extension.actions(),
            "shared execution boundary",
            100.0,
            100.0,
        );
        call(&state, atlas_extension.actions(), "atlas_read", json!({})).expect("initial read");

        let action = board_extension
            .actions()
            .iter()
            .find(|action| action.name == "await_input")
            .expect("unified inbound wait");
        let Effect::AsyncQuery(waiting) = (action.apply)(&json!({ "seconds": 30 })) else {
            panic!("await_input should be an async query");
        };
        let waiting = tokio::spawn(waiting);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "the unified wait must park while every lane is quiet"
        );

        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        atlas::mark(
            &mut browser,
            &target,
            "?",
            "which side owns retries?",
            &Author::Human,
        )
        .expect("human mark");
        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("state vector"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts the human mark");

        let seen = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the unified wait should wake within five seconds")
            .expect("the wait task did not panic")
            .expect("the wait returns the Atlas read-back");
        assert!(seen.contains("NEW MARK"), "{seen}");
        assert!(
            seen.contains("? on \"shared execution boundary\""),
            "{seen}"
        );
        assert!(seen.contains("which side owns retries?"), "{seen}");
    }

    #[tokio::test]
    async fn await_atlas_does_not_wake_for_the_callers_own_write() {
        let dir = temp_dir("await-own-write");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("Same agent"));
        call(&state, extension.actions(), "atlas_read", json!({})).expect("initial read");

        let waiting = tokio::spawn(wait_effect(&extension, 1));
        tokio::time::sleep(Duration::from_millis(100)).await;
        place(&state, extension.actions(), "my own addition", 100.0, 100.0);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !waiting.is_finished(),
            "the caller's own commit must not wake await_atlas"
        );

        let seen = tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .expect("the bounded wait should time out cleanly")
            .expect("the wait task did not panic")
            .expect("a quiet timeout is not an error");
        assert!(seen.contains("did not change"), "{seen}");
    }

    #[tokio::test]
    async fn await_atlas_timeout_returns_cleanly_with_no_change() {
        let dir = temp_dir("await-timeout");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state);
        let seen = wait_effect(&extension, 1)
            .await
            .expect("a quiet atlas is not an error");
        assert!(seen.contains("did not change"), "{seen}");
        assert!(seen.contains("await_atlas again"), "{seen}");
    }

    #[tokio::test]
    async fn attention_post_then_atlas_read_contains_where_the_human_is() {
        let dir = temp_dir("attention-read-back");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        place(&state, &defs, "runtime kernel", 100.0, 100.0);

        let response = post_attention(
            &state,
            attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);

        let read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let section = where_the_human_is(&read);
        assert!(section.contains("runtime kernel"), "{section}");
        assert!(section.contains("looking at"), "{section}");
    }

    #[tokio::test]
    async fn viewport_without_selection_reports_bounds_zoom_and_monotonic_freshness() {
        let dir = temp_dir("attention-viewport-truth");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        let mut first = attention_body(50.0, 60.0, 400.0, 300.0, JsonValue::Null);
        first["zoom"] = json!(1.5);
        let response = post_attention(&state, first).await;
        assert_eq!(response.status, 200, "{}", response.body);

        let first_read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let first_section = where_the_human_is(&first_read);
        assert!(!first_section.contains("Selected"), "{first_section}");
        assert!(first_section.contains("observation #1"), "{first_section}");
        assert!(first_section.contains("age "), "{first_section}");
        assert!(
            first_section.contains("world bounds (50.0,60.0) to (450.0,360.0)"),
            "{first_section}"
        );
        assert!(first_section.contains("zoom 1.50x"), "{first_section}");

        let mut second = attention_body(70.0, 80.0, 200.0, 100.0, JsonValue::Null);
        second["zoom"] = json!(2.0);
        let response = post_attention(&state, second).await;
        assert_eq!(response.status, 200, "{}", response.body);
        let second_read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let second_section = where_the_human_is(&second_read);
        assert!(
            second_section.contains("observation #2"),
            "{second_section}"
        );
        assert!(second_section.contains("zoom 2.00x"), "{second_section}");
    }

    #[tokio::test]
    async fn composer_attention_source_is_accepted_by_the_host() {
        let dir = temp_dir("attention-composer-source");
        let (state, _ws) = state(&dir);
        let mut body = attention_body(50.0, 60.0, 400.0, 300.0, JsonValue::Null);
        body["source"] = json!("composer");

        let response = post_attention(&state, body).await;
        assert_eq!(response.status, 200, "{}", response.body);
    }

    /// The layout unit is derived from a number that lives in CSS, and
    /// a constant agreeing with another language only by comment is one commit
    /// from disagreeing. Read the renderer and check.
    ///
    /// Red before green: with `DIAGRAM_LABEL_PX` at the old implicit 40 this
    /// fails against an unchanged renderer, which is the drift it exists to
    /// catch.
    #[test]
    fn the_layout_unit_matches_the_size_the_renderer_draws_labels_at() {
        let source = include_str!("../static/styles.css");
        let rule = source
            .split(".node-label {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("a node-label rule");
        let marker = "font-size:";
        let drawn = rule
            .split(marker)
            .nth(1)
            .and_then(|rest| rest.split("px").next())
            .map(str::trim)
            .expect("a node label font size")
            .parse::<f64>()
            .expect("the renderer's font size is a number");
        assert_eq!(
            drawn, DIAGRAM_LABEL_PX,
            "the renderer draws node labels at {drawn}px but the layout is sized for {DIAGRAM_LABEL_PX}px"
        );
    }

    #[test]
    fn container_extent_uses_the_renderer_frame_insets() {
        let source = include_str!("../static/extensions/atlas/index.js");
        let constant = |name: &str| -> f64 {
            let marker = format!("const {name} = ");
            source
                .lines()
                .find_map(|line| line.trim().strip_prefix(&marker))
                .and_then(|value| value.strip_suffix(';'))
                .unwrap_or_else(|| panic!("renderer has no {name}"))
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("renderer {name} is not numeric"))
        };
        assert_eq!(constant("FRAME_PAD"), atlas::CONTAINER_PAD);
        assert_eq!(constant("FRAME_TITLE_H"), atlas::CONTAINER_TITLE_HEIGHT);
    }

    /// The property that actually matters: a box is sized for the type it
    /// holds. Sizes are asserted as a ratio rather than in pixels so the test
    /// survives any future retuning of the layout that keeps it proportionate.
    #[test]
    fn a_diagram_box_is_sized_for_the_label_it_holds() {
        let spec = diagram::Spec::parse(&json!({
            "title": "Proportion",
            "direction": "right",
            "nodes": [
                { "id": "a", "label": "attention state" },
                { "id": "b", "label": "the record" },
            ],
            "edges": [{ "from": "a", "to": "b" }],
        }))
        .expect("spec");
        let layout = spec.layout();
        let unit = diagram_unit(&layout);
        let placed = layout.place(unit, 0.0, 0.0);
        assert!(
            (placed.text_size - DIAGRAM_LABEL_PX).abs() < 1e-9,
            "text lands at {} not {DIAGRAM_LABEL_PX}",
            placed.text_size
        );
        let node = &placed.nodes[0];
        let ratio = node.h / placed.text_size;
        assert!(
            (1.5..4.0).contains(&ratio),
            "a box {}px tall around {}px type is not a box sized for its label (ratio {ratio})",
            node.h,
            placed.text_size
        );
    }

    /// A page that predates the preference is still describing real behaviour,
    /// so its silence has to mean the default rather than an error.
    #[tokio::test]
    async fn attention_without_a_camera_field_reads_as_the_default_policy() {
        let dir = temp_dir("attention-camera-default");
        let (state, _ws) = state(&dir);
        let response = post_attention(
            &state,
            attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);

        let section = {
            let read = call(&state, &actions(&state), "atlas_read", json!({}))
                .expect("read")
                .expect("text");
            where_the_human_is(&read).to_string()
        };
        assert!(
            section.contains("lets a deliberate reveal move their view"),
            "{section}"
        );
    }

    /// The whole point of telling the agent: under `hold` a reveal does not
    /// move anything, so an agent that says "I moved you there" is lying, and
    /// the read-back has to give it what it needs to say something true.
    #[tokio::test]
    async fn a_held_camera_tells_the_agent_a_reveal_will_not_move_the_view() {
        let dir = temp_dir("attention-camera-hold");
        let (state, _ws) = state(&dir);
        let mut body = attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null);
        body["camera"] = json!("hold");
        let response = post_attention(&state, body).await;
        assert_eq!(response.status, 200, "{}", response.body);

        let section = {
            let read = call(&state, &actions(&state), "atlas_read", json!({}))
                .expect("read")
                .expect("text");
            where_the_human_is(&read).to_string()
        };
        assert!(section.contains("holds the view"), "{section}");
        assert!(section.contains("will not move it"), "{section}");
    }

    #[tokio::test]
    async fn an_unknown_camera_policy_is_refused_rather_than_guessed_at() {
        let dir = temp_dir("attention-camera-unknown");
        let (state, _ws) = state(&dir);
        let mut body = attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null);
        body["camera"] = json!("whenever-it-likes");
        let response = post_attention(&state, body).await;
        assert_eq!(response.status, 400, "{}", response.body);
        assert!(
            format!("{}", response.body).contains("follow, ask, or hold"),
            "{}",
            response.body
        );
    }

    #[tokio::test]
    async fn stale_viewport_is_omitted_from_atlas_read() {
        let dir = temp_dir("stale-attention");
        let (state, _ws) = state(&dir);
        let response = post_attention(
            &state,
            attention_body(10.0, 20.0, 300.0, 200.0, JsonValue::Null),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        state
            .attention
            .lock()
            .as_mut()
            .expect("recorded attention")
            .observed_at = Instant::now() - ATTENTION_STALE_AFTER - Duration::from_millis(1);

        let read = call(&state, &actions(&state), "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(!read.contains("WHERE THE HUMAN IS"), "{read}");
    }

    #[test]
    fn no_attention_read_omits_where_the_human_is() {
        let dir = temp_dir("no-attention");
        let (state, _ws) = state(&dir);
        let read = call(&state, &actions(&state), "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(!read.contains("WHERE THE HUMAN IS"), "{read}");
    }

    /// One target as the gesture the read-back actually receives.
    fn gesture(targets: Vec<SemanticTarget>) -> SemanticAttention {
        let mut targets = targets;
        let anchor = ag_ui_surface::ComposerAnchor::over(&targets.iter().collect::<Vec<_>>());
        let target = targets.remove(0);
        SemanticAttention {
            mode: AttentionMode::Selection,
            anchor,
            target,
            additional_targets: targets,
            message: None,
        }
    }

    #[test]
    fn selection_wording_marks_browser_identity_as_self_reported() {
        let selection = gesture(vec![SemanticTarget::new(
            SemanticTargetRef::new("atlas", "node-1"),
            "dispatch boundary",
            "a selected node",
        )]);
        let section =
            human_attention_section(&atlas::Atlas::default(), Some(&selection), None, None);
        assert!(section.contains("self-reported"), "{section}");
        assert!(
            section.contains("participantId is body-claimed"),
            "{section}"
        );
        assert!(section.contains("\"dispatch boundary\""), "{section}");
    }

    #[test]
    fn composer_source_is_read_back_as_pointing_not_selection() {
        let target = gesture(vec![SemanticTarget::new(
            SemanticTargetRef::new("atlas", "node-1"),
            "dispatch boundary",
            "a pointed-at node",
        )]);
        let attention = HumanAttention {
            viewport: atlas::ViewportRect::new(0.0, 0.0, 100.0, 100.0).expect("viewport"),
            dragging: None,
            zoom: Some(1.0),
            sequence: 1,
            source: AttentionSource::Composer,
            camera: CameraPolicy::Follow,
            observed_at: Instant::now(),
        };

        let section = human_attention_section(
            &atlas::Atlas::default(),
            Some(&target),
            Some(&attention),
            None,
        );
        assert!(section.contains("Pointing (composer"), "{section}");
        assert!(!section.contains("- Selected"), "{section}");
    }

    #[test]
    fn browser_debounces_camera_attention_and_reports_node_drag_lifecycle() {
        let source = include_str!("../static/extensions/atlas/index.js");
        assert!(source.contains("const ATTENTION_SETTLE_MS = 200;"));
        assert!(source.contains("const DRAG_ATTENTION_KEEPALIVE_MS = 250;"));
        assert!(source.contains("fetch(\"/atlas/attention\""));
        assert!(source.contains("credentials: \"same-origin\""));
        // The world rectangle the report carries is the core's answer, not a
        // second copy of the projection in JavaScript. `camera.rs` owns it and
        // `the_world_viewport_is_what_the_pane_shows` holds it to the numbers.
        assert!(source
            .contains("const seen = atlasCamera.world_viewport(viewport.width, viewport.height);"));
        assert!(
            !source.contains("box.width / camera.scale"),
            "the page is projecting its own viewport again"
        );
        assert!(source.contains("startDragAttention(id);"));
        assert!(source.contains("if (endedNodeDrag) {"));
        assert!(source.contains("endDragAttention();"));
        assert!(source.contains("view.focus({ preventScroll: true });"));
    }

    #[test]
    fn an_offline_replica_cannot_accept_a_human_write_that_will_be_discarded() {
        let source = include_str!("../static/extensions/atlas/index.js");
        assert!(source.contains("view.inert = true;"));
        assert!(source.contains(
            "if (!socket || socket.readyState !== WebSocket.OPEN) {\n      report(\"Replica offline. No change was saved. Reconnecting...\");"
        ));
        assert!(source.contains("setSync(\"replica live\", true);\n      view.inert = false;"));
        assert!(source.contains(
            "setSync(\"Replica offline. Reconnecting...\", false);\n      view.inert = true;"
        ));
    }

    /// F9's rule, re-pinned after selection became a set.
    ///
    /// The guarantee is unchanged: zero movement writes nothing. The write
    /// call it guards changed from `place` to `placeSet`, because a set drag
    /// has to travel as one transaction, so the literal moved with it. The
    /// assertion below is deliberately stronger than the one it replaces: it
    /// checks that the node branch of `endDrag` has NO write call outside the
    /// `drag.moved` guard, rather than checking one blessed line exists.
    #[test]
    fn a_zero_movement_card_click_selects_without_writing() {
        let source = include_str!("../static/extensions/atlas/index.js");
        assert!(source.contains("const NODE_DRAG_THRESHOLD_PX = 4;"));
        assert!(source.contains("startClientX: event.clientX"));
        assert!(source.contains(
            "if (!drag.moved && nodeDragDistance(drag, event) < NODE_DRAG_THRESHOLD_PX) return;"
        ));
        assert!(source.contains("if (drag.moved) {"));

        let end_drag = source
            .split("function endDrag(event) {")
            .nth(1)
            .expect("endDrag body")
            .split("view.addEventListener(\"pointerup\", endDrag);")
            .next()
            .expect("end of endDrag");
        let node_branch = end_drag
            .split("drag.element.classList.remove(\"dragging\");")
            .nth(1)
            .expect("the node branch of endDrag");
        // The guard used to be one line, so checking that a write line began
        // with it was enough. A committed drag now also captures the moved
        // geometry into any agent-ink variable bound to it, so the guard is a
        // block. Track the block instead of the line: every write AND every
        // capture in the node branch has to sit inside it, which is the same
        // guarantee stated against a shape the code can now take.
        let mut depth_in_guard: Option<i32> = None;
        for line in node_branch.lines() {
            let line = line.trim();
            let writes = line.contains("place(")
                || line.contains("placeSet(")
                || line.contains("captureAgentInk(");
            if let Some(depth) = depth_in_guard.as_mut() {
                *depth += line.matches('{').count() as i32;
                *depth -= line.matches('}').count() as i32;
                if *depth <= 0 {
                    depth_in_guard = None;
                }
            } else if line.starts_with("if (drag.moved)") {
                // A single-line guard opens and closes on the same line, and
                // whatever it guards is on that line too, so it needs no block.
                if line.contains('{') {
                    depth_in_guard = Some(1);
                }
                continue;
            } else {
                assert!(
                    !writes,
                    "every write and capture in the node branch of endDrag must be behind \
                     the movement guard, found: {line}"
                );
            }
        }
    }

    /// A marquee is one gesture over a set, and it obeys the same F9 rule.
    #[test]
    fn a_marquee_selects_what_it_touches_and_a_click_on_nothing_writes_nothing() {
        let source = include_str!("../static/extensions/atlas/index.js");
        // Excalidraw parity: drag on empty canvas draws the box, and panning
        // moved onto space-drag and the middle button.
        assert!(source.contains("startMarquee(event);"));
        assert!(source.contains("if (event.button === 1 || spaceHeld) return startPan(event);"));
        // One gesture, one publish. The pointerdown clear is local; `endDrag`
        // is the only place a background gesture reaches the host.
        assert!(source.contains("if (event.button === 1 || spaceHeld || active.tool === \"hand\") return startPan(event);"));
        // The composer anchor is a host call too, so an unpublished clear
        // must leave it alone or the gesture reaches the host twice.
        assert!(source.contains("else if (publish) conversation?.clearAnchor?.();"));
        // Intersection, not containment.
        assert!(source.contains("const touches = rect.right >= box.left"));
        // Nodes, drawn shapes and constraint glyphs are all selectable.
        assert!(source.contains("for (const [id, entry] of nodeElements) {"));
        assert!(source.contains("for (const group of world.querySelectorAll(\".shape\")) {"));
        assert!(source.contains("for (const [id, group] of constraintTargetElements) {"));
        // Below the threshold the box never opens, and release writes nothing.
        assert!(source.contains(
            "      if (!drag.moved && nodeDragDistance(drag, event) < NODE_DRAG_THRESHOLD_PX) return;"
        ));
        assert!(source
            .contains("selectSet(drag.moved ? [...drag.base, ...marqueeHits(drag)] : drag.base);"));
    }

    /// R2: shift toggles membership, on every kind of selectable object.
    #[test]
    fn shift_click_toggles_one_member_of_the_set() {
        let source = include_str!("../static/extensions/atlas/index.js");
        assert!(source.contains(
            "    selectSet(\n      selection.includes(id)\n        ? selection.filter((member) => member !== id)\n        : [...selection, id],"
        ));
        // A node, a drawn shape, and a constraint glyph each reach the toggle.
        assert!(source.contains("    if (event.shiftKey) {\n      toggleSelected(id);"));
        assert!(source.contains("toggleSelected(shapeElement.dataset.id);"));
        assert!(source.contains(
            "onSelect: (id, event) => (event?.shiftKey ? toggleSelected(id) : select(id)),"
        ));
    }

    /// R3: one hand movement is one intent, however many cards it carried.
    #[test]
    fn dragging_one_member_moves_the_set_in_a_single_write() {
        let source = include_str!("../static/extensions/atlas/index.js");
        // Every member's patch goes through one `write`, so one flush, one
        // frame on the wire, and one commit on the host.
        assert!(source.contains(
            "  const placeSet = (patches) => write(() => {\n    let last = null;\n    for (const patch of patches) last = doc.place_node(JSON.stringify(patch));"
        ));
        assert!(source.contains("function dragPatches(candidate) {"));
        assert!(source.contains("...(candidate.followers || []).map((follower) => ({"));
        // Grabbing a member keeps the set; grabbing anything else narrows it.
        assert!(source.contains("if (!isSelected(id)) select(id);"));
        assert!(source.contains("for (const follower of drag.followers || []) {"));
        // The throttled mid-drag write batches too, not just the release.
        assert!(source.contains("      drag.lastWrite = now;\n      placeSet(dragPatches(drag));"));
    }

    /// The panels a human reads degrade to the set instead of naming one card.
    #[test]
    fn the_selected_panel_and_composer_speak_for_the_whole_set() {
        let atlas_source = include_str!("../static/extensions/atlas/index.js");
        let board_source = include_str!("../static/extensions/board/index.js");
        assert!(atlas_source.contains("if (selection.length > 1) return renderSetInspector();"));
        assert!(
            atlas_source.contains("inspectorTitle.textContent = `${selection.length} selected`;")
        );
        assert!(atlas_source.contains("conversation.setAnchor({ label: selectionSummary() });"));
        assert!(atlas_source.contains("`${names.length} selected: ${shown}`"));
        // One publish for the whole set, never one per member.
        assert!(atlas_source.contains(
            "const operation = live.length ? targets.selectMany(live) : targets.clear();"
        ));
        assert!(board_source.contains("`Pointing at ${atlasTarget.summary}`"));
    }

    /// A mark aimed at a member of a set pins to the set, and says so.
    #[test]
    fn a_mark_with_a_multi_selection_pins_to_every_member() {
        let source = include_str!("../static/extensions/atlas/index.js");
        assert!(source.contains(
            "const pinned = isSelected(targetId) && selection.length > 1 ? [...selection] : [targetId];"
        ));
        assert!(source.contains("? markEach(pinned, glyph, text)"));
        // The refusal case names what did not get written rather than folding
        // a partial write into a success.
        assert!(source.contains("`${glyph} reached ${saved.length} of ${ids.length}."));
        assert!(
            source.contains("`${glyph} ${GLYPH_MEANING[glyph]} attached to all ${ids.length}.`")
        );
    }

    #[test]
    fn a_card_click_takes_keyboard_scope_only_after_pointerup() {
        let source = include_str!("../static/extensions/atlas/index.js");
        let start_drag = source
            .split("function startNodeDrag")
            .nth(1)
            .expect("node drag start")
            .split("function nodeDragDistance")
            .next()
            .expect("node drag body");
        assert!(
            !start_drag.contains("view.focus"),
            "pointerdown must not steal focus before the human completes the click"
        );
        assert!(source.contains(
            "if (endedNodeDrag) {\n      endDragAttention();\n      view.focus({ preventScroll: true });\n    }"
        ));
        let toolbar_guard = source
            .find(".draw-tools, .zoom-controls")
            .expect("toolbar pointer guard");
        let node_drag = source
            .find("return startNodeDrag(event, nodeElement);")
            .expect("node drag dispatch");
        assert!(
            toolbar_guard < node_drag,
            "toolbar controls must retain their own click path"
        );
        assert!(source.contains(
            "if (id && publish && !focusedTextEntry()) view.focus({ preventScroll: true });"
        ));
    }

    #[test]
    fn the_board_pointing_chip_clears_with_atlas_selection() {
        let atlas_source = include_str!("../static/extensions/atlas/index.js");
        let board_source = include_str!("../static/extensions/board/index.js");
        assert!(
            atlas_source.contains("if (selected && !documentHasTarget(selected)) select(null);")
        );
        assert!(atlas_source.contains("startMarquee(event);"));
        assert!(atlas_source.contains("else if (publish) conversation?.clearAnchor?.();"));
        assert!(board_source.contains("showAtlasTarget(event.detail);"));
        assert!(board_source.contains("composeContext.hidden = !atlasTarget;"));
    }

    #[test]
    fn advertised_mark_keys_have_a_scoped_editor_and_report_rejection() {
        let source = include_str!("../static/extensions/atlas/index.js");
        assert!(source.contains("function reportRejectedGesture"));
        assert!(source.contains("view.focus({ preventScroll: true })"));
        assert!(source.contains("openMarkEditor(selected, event.key)"));
        assert!(source.contains("No mark was attached"));
    }

    #[tokio::test]
    async fn a_rejected_human_mark_gesture_is_agent_readable_attention() {
        let dir = temp_dir("rejected-mark-attention");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let mut body = attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null);
        body["rejectedGesture"] = json!({
            "key": "!",
            "target": null,
            "reason": "no selected target"
        });

        let response = post_attention(&state, body).await;
        assert_eq!(response.status, 200, "{}", response.body);
        let read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(read.contains("Rejected gesture \"!\""), "{read}");
        assert!(read.contains("no selected target"), "{read}");
        assert!(read.contains("No mark was attached"), "{read}");
    }

    #[tokio::test]
    async fn ended_drag_disappears_from_the_next_atlas_read() {
        let dir = temp_dir("attention-drag-end");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let id = place(&state, &defs, "dispatch boundary", 100.0, 100.0);

        let response =
            post_attention(&state, attention_body(50.0, 50.0, 400.0, 300.0, json!(id))).await;
        assert_eq!(response.status, 200, "{}", response.body);
        let during = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(
            during.contains("currently dragging \"dispatch boundary\""),
            "{during}"
        );

        let response = post_attention(
            &state,
            attention_body(50.0, 50.0, 400.0, 300.0, JsonValue::Null),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let after = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(!after.contains("currently dragging"), "{after}");
    }

    #[tokio::test]
    async fn intersecting_viewport_reports_world_bounds_and_relational_context() {
        let dir = temp_dir("attention-no-coordinates");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        place(&state, &defs, "the visible claim", 1234.0, 5678.0);
        let response = post_attention(
            &state,
            attention_body(1201.0, 5603.0, 333.0, 277.0, JsonValue::Null),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);

        let read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let section = where_the_human_is(&read);
        assert!(section.contains("the visible claim"), "{section}");
        assert!(
            section.contains("world bounds (1201.0,5603.0) to (1534.0,5880.0)"),
            "{section}"
        );
    }

    #[test]
    fn cement_is_human_only_and_every_other_base_action_is_agent_only() {
        let dir = temp_dir("audience");
        let (state, _ws) = state(&dir);
        assert!(assert_base_action_audiences(&actions(&state)));
    }

    #[test]
    fn multimodal_segment_action_mints_only_receipt_bound_semantics() {
        let dir = temp_dir("multimodal-segment-action");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let proposal = atlas::SegmentProposal {
            label: "Mechanical owl".to_string(),
            tags: vec![
                "visual-reasoning".to_string(),
                "central-subject".to_string(),
            ],
            ocr: vec!["VISUAL INSTINCT".to_string()],
            prompt_box: [530.0, 205.0, 1005.0, 845.0],
            x: 120.0,
            y: 160.0,
            w: 420.0,
            animation: "none".to_string(),
            parent_id: String::new(),
        };
        let output_sha256 =
            atlas::segment_semantic_output_sha256(&proposal).expect("semantic digest");
        let payload = json!({
            "label": proposal.label,
            "tags": proposal.tags,
            "ocr": proposal.ocr,
            "prompt_box": proposal.prompt_box,
            "x": proposal.x,
            "y": proposal.y,
            "w": proposal.w,
            "semantic_receipt": {
                "schema": atlas::SEGMENT_SEMANTIC_RECEIPT_SCHEMA,
                "provider": "test-provider",
                "model": "test-vision-model",
                "request_id": "test-request-1",
                "execution_location": "external-multimodal-agent",
                "trust": "participant-claimed",
                "source_sha256": atlas::SEGMENT_SOURCE_SHA256,
                "source_size": [atlas::SEGMENT_SOURCE_WIDTH, atlas::SEGMENT_SOURCE_HEIGHT],
                "input_mime_type": "image/png",
                "output_sha256": output_sha256,
                "fallback_used": false,
                "mock_used": false
            }
        });

        let result = call(&state, &defs, "atlas_segment_agent_import", payload.clone())
            .expect("receipt-bound action")
            .expect("action response");
        assert!(
            result.contains("receipt-bound multimodal semantics"),
            "{result}"
        );
        let atlas = state.read().expect("read");
        assert_eq!(atlas.shapes.len(), 1);
        assert_eq!(atlas.shapes[0].segment_semantics_source, "multimodal-agent");

        let mut rejected = payload;
        rejected["semantic_receipt"]["fallback_used"] = json!(true);
        let error = call(&state, &defs, "atlas_segment_agent_import", rejected)
            .expect_err("fallback receipt");
        assert!(error.contains("mock or fallback"), "{error}");
        assert_eq!(state.read().expect("read after refusal").shapes.len(), 1);
    }

    #[test]
    fn wing_action_preserves_the_parent_segment_and_records_both_prompts() {
        let dir = temp_dir("segment-wing-action");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let id = {
            let mut scene = state.scene.lock();
            atlas::propose_segment(
                &mut scene,
                &atlas::SegmentProposal {
                    label: "Mechanical owl".to_string(),
                    tags: vec!["central-subject".to_string()],
                    ocr: vec!["VISUAL INSTINCT".to_string()],
                    prompt_box: [530.0, 205.0, 1005.0, 845.0],
                    x: 120.0,
                    y: 160.0,
                    w: 420.0,
                    animation: "none".to_string(),
                    parent_id: String::new(),
                },
                &Author::Agent,
            )
            .expect("segment")
        };
        let result = call(
            &state,
            &defs,
            "atlas_segment_wings",
            json!({
                "id": id,
                "left_box": [540.0, 430.0, 700.0, 770.0],
                "right_box": [840.0, 430.0, 1000.0, 770.0]
            }),
        )
        .expect("wing action")
        .expect("response");
        assert!(result.contains("generation 2"), "{result}");
        let atlas = state.read().expect("read");
        assert_eq!(atlas.shapes.len(), 1);
        let segment = atlas.shape(&id).expect("same segment");
        assert_eq!(segment.segment_generation, 2);
        assert_eq!(segment.segment_status, "proposed");
        assert_eq!(
            segment.segment_wing_prompts,
            json!({
                "left-wing": [540.0, 430.0, 700.0, 770.0],
                "right-wing": [840.0, 430.0, 1000.0, 770.0]
            })
        );
    }

    #[test]
    fn browser_wing_control_uses_real_masks_and_keeps_pointer_ownership() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        for required in [
            "doc.set_segment_flap(segmentId, value)",
            "event.isTrusted",
            ".segment-flap-panel, .segment-tree-panel, .segment-picker, .inline-input",
            "paintSegmentComposition(",
            "function preservesSegmentSource(shape)",
            "shape.segment_motion.child_compositing === \"overlay\"",
            "const childSegments = preserveOwnerImage ? []",
            "bodyCanvas.dataset.redundantSourceOverlay = String(redundantSourceOverlay)",
            "paintSegmentSource(bodyCanvas, body, image",
            "bodyCanvas.dataset.sourcePreserved = String(preserveOwnerImage)",
            "const segmentGroups = new Map()",
            "(parent || layer).append(group)",
            "wingsSubtracted",
            "hierarchyPartsSubtracted",
        ] {
            assert!(
                browser.contains(required),
                "missing browser wing contract {required:?}"
            );
        }
        let worker = include_str!("../static/extensions/atlas/segment-worker.js");
        for required in [
            "const body = await decodePrompt",
            "for (const [role, promptBox] of wingEntries)",
            "executionProviders: [\"webgpu\"]",
            "fallbackUsed: false",
            "mockUsed: false",
        ] {
            assert!(
                worker.contains(required),
                "missing Worker wing contract {required:?}"
            );
        }
        assert!(
            !worker.contains("executionProviders: [\"wasm\"]"),
            "the wing Worker must not configure a fallback provider"
        );
    }

    #[test]
    fn browser_product_flow_accepts_a_real_mask_through_the_human_replica() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        for required in [
            "createSegmentPicker",
            "doc.accept_segment(",
            "doc.set_segment_motion(active.owner.id, \"null\")",
            "doc.set_segment_parent(movingId, segment.id)",
            "Animate ${names}: ",
            "event.shiftKey ? toggleSelected(segment.id) : select(segment.id)",
            "activateSegmentMotions(group, shape)",
            "animation.id = `atlas-motion:",
            "iterations: track.loop ? Infinity : 1",
            "stagger_ms",
            "segment-tree-panel",
            "segment-tree-collapse",
            "appendBranch(child, depth + 1)",
            "finishInline(box.dataset.closeMode !== \"discard\")",
            "points.some((point) => !point.trusted)",
            "segmentPicker?.refresh(atlas)",
            "finish(parentId)",
            "const segmentSourceImages = new Map()",
            "segmentSourceImages.get(url)",
        ] {
            assert!(
                browser.contains(required),
                "missing browser product-flow contract {required:?}"
            );
        }

        let picker = include_str!("../static/extensions/atlas/segment-picker.js");
        for required in [
            "Click the object you want",
            "The mask appears after one click",
            "Click a point again to undo it",
            "Add mask",
            "Create object",
            "Add part",
            "Finish parts",
            "Add parts to",
            "Accepted child masks stay highlighted",
            "appendParentOptions(child, depth + 1)",
            "Add to mask",
            "Remove from mask",
            "nextNumberedPartLabel",
            "decodeMaskRuns",
            "seriesAccepted",
            "visibleAcceptedMasks",
            "lastAccepted",
            "trustedPointCount",
            "Infographic source",
            "selectSource(sourceSelect.value)",
        ] {
            assert!(
                picker.contains(required),
                "missing mask-picker interaction {required:?}"
            );
        }

        let worker = include_str!("../static/extensions/atlas/segment-worker.js");
        for required in [
            "prepare-picker",
            "resolve-picker",
            "decodePointPrompt",
            "segmentSourceByIdentity",
            "pickers: new Map()",
            "model.pickers.get(source.id)",
            "...picker.receipt",
            "executionProviders: [\"webgpu\"]",
            "fallbackUsed: false",
            "mockUsed: false",
        ] {
            assert!(
                worker.contains(required),
                "missing product Worker contract {required:?}"
            );
        }
        assert!(!worker.contains("executionProviders: [\"wasm\"]"));
    }

    #[test]
    fn browser_text_editing_and_object_tree_disclosure_are_explicit() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        for required in [
            "function editTextInPlace(target, value, ariaLabel, onCommit)",
            "target.setAttribute(\"contenteditable\", \"plaintext-only\")",
            "document.addEventListener(\"pointerdown\", (event) =>",
            "settleInlineEditor(true)",
            "if (commit && text) onCommit(text)",
            "const collapsedSegments = new Set",
            "disclosure.setAttribute(\"aria-expanded\", String(expanded))",
            "if (expanded) {",
            "localStorage.setItem(SEGMENT_TREE_STORAGE_KEY",
        ] {
            assert!(
                browser.contains(required),
                "missing browser editing or disclosure contract {required:?}"
            );
        }

        let styles = include_str!("../static/styles.css");
        for required in [
            ".inline-direct-editor",
            ".segment-tree-toggle",
            ".segment-tree-row",
        ] {
            assert!(
                styles.contains(required),
                "missing editing or disclosure style {required:?}"
            );
        }
    }

    #[test]
    fn segment_move_rejects_an_ordinary_shape_without_mutating_it() {
        let dir = temp_dir("segment-move-non-segment");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let id = {
            let mut scene = state.scene.lock();
            atlas::place_shape(
                &mut scene,
                &atlas::ShapePatch {
                    form: Some("rect".to_string()),
                    x: Some(10.0),
                    y: Some(20.0),
                    w: Some(120.0),
                    h: Some(80.0),
                    label: Some("ordinary shape".to_string()),
                    ..atlas::ShapePatch::default()
                },
                &Author::Agent,
            )
            .expect("place ordinary shape")
        };
        let before = state
            .read()
            .expect("read before rejection")
            .shape(&id)
            .cloned()
            .expect("ordinary shape before rejection");

        let error = call(
            &state,
            &defs,
            "atlas_segment_move",
            json!({ "id": id, "x": 700.0, "y": 900.0 }),
        )
        .expect_err("an ordinary shape must not be accepted as a segment");

        let after = state
            .read()
            .expect("read after rejection")
            .shape(&before.id)
            .cloned()
            .expect("ordinary shape after rejection");
        assert!(error.contains("no segment"), "{error}");
        assert_eq!((after.x, after.y), (before.x, before.y));
        assert_eq!(after.form, before.form);
        assert_eq!(after.touched_by, before.touched_by);
    }

    #[test]
    fn segment_motion_action_targets_exact_parts_and_refuses_cross_object_targets() {
        let dir = temp_dir("segment-motion-action");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let root = materialized_human_segment(&state, "Owl", "", 120.0);
        let foot = materialized_human_segment(&state, "Left foot", &root, 180.0);
        let left = materialized_human_segment(&state, "Left claw", &root, 210.0);
        let right = materialized_human_segment(&state, "Right claw", &root, 330.0);
        let unrelated = materialized_human_segment(&state, "Other object", "", 900.0);
        let nested = call(
            &state,
            &defs,
            "atlas_segment_reparent",
            json!({ "id": left.clone(), "parent_id": foot.clone() }),
        )
        .expect("reparent action")
        .expect("reparent response");
        assert!(nested.contains("world position"), "{nested}");
        assert_eq!(
            state
                .read()
                .expect("read nested action")
                .shape(&left)
                .expect("nested claw")
                .segment_parent_id,
            foot
        );
        let before = state
            .read()
            .expect("read positions")
            .shapes
            .iter()
            .map(|shape| (shape.id.clone(), shape.x, shape.y))
            .collect::<Vec<_>>();
        let motion = json!({
            "schema": atlas::SEGMENT_MOTION_SCHEMA,
            "label": "Alternating claw loading",
            "child_compositing": "overlay",
            "tracks": [{
                "label": "Claw taps",
                "target_ids": [left, right],
                "duration_ms": 900,
                "stagger_ms": 110,
                "loop": true,
                "keyframes": [
                    { "at": 0, "y": 0 },
                    { "at": 0.35, "y": 11, "rotate": 4 },
                    { "at": 1, "y": 0 }
                ]
            }]
        });
        let result = call(
            &state,
            &defs,
            "atlas_segment_motion",
            json!({ "owner_id": root, "motion": motion }),
        )
        .expect("motion action")
        .expect("motion response");
        assert!(result.contains("for 2 target(s)"), "{result}");
        assert!(
            result.contains("saved positions were unchanged"),
            "{result}"
        );
        assert!(result.contains("Inspect a real browser"), "{result}");
        let after = state.read().expect("read motion");
        assert_eq!(
            after
                .shapes
                .iter()
                .map(|shape| (shape.id.clone(), shape.x, shape.y))
                .collect::<Vec<_>>(),
            before
        );
        let accepted_motion = after.shape(&root).expect("root").segment_motion.clone();
        assert_eq!(accepted_motion["child_compositing"], "overlay");

        let mut invalid = motion;
        invalid["tracks"][0]["target_ids"] = json!([left, unrelated]);
        let error = call(
            &state,
            &defs,
            "atlas_segment_motion",
            json!({ "owner_id": root, "motion": invalid }),
        )
        .expect_err("cross-object target");
        assert!(error.contains("cannot target"), "{error}");
        assert_eq!(
            state
                .read()
                .expect("read after refusal")
                .shape(&root)
                .expect("root after refusal")
                .segment_motion,
            accepted_motion
        );
    }

    #[test]
    fn tool_descriptions_make_drawing_primary() {
        let dir = temp_dir("drawing-primary");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let description = |name: &str| -> &str {
            &defs
                .iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .description
        };

        let sketch = description("atlas_sketch");
        assert!(sketch.starts_with("Draw the picture."), "{sketch}");
        assert!(sketch.contains("primary way"), "{sketch}");
        assert!(sketch.contains("PROBLEMS"), "{sketch}");
        assert!(!sketch.contains("ANNOTATION"), "{sketch}");
        for semantic_tool in ["atlas_place", "atlas_draw", "atlas_diagram", "atlas_link"] {
            assert!(sketch.contains(semantic_tool), "{sketch}");
            let semantic = description(semantic_tool);
            assert!(
                semantic.contains("node register"),
                "{semantic_tool}: {semantic}"
            );
            assert!(
                semantic.contains("atlas_sketch"),
                "{semantic_tool}: {semantic}"
            );
            assert!(
                !semantic.contains("only to annotate"),
                "{semantic_tool}: {semantic}"
            );
        }
        for name in [
            "atlas_sketch",
            "atlas_place",
            "atlas_draw",
            "atlas_diagram",
            "atlas_link",
        ] {
            assert!(!description(name).contains('\u{2014}'), "{name}");
        }
    }

    #[test]
    fn atlas_read_names_absent_attention_as_live_only_context() {
        let dir = temp_dir("attention-description");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let read = defs
            .iter()
            .find(|def| def.name == "atlas_read")
            .expect("atlas_read action");
        assert!(read.description.contains("live-only, lease-bound context"));
        assert!(read.description.contains("not a history"));
        assert!(read.description.contains("currently absent"));
        assert!(read.description.contains("channel is not broken"));
    }

    #[test]
    fn the_document_revision_changes_when_the_atlas_changes() {
        let dir = temp_dir("document-revision");
        let (state, _ws) = state(&dir);
        let before = state.document_revision().expect("initial revision");
        call(
            &state,
            &actions(&state),
            "atlas_place",
            json!({ "label": "a real edit" }),
        )
        .expect("place");
        let after = state.document_revision().expect("changed revision");
        assert_ne!(before, after, "a receipt must bind the edited document");
    }

    #[test]
    fn a_sketch_reports_what_it_meant_not_that_it_was_written() {
        let dir = temp_dir("sketch");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_draw",
            json!({
                "nodes": [
                    { "ref": "a", "label": "parser", "x": 100, "y": 100, "w": 200 },
                    { "ref": "b", "label": "lexer", "x": 100, "y": 320, "w": 200 }
                ]
            }),
        )
        .expect("draw two nodes");
        let ids: Vec<String> = state
            .read()
            .expect("read")
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect();

        let reply = call(
            &state,
            &defs,
            "atlas_sketch",
            json!({
                "shapes": [
                    { "form": "rect", "x": 60, "y": 60, "w": 300, "h": 460, "ink": "violet", "label": "front end" },
                    { "form": "arrow", "from": ids[0], "to": ids[1] }
                ]
            }),
        )
        .expect("sketch")
        .expect("a summary");

        // The agent asked for a box and got told what the box turned out to
        // contain. Without that it would have to guess whether its coordinates
        // actually grouped anything.
        assert!(reply.contains("encloses"), "{reply}");
        assert!(reply.contains("parser"), "{reply}");
        assert!(reply.contains("lexer"), "{reply}");
        assert!(reply.contains("connects"), "{reply}");

        let described = state.describe().expect("read-back");
        assert!(described.contains("ENCLOSES"), "{described}");
        assert!(described.contains("labelled \"front end\""), "{described}");
    }

    #[test]
    fn a_sketch_that_names_a_missing_node_is_rejected_with_a_fixable_error() {
        let dir = temp_dir("sketch-reject");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let error = call(
            &state,
            &defs,
            "atlas_sketch",
            json!({ "shapes": [{ "form": "arrow", "from": "n_missing", "to": "n_gone" }] }),
        )
        .expect_err("a dangling arrow renders as nothing");
        assert!(error.contains("shapes[0]"), "{error}");
        assert!(error.contains("no bindable node or shape"), "{error}");
    }

    #[test]
    fn a_source_reference_that_does_not_hold_is_rejected() {
        let dir = temp_dir("verify");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        let error = call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "ghost", "path": "src/does-not-exist.rs" }),
        )
        .expect_err("must reject");
        assert!(error.contains("does not exist"), "{error}");

        let error = call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "stale", "path": "Cargo.toml", "lines": "9000-9100" }),
        )
        .expect_err("must reject");
        assert!(error.contains("stale"), "{error}");

        // The honest reference is accepted.
        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "manifest", "path": "Cargo.toml", "lines": "1-3", "tone": "doc" }),
        )
        .expect("real reference accepted");
    }

    #[test]
    fn one_draw_call_puts_a_whole_verified_subgraph_on_the_page() {
        let dir = temp_dir("draw");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        let reply = call(
            &state,
            &defs,
            "atlas_draw",
            json!({
                "nodes": [
                    {
                        "ref": "core",
                        "label": "same-page-atlas-core",
                        "path": "core/Cargo.toml",
                        "lines": "1-16",
                        "tone": "crate",
                        "note": "one mutation vocabulary, compiled twice"
                    },
                    {
                        "ref": "surface",
                        "label": "the host's actions",
                        "path": "src/atlas.rs",
                        "lines": "1-20",
                        "tone": "crate",
                        "note": "the agent writes through the same core crate"
                    },
                    { "ref": "atlas", "label": "same-page-atlas", "tone": "example" }
                ],
                "links": [
                    { "from": "surface", "to": "core", "label": "speaks" },
                    { "from": "atlas", "to": "surface", "label": "runs on" }
                ]
            }),
        )
        .expect("the batch is accepted")
        .expect("a reply");

        assert!(reply.contains("drew 3 node(s) and 2 link(s)"), "{reply}");
        // The caller's own names are echoed against the real ids, so the next
        // turn can address what it just drew without another read.
        assert!(reply.contains("core -> "), "{reply}");

        let atlas = state.read().expect("read back");
        assert_eq!(atlas.nodes.len(), 3);
        assert_eq!(atlas.edges.len(), 2);
    }

    #[test]
    fn one_diagram_call_lands_nodes_links_and_no_drawn_shapes() {
        let dir = temp_dir("diagram");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let reply = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "title": "Request path",
                "direction": "right",
                "nodes": [
                    { "id": "client", "label": "Client", "shape": "ellipse", "color": "blue", "size": "hero", "group": "Input" },
                    { "id": "router", "label": "Route request", "shape": "diamond", "color": "amber", "group": "Input" },
                    { "id": "policy", "label": "Check policy", "shape": "diamond", "color": "red", "group": "Runtime" },
                    { "id": "queue", "label": "Queue turn", "shape": "rect", "color": "violet", "group": "Runtime" },
                    { "id": "worker", "label": "Run provider", "shape": "rect", "color": "green", "group": "Runtime" },
                    { "id": "tools", "label": "Dispatch tools", "shape": "rect", "color": "blue", "group": "Runtime" },
                    { "id": "state", "label": "Apply state", "shape": "rect", "color": "green", "group": "Output" },
                    { "id": "events", "label": "Emit events", "shape": "rect", "color": "amber", "group": "Output" },
                    { "id": "reply", "label": "Reply", "shape": "ellipse", "color": "violet", "group": "Output" }
                ],
                "edges": [
                    { "from": "client", "to": "router", "label": "send" },
                    { "from": "router", "to": "policy", "label": "authorize" },
                    { "from": "policy", "to": "queue", "label": "admit" },
                    { "from": "queue", "to": "worker", "label": "dispatch" },
                    { "from": "worker", "to": "tools", "label": "call" },
                    { "from": "tools", "to": "worker", "label": "result" },
                    { "from": "worker", "to": "state", "label": "update" },
                    { "from": "worker", "to": "events", "label": "publish" },
                    { "from": "state", "to": "reply", "label": "read" },
                    { "from": "events", "to": "reply", "label": "stream" }
                ]
            }),
        )
        .expect("diagram")
        .unwrap_or_default();
        assert!(reply.contains("9 node(s)"), "{reply}");
        assert!(reply.contains("10 link(s)"), "{reply}");
        assert!(reply.contains("3 container(s)"), "{reply}");

        let atlas = state.read().expect("read diagram");
        // Nine authored nodes plus one container per group. Groups are real
        // containment now, so no group constraint is written at all: this
        // assertion was `9` nodes and `3` group constraints before P1b, and it
        // is inverted deliberately rather than relaxed.
        assert_eq!(atlas.nodes.len(), 12, "{}", atlas.describe());
        assert_eq!(atlas.edges.len(), 10, "{}", atlas.describe());
        assert!(atlas.shapes.is_empty(), "{}", atlas.describe());
        assert!(atlas.constraints.is_empty(), "{}", atlas.describe());
        for label in ["Input", "Runtime", "Output"] {
            let container = atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("no container {label}: {}", atlas.describe()));
            assert!(
                container.parent.is_empty(),
                "a diagram container must sit at the top level: {}",
                atlas.describe()
            );
            assert!(
                !atlas.children_of(&container.id).is_empty(),
                "{label} holds nothing: {}",
                atlas.describe()
            );
        }
        let client = atlas
            .nodes
            .iter()
            .find(|node| node.label == "Client")
            .expect("Client node");
        assert_eq!(
            atlas
                .node(&client.parent)
                .map(|container| container.label.as_str()),
            Some("Input"),
            "{}",
            atlas.describe()
        );
        let hero = atlas
            .nodes
            .iter()
            .find(|node| node.label == "Client")
            .expect("hero node");
        let ordinary = atlas
            .nodes
            .iter()
            .find(|node| node.label == "Run provider")
            .expect("ordinary node");
        assert!(
            hero.w > ordinary.w,
            "hero size did not create hierarchy: hero {} vs ordinary {}",
            hero.w,
            ordinary.w
        );

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back text");
        assert_eq!(read_back.matches(" created_by=").count(), 12, "{read_back}");
        assert_eq!(read_back.matches(" : ").count(), 10, "{read_back}");
        assert!(!read_back.contains("\nDRAWING"), "{read_back}");
        // The nesting, in the text the model actually gets: a container line
        // saying how many it holds, and its members indented under it.
        assert!(read_back.contains("\"Input\" tone="), "{read_back}");
        assert!(read_back.contains("CONTAINS 2"), "{read_back}");
        assert!(
            read_back.contains("  - [") && read_back.contains("\"Client\" tone="),
            "{read_back}"
        );
        assert!(
            read_back
                .contains("\"Client\" (inside \"Input\") -> \"Route request\" (inside \"Input\")"),
            "{read_back}"
        );
    }

    /// R10 mechanical half. This is the exact round-2 architecture payload
    /// that measured 6180x683px at a 10% Fit, then needed nine hand moves to
    /// become a 2066x1082px picture at the observed legible 29% zoom.
    ///
    /// The human-clock half stays in P3. This test owns the deterministic
    /// geometry only: one semantic authoring call, real projected node bounds,
    /// content-sized parents, no card overlaps, and the same camera-fit
    /// arithmetic the client uses.
    #[test]
    fn round_two_architecture_hierarchy_is_readable_in_one_fit() {
        let dir = temp_dir("r10-hierarchy-fit");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "direction": "right",
                "nodes": [
                    { "id": "doc", "label": "Shared CRDT document\n(one yrs doc, both peers write it)", "size": "hero", "shape": "rect", "color": "violet" },
                    { "id": "core", "label": "same-page-atlas-core\nthe one edit vocabulary", "size": "primary", "shape": "rect", "color": "blue", "group": "Rust host" },
                    { "id": "host", "label": "same-page-atlas\nhost process", "shape": "rect", "color": "blue", "group": "Rust host" },
                    { "id": "runtime", "label": "ag-ui-surface runtime\nserves /ws, /mcp, /surface/state", "shape": "rect", "color": "blue", "group": "Rust host" },
                    { "id": "repo", "label": "repository service\nverifies a cited file before accepting it", "shape": "rect", "color": "slate", "group": "Rust host" },
                    { "id": "wasm", "label": "Browser replica\nsame core compiled to wasm", "shape": "rect", "color": "green", "group": "Browser peer" },
                    { "id": "atlasjs", "label": "Atlas extension\ndraws the page, reads gestures", "shape": "rect", "color": "green", "group": "Browser peer" },
                    { "id": "human", "label": "Human\nin the browser", "shape": "ellipse", "color": "amber", "group": "Who edits" },
                    { "id": "agent", "label": "Terminal agent\nattached over /mcp", "shape": "ellipse", "color": "amber", "group": "Who edits" }
                ],
                "edges": [
                    { "from": "human", "to": "atlasjs", "label": "drags, marks, draws" },
                    { "from": "atlasjs", "to": "wasm", "label": "every edit goes through the core" },
                    { "from": "wasm", "to": "doc", "label": "y-sync over /ws" },
                    { "from": "agent", "to": "runtime", "label": "tool calls at /mcp" },
                    { "from": "runtime", "to": "doc", "label": "applies agent edits" },
                    { "from": "doc", "to": "atlasjs", "label": "renders back, no reload" },
                    { "from": "core", "to": "wasm", "label": "compiled twice" },
                    { "from": "core", "to": "host", "label": "one vocabulary" },
                    { "from": "host", "to": "runtime", "label": "composes the extensions" },
                    { "from": "runtime", "to": "repo", "label": "atlas_place checks the path" }
                ]
            }),
        )
        .expect("one atlas_diagram call");

        let atlas = state.read().expect("projected geometry");
        assert_eq!(atlas.nodes.len(), 12, "{}", atlas.describe());
        assert_eq!(atlas.edges.len(), 10, "{}", atlas.describe());
        assert!(atlas.shapes.is_empty(), "{}", atlas.describe());

        for container in atlas
            .nodes
            .iter()
            .filter(|node| atlas.is_container(&node.id))
        {
            let children = atlas.children_of(&container.id);
            let content = children.iter().fold(
                (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                |(left, top, right, bottom), child| {
                    let (cl, ct, cr, cb) = child.bounds();
                    (left.min(cl), top.min(ct), right.max(cr), bottom.max(cb))
                },
            );
            assert_eq!(
                container.bounds(),
                (
                    content.0 - atlas::CONTAINER_PAD,
                    content.1 - atlas::CONTAINER_PAD - atlas::CONTAINER_TITLE_HEIGHT,
                    content.2 + atlas::CONTAINER_PAD,
                    content.3 + atlas::CONTAINER_PAD
                ),
                "{} is not sized to the content it owns: {}",
                container.label,
                atlas.describe()
            );
        }

        let geometry = atlas::layout(&atlas);
        assert!(
            geometry.problems.is_empty(),
            "hierarchy packing still has a geometry defect: {:?}\n{}",
            geometry.problems,
            atlas.describe()
        );
        let (left, top, right, bottom) = geometry.extent.expect("diagram extent");
        let (width, height) = (right - left, bottom - top);

        // 1440x813 CSS pixels was the real round-2 browser. With the 360px
        // board, 380px side pane, two 7px seams, toolbar and 40px Fit margin,
        // the atlas has 606x640 usable pixels. The camera clamps at 10%.
        let fit_zoom = (DIAGRAM_FIT_VIEWPORT.0 / width)
            .min(DIAGRAM_FIT_VIEWPORT.1 / height)
            .clamp(0.1_f64, 1.0_f64);
        assert!(
            fit_zoom >= 0.29,
            "one Fit is still below the measured legible threshold: zoom={fit_zoom:.3}, extent={width:.0}x{height:.0}\n{}",
            atlas.describe()
        );
        let viewport_aspect = DIAGRAM_FIT_VIEWPORT.0 / DIAGRAM_FIT_VIEWPORT.1;
        let diagram_aspect = width / height;
        assert!(
            (diagram_aspect / viewport_aspect).max(viewport_aspect / diagram_aspect) <= 2.6,
            "top level did not pack toward the viewport: viewport={viewport_aspect:.3}, diagram={diagram_aspect:.3}"
        );
    }

    /// R8's hit-test half. `computeFrameBox` reserves a FRAME_TITLE_H band
    /// above a container's content and `frameEdgeHit` accepts that band as
    /// the frame's grab handle, with the comment above it saying so. The
    /// renderer then drew the label *outside* the box, lifted clear by
    /// `translateY(-100%)` into world-y strictly below `box.top`, so the band
    /// was empty and the label a human aims at was outside the only region
    /// the hit-test accepts. Measured cost: 22 seconds of a human probing
    /// five points to find that only the border works, and one wasted agent
    /// turn woken by the marquee the miss started.
    ///
    /// Red before green: this fails against the pre-fix stylesheet.
    #[test]
    fn the_frame_title_is_drawn_inside_the_band_the_hit_test_reserves() {
        let css = include_str!("../static/styles.css");
        let rule = css
            .split(".atlas-frame-title {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("an atlas-frame-title rule");
        assert!(
            !rule.contains("translateY(-100%)"),
            "the frame label is lifted out of the box the hit-test uses: {rule}"
        );
        let top = rule
            .split("top:")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .map(str::trim)
            .expect("a top offset");
        let top: f64 = top
            .strip_suffix("px")
            .unwrap_or(top)
            .parse()
            .expect("a numeric top offset");
        assert!(
            top >= 0.0,
            "the label starts above the frame root at top:{top}px, outside the grab band"
        );
        let height = rule
            .split("height:")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .map(str::trim)
            .and_then(|value| value.strip_suffix("px"))
            .and_then(|value| value.parse::<f64>().ok())
            .expect("an explicit label height, so the band cannot silently outgrow it");
        assert!(
            top + height <= atlas::CONTAINER_TITLE_HEIGHT,
            "the label is {}px tall from top:{top}px and the reserved band is {}px",
            height,
            atlas::CONTAINER_TITLE_HEIGHT
        );
    }

    /// A mark on a container was stored, delivered over MCP and answered in
    /// place while being invisible on the page: `renderNode` built the chips
    /// into the card root and then hid that root, `root.hidden =
    /// isContainer(node.id)`, and `renderFrames` never read the marks at all.
    /// So a human could not see their own flag on a frame, or the reply to it.
    #[test]
    fn a_mark_on_a_container_is_rendered_on_its_frame() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        assert!(
            browser.contains("function renderFrames(marksByTarget = marksIndex)"),
            "the frame renderer still cannot see the marks"
        );
        assert!(
            browser.contains("renderFrames(marksByTarget);"),
            "the render loop does not hand the frame renderer its marks"
        );
        assert!(
            browser.contains("frame.marks.replaceChildren("),
            "a frame is not painting its marks"
        );
        assert!(
            browser.contains("function markSummary(marks, targetId)"),
            "the card and the frame must build a chip the same way or they drift"
        );
        let css = include_str!("../static/styles.css");
        let rule = css
            .split(".atlas-frame-marks {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("an atlas-frame-marks rule");
        assert!(
            rule.contains("pointer-events: auto"),
            "the frame layer disables pointer events wholesale, so a chip that \
             does not re-enable them cannot select what it is attached to: {rule}"
        );
    }

    /// Three surfaces, one sentence. The panel said `4 selected`, the board
    /// chip said `Pointing at 4 selected: ... and 1 more`, and the composer,
    /// the surface actually attached to the question, said
    /// `pointing at Atlas pane renderer`. The whole ordered set was already on
    /// the wire; only the anchor was narrowed. This pins the host's wording
    /// and the renderer's to each other so they cannot drift back apart.
    #[test]
    fn the_composer_anchor_says_what_the_board_and_the_panel_say() {
        let targets: Vec<ag_ui_surface::SemanticTarget> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(|id| {
                ag_ui_surface::SemanticTarget::new(
                    ag_ui_surface::SemanticTargetRef::new("atlas", id),
                    id,
                    format!("the {id} card"),
                )
            })
            .collect();
        let anchor = ag_ui_surface::ComposerAnchor::over(&targets.iter().collect::<Vec<_>>());
        assert_eq!(anchor.label, "5 selected: a, b, c, and 2 more");
        assert_eq!(anchor.count, 5);
        assert_eq!(anchor.labels.len(), 5, "the payload keeps every name");

        let browser = include_str!("../static/extensions/atlas/index.js");
        assert!(
            browser.contains("`${names.length} selected: ${shown}, and ${names.length - 3} more`"),
            "the selection panel no longer words a set the way the anchor does"
        );
        assert!(
            browser.contains("`${names.length} selected: ${shown}`"),
            "the short form has to match too, or three and four names read as \
             two different products"
        );
    }

    /// R9's page half. The inspector had a fixed six-row schema older than the
    /// style register: `color`, `emphasis`, `size` and `kind` were never
    /// emitted, so the agent could publish a taxonomy, the page could render
    /// it, and the page could not tell the human what any of it meant. The
    /// claim was auditable only through the agent's own sentence in chat.
    #[test]
    fn the_inspector_shows_the_style_register_the_read_back_shows() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        assert!(
            browser.contains("doc.node_style_words(id)"),
            "the inspector still does not ask the model what this card chose"
        );
        assert!(
            browser.contains("for (const word of styleRows(node.id))"),
            "the style rows never reach the inspector's row list"
        );
        let replica = include_str!("../web/src/lib.rs");
        assert!(
            replica.contains("pub fn node_style_words(&self, id: &str)")
                && replica.contains("atlas::Node::style_words"),
            "the replica must answer with the read-back's own filter rather \
             than a second copy of it in JavaScript"
        );
    }

    /// R10's opening half. The opening refuses the whole-content
    /// fit below `NODE_TITLE_READABLE_SCALE` (0.8148) and frames the densest
    /// legible cluster instead. That rule was earned against a 10% smear and
    /// had no case for the arrival it was most likely to meet: one connected
    /// diagram, authored in one call, answering a request to see the whole
    /// system. Measured live: an 18-node hierarchy opened at 81%, two of its
    /// four frames off screen and a third cut off, while its true fit was 41%
    /// and every title in it was legible.
    #[test]
    fn one_connected_diagram_opens_whole_rather_than_as_its_densest_fragment() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        assert!(
            browser.contains("const WHOLE_DIAGRAM_LEGIBLE_SCALE = 0.29;"),
            "the opening has no floor for showing a connected diagram entire"
        );
        // The rule itself now lives in `camera.rs`, exercised directly by
        // `one_connected_diagram_opens_whole_above_the_cohesion_floor`. What
        // this test still owns is the wiring: the page has to TELL the core
        // the content is cohesive, and it has to hold the same floor number
        // the layout engine is held to.
        let pane = camera::Viewport {
            width: 1000.0,
            height: 800.0,
        };
        let mut boxes = Vec::new();
        for row in 0..6 {
            for column in 0..3 {
                boxes.push(camera::WorldBox {
                    x: f64::from(column) * 700.0,
                    y: f64::from(row) * 340.0,
                    w: 600.0,
                    h: 240.0,
                });
            }
        }
        let whole = camera::fit(&boxes, pane, 40.0, 0.0, 1.0).expect("a whole-content fit");
        let cohesive = camera::opening(&boxes, pane, 0.8148, 40.0, &boxes, Some(0.29), 1.0)
            .expect("an opening");
        assert_eq!(
            cohesive.camera, whole.camera,
            "the whole-content fit never wins for a cohesive arrival"
        );
        assert!(
            browser.contains("function contentIsOneConnectedDiagram()"),
            "nothing decides whether the board holds one connected picture"
        );
        assert!(
            browser.contains("contentIsOneConnectedDiagram() ? WHOLE_DIAGRAM_LEGIBLE_SCALE : null"),
            "the opening fit is not told whether the content is cohesive"
        );
        // The floor is the number the layout engine is already held to, so a
        // payload this test suite calls readable is a payload the camera shows.
        let hierarchy_floor = 0.29_f64;
        assert!(
            browser.contains(&format!("{hierarchy_floor}")),
            "the camera floor and the layout floor must be the same measurement"
        );
    }

    #[test]
    fn an_off_camera_arrival_opens_a_named_one_click_frame_control() {
        let browser = include_str!("../static/extensions/atlas/index.js");
        assert!(
            browser.contains("function mostlyOutsideViewport(target, viewport)"),
            "the arrival lane has no viewport test"
        );
        assert!(
            browser.contains("const offscreenArrivalIds = changeLines"),
            "changed objects are not checked against the current viewport"
        );
        // Was `if (offscreenArrivalIds.length) changesOpen = true;`, and that
        // unconditional form is now the defect rather than the fix. The
        // off-camera rule and the opening fit are causally linked: the fit is
        // what leaves content off camera, so the panel opened itself exactly
        // when the view was worst, at 320 CSS pixels over a 683 pixel pane.
        // It still opens itself; it declines to do so when it would cover the
        // picture it is announcing.
        assert!(
            browser.contains(
                "if (offscreenArrivalIds.length && panelFitsBesideTheCanvas(viewport)) changesOpen = true;"
            ),
            "an off-camera arrival stays collapsed"
        );
        assert!(
            browser.contains("function panelFitsBesideTheCanvas(viewport)"),
            "nothing decides whether the announcement would cover the canvas"
        );
        assert!(
            browser.contains("const PANEL_SHARE = 1 / 3;"),
            "the share of the pane an announcement may take is not stated"
        );
        assert!(
            browser.contains("revealTarget(id, { initiator: \"human\" });"),
            "the named arrival control does not frame its target"
        );
        assert!(
            browser.contains(concat!(
                ".draw-tools, .zoom-controls, .minimap, ",
                ".gesture-card, .atlas-changes, .source-popover"
            )),
            "the canvas captures the arrival control's pointer before click"
        );
    }

    /// R5. Two groups of three become two containers holding three each, the
    /// read-back shows the nesting, and nothing lands in the drawn register.
    #[test]
    fn two_diagram_groups_become_two_containers_of_three() {
        let dir = temp_dir("diagram-containers");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let reply = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "nodes": [
                    { "id": "ingest", "label": "Ingest", "group": "Front" },
                    { "id": "route", "label": "Route", "group": "Front" },
                    { "id": "admit", "label": "Admit", "group": "Front" },
                    { "id": "plan", "label": "Plan", "group": "Back" },
                    { "id": "run", "label": "Run", "group": "Back" },
                    { "id": "emit", "label": "Emit", "group": "Back" }
                ],
                "edges": [
                    { "from": "ingest", "to": "route" },
                    { "from": "route", "to": "admit" },
                    { "from": "admit", "to": "plan" },
                    { "from": "plan", "to": "run" },
                    { "from": "run", "to": "emit" }
                ]
            }),
        )
        .expect("diagram")
        .unwrap_or_default();
        assert!(reply.contains("2 container(s)"), "{reply}");

        let atlas = state.read().expect("read");
        assert_eq!(atlas.nodes.len(), 8, "{}", atlas.describe());
        assert!(atlas.shapes.is_empty(), "0 shapes: {}", atlas.describe());
        assert!(
            atlas.constraints.is_empty(),
            "a container is not a constraint: {}",
            atlas.describe()
        );
        for label in ["Front", "Back"] {
            let container = atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("no container {label}: {}", atlas.describe()));
            let children = atlas.children_of(&container.id);
            assert_eq!(
                children.len(),
                3,
                "{label} holds {} not 3: {}",
                children.len(),
                atlas.describe()
            );
            for child in children {
                assert_eq!(atlas.depth(&child.id), 1, "{}", atlas.describe());
            }
        }

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back text");
        assert!(read_back.contains("0 shapes"), "{read_back}");
        assert!(
            read_back.matches("CONTAINS 3").count() == 2,
            "the read-back does not name both containers: {read_back}"
        );
        for label in ["Ingest", "Route", "Admit", "Plan", "Run", "Emit"] {
            let line = read_back
                .lines()
                .find(|line| line.contains(&format!("\"{label}\" tone=")))
                .unwrap_or_else(|| panic!("no node line for {label}: {read_back}"));
            assert!(
                line.starts_with("  - ["),
                "{label} is not nested under its container: {read_back}"
            );
        }
        assert!(
            read_back.contains("\"Ingest\" (inside \"Front\") -> \"Route\" (inside \"Front\")"),
            "{read_back}"
        );
        assert!(
            read_back.contains("\"Admit\" (inside \"Front\") -> \"Plan\" (inside \"Back\")"),
            "a relation across two levels lost its containers: {read_back}"
        );
    }

    #[test]
    fn a_three_level_hierarchy_lays_out_with_no_container_overlap_and_every_child_inside_its_parent(
    ) {
        let dir = temp_dir("typed-hierarchy-layout");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "kind": "hierarchy",
                "containers": [
                    { "id": "host", "label": "Host" },
                    { "id": "ink", "label": "Agent ink", "parent": "host" },
                    { "id": "solver", "label": "Solver", "parent": "ink" },
                    { "id": "browser", "label": "Browser" }
                ],
                "nodes": [
                    { "id": "dispatch", "label": "Dispatch", "group": "host" },
                    { "id": "variable", "label": "Variable", "group": "ink" },
                    { "id": "solve", "label": "Solve", "group": "solver" },
                    { "id": "render", "label": "Render", "group": "browser" }
                ],
                "edges": [
                    { "from": "dispatch", "to": "variable" },
                    { "from": "variable", "to": "solve" },
                    { "from": "solve", "to": "render" }
                ]
            }),
        )
        .expect("nested hierarchy");

        let atlas = state.read().expect("read hierarchy");
        let by_label = |label: &str| {
            atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("no {label}: {}", atlas.describe()))
        };
        let inside = |child: &atlas::Node, parent: &atlas::Node| {
            let child = child.bounds();
            let parent = parent.bounds();
            child.0 >= parent.0 && child.1 >= parent.1 && child.2 <= parent.2 && child.3 <= parent.3
        };
        assert!(inside(by_label("Agent ink"), by_label("Host")));
        assert!(inside(by_label("Solver"), by_label("Agent ink")));
        assert!(inside(by_label("Dispatch"), by_label("Host")));
        assert!(inside(by_label("Variable"), by_label("Agent ink")));
        assert!(inside(by_label("Solve"), by_label("Solver")));
        assert!(inside(by_label("Render"), by_label("Browser")));
        let host = by_label("Host").bounds();
        let browser = by_label("Browser").bounds();
        assert!(
            host.2 <= browser.0
                || browser.2 <= host.0
                || host.3 <= browser.1
                || browser.3 <= host.1,
            "top-level containers overlap: {}",
            atlas.describe()
        );
        assert_eq!(atlas.depth(&by_label("Solve").id), 3);

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        assert!(
            read_back.contains("HIERARCHY\n- Host\n  - Agent ink\n    - Solver\n      - Solve"),
            "{read_back}"
        );
    }

    #[test]
    fn atlas_read_on_the_fixture_contains_the_grammar_sentence_for_at_least_one_transition() {
        let payload: JsonValue = serde_json::from_str(include_str!(
            "../fixtures/typed-diagrams/explanation-cursor.json"
        ))
        .expect("fixture JSON");
        let dir = temp_dir("typed-state-machine");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        let mut stale = payload.clone();
        stale["nodes"][0]["lines"] = JsonValue::String("999999".to_string());
        let error = call(&state, &defs, "atlas_diagram", stale)
            .expect_err("a stale range in the large source must be refused");
        assert!(error.contains("reference is stale"), "{error}");
        assert!(
            state.read().expect("read after refusal").nodes.is_empty(),
            "large-source verification left a partial diagram"
        );
        call(&state, &defs, "atlas_diagram", payload).expect("fixture diagram");

        let atlas = state.read().expect("read machine");
        let machine = atlas
            .nodes
            .iter()
            .find(|node| node.diagram_kind == "state_machine")
            .expect("state machine container");
        assert_eq!(machine.label, "explanation cursor");
        assert_eq!(atlas.children_of(&machine.id).len(), 4);
        assert_eq!(atlas.edges.len(), 10);
        let problems = atlas::layout(&atlas).problems;
        assert!(problems.is_empty(), "fixture PROBLEMS: {problems:?}");

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        assert!(
            read_back.contains(
                "STATE MACHINE \"explanation cursor\" (4 states, 10 transitions, cyclic)"
            ),
            "{read_back}"
        );
        assert!(
            read_back.contains(
                "- from \"active\" on advance when the next beat is not terminal -> \"active\""
            ),
            "{read_back}"
        );
        assert!(!read_back.contains("PROBLEMS"), "{read_back}");
    }

    /// Push one human write from a browser replica into the host.
    fn human_write(state: &Arc<AtlasState>, write: impl FnOnce(&mut Scene) -> String) -> String {
        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        let id = write(&mut browser);
        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("state vector"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts human write");
        id
    }

    #[tokio::test]
    #[ignore = "requires the real G8 CLI; run explicitly for cement acceptance"]
    async fn reviewed_cement_activates_the_real_g8_gate_and_records_the_adr() {
        let dir = temp_dir("reviewed-cement-action");
        let workspace = dir.join("project");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("state.rs"),
            "pub enum State { Ready, Done }\n",
        )
        .unwrap();
        let (state, _ws) = state_rooted(&dir.join("state"), &workspace, false);
        let defs = actions(&state);
        let id = place(&state, &defs, "Persistence", 0.0, 0.0);
        let ids = vec![id.clone()];
        let draft_id = human_write(&state, |scene| {
            atlas::context::set(
                scene,
                &atlas::context::ContextInput {
                    question: "Which lifecycle are we agreeing?".into(),
                    altitude: atlas::context::Altitude::Component,
                    subjects: vec![id.clone()],
                    assumptions: String::new(),
                    replaces: vec![],
                },
                &Author::Human,
            )
            .unwrap();
            let claim = atlas::claims::claim_at(
                scene,
                &id,
                "The lifecycle has Ready and Done states",
                "inferred",
                "",
                "",
                "",
                &Author::Agent,
            )
            .unwrap();
            atlas::claims::claim_verdict(scene, &claim, "accepted", &Author::Human).unwrap();
            atlas::place_node(
                scene,
                &atlas::NodePatch {
                    id: Some(id.clone()),
                    status: Some("agreed".into()),
                    ..Default::default()
                },
                &Author::Human,
            )
            .unwrap();
            let input: atlas::decision::DraftInput = serde_json::from_value(json!({
                "ids":ids,"title":"Persistence lifecycle","decision":"Use Ready and Done", "rationale":"Keep completion explicit", "alternatives":[{"option":"One boolean","reason_not_chosen":"Unclear transition semantics"}],"tradeoffs":"More explicit state", "consequences":"Review before adding another state", "checks":[{"id":"STATE-01","requirement":"Keep the two agreed variants","check":{"backend":"rust_enum_shape","args":{"file":"state.rs","enum_name":"State","expected_variants":["Ready","Done"],"expected_serde_rename_all":null}},"evidence_files":["state.rs"]}],"not_enforced":["Persistence behavior requires integration tests"]
            })).unwrap();
            atlas::decision::save(scene, &input, &Author::Agent).unwrap()
        });
        let preview: JsonValue =
            serde_json::from_str(&propose_cement_decision(&state, &ids, &draft_id, true).unwrap())
                .unwrap();
        let target = dir.join("bundle");
        let args = json!({"directory":target,"ids":ids,"draft_id":draft_id,"review_token":preview["review_token"]});
        let (_, _, allowed) = dispatch(
            &state,
            &defs,
            ag_ui_surface::Caller::Agent,
            "atlas_cement_decision",
            args.clone(),
        )
        .await;
        assert!(!allowed);
        assert!(!target.exists());
        let (result, _, allowed) = dispatch(
            &state,
            &defs,
            ag_ui_surface::Caller::Human,
            "atlas_cement_decision",
            args,
        )
        .await;
        assert!(allowed, "{result}");
        assert!(result.contains("active and passing"), "{result}");
        assert!(workspace.join("g8.lock").is_file());
        assert!(workspace
            .join("docs/decisions/0001-persistence.md")
            .is_file());
        assert_eq!(state.read().unwrap().challenge(&id).standing, "cemented");
        assert!(state.read().unwrap().describe().contains("CEMENTED as"));
        std::fs::write(
            workspace.join("state.rs"),
            "pub enum State { Ready, Lost }\n",
        )
        .unwrap();
        let gate = std::process::Command::new("g8")
            .args(["check", "--enforce", "--json"])
            .current_dir(&workspace)
            .output()
            .unwrap();
        assert_eq!(
            gate.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&gate.stdout)
        );
    }

    /// The challenge fixture: a three node hierarchy with three claims on
    /// Persistence in the three states the contract shows, one `?` mark on
    /// the rejected claim. The CLAIMS section is pinned by content.
    #[test]
    fn atlas_read_on_the_challenge_fixture_prints_the_claims_section_exactly() {
        let fixture: JsonValue =
            serde_json::from_str(include_str!("../fixtures/challenge/persistence.json"))
                .expect("fixture JSON");
        let dir = temp_dir("challenge-fixture");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        call(&state, &defs, "atlas_diagram", fixture["diagram"].clone()).expect("fixture diagram");
        let atlas = state.read().expect("read");
        let node_id = |label: &str| {
            atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .map(|node| node.id.clone())
                .expect(label)
        };
        let persistence = node_id("Persistence");

        let mut claim_ids = Vec::new();
        for claim in fixture["claims"].as_array().expect("claims") {
            if claim["about"] != "persistence" {
                continue;
            }
            let mut args = json!({
                "about": persistence,
                "text": claim["text"],
                "basis": claim["basis"],
            });
            if let Some(path) = claim.get("path") {
                args["path"] = path.clone();
                args["lines"] = claim["lines"].clone();
            }
            let reply = call(&state, &defs, "atlas_claim", args)
                .expect("atlas_claim")
                .expect("text");
            let id = reply
                .strip_prefix("claim ")
                .and_then(|rest| rest.split(' ').next())
                .expect("claim id in reply")
                .to_string();
            let verdict = claim["verdict"].as_str().expect("verdict");
            if verdict != "open" {
                human_write(&state, |scene| {
                    atlas::claim_verdict(scene, &id, verdict, &Author::Human).expect("verdict");
                    id.clone()
                });
            }
            if let Some(mark) = claim.get("mark") {
                human_write(&state, |scene| {
                    atlas::mark(
                        scene,
                        &id,
                        mark["glyph"].as_str().unwrap(),
                        mark["text"].as_str().unwrap(),
                        &Author::Human,
                    )
                    .expect("mark on claim")
                });
            }
            claim_ids.push(id);
        }
        let mark_id = state.read().expect("read").marks[0].id.clone();

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        let expected = format!(
            "\nCLAIMS (what the agent says it understands, and what the human said back)\n\
             - \"Persistence\" DISAGREED: 3 claims, 1 open, 1 rejected, 1 accepted\n\
             \x20   [{}] verified crates/ag-ui-surface/src/activity.rs:28 \"The journal keeps at most 800 events in memory\" ACCEPTED by human\n\
             \x20   [{}] inferred \"Eviction is by age, oldest first\" REJECTED by human\n\
             \x20       ? [{mark_id}] \"where is the age compared?\" UNANSWERED\n\
             \x20   [{}] assumed \"Nothing reads the journal but the trace panel\" open\n",
            claim_ids[0], claim_ids[1], claim_ids[2]
        );
        assert!(
            read_back.contains(&expected),
            "expected:\n{expected}\ngot:\n{read_back}"
        );
        assert!(!read_back.contains("no longer resolves"), "{read_back}");

        // The mark on a claim is answered in place like any other mark.
        call(
            &state,
            &defs,
            "atlas_answer",
            json!({ "mark_id": mark_id, "answer": "nowhere; it is a ring, I withdraw the claim" }),
        )
        .expect("answer claim mark");
        call(
            &state,
            &defs,
            "atlas_withdraw_claim",
            json!({ "id": claim_ids[1] }),
        )
        .expect("withdraw");
        human_write(&state, |scene| {
            atlas::claim_verdict(scene, &claim_ids[2], "accepted", &Author::Human).expect("accept");
            String::new()
        });
        let settled = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        assert!(
            settled.contains("- \"Persistence\" SETTLED: 2 claims, all accepted"),
            "{settled}"
        );
        assert!(
            settled.contains("answered: nowhere; it is a ring"),
            "{settled}"
        );
        assert!(settled.contains("WITHDRAWN by agent"), "{settled}");
        assert!(
            settled.contains("\"Persistence\" is now SETTLED"),
            "{settled}"
        );
    }

    #[test]
    fn atlas_claim_refuses_a_verified_claim_whose_range_does_not_hold() {
        let dir = temp_dir("challenge-stale");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "Persistence", "x": 0, "y": 0 }),
        )
        .expect("node");
        let id = state.read().expect("read").nodes[0].id.clone();
        let error = call(
            &state,
            &defs,
            "atlas_claim",
            json!({
                "about": id,
                "text": "stale",
                "basis": "verified",
                "path": "crates/ag-ui-surface/src/activity.rs",
                "lines": "999999"
            }),
        )
        .expect_err("stale range refused");
        assert!(error.contains("claim source does not hold"), "{error}");
        assert!(state.read().expect("read").claims.is_empty());
    }

    const CHALLENGE_BASELINE_REVISION: &str = "38012ed3dd144bfc29b34d4ba508b79e1af9b6b8";

    #[test]
    fn atlas_claim_and_its_revise_path_verify_a_present_revision_with_git() {
        let dir = temp_dir("challenge-revision-present");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        let about = place(&state, &defs, "Persistence", 0.0, 0.0);
        call(
            &state,
            &defs,
            "atlas_claim",
            json!({
                "about": about,
                "text": "The challenge contract exists",
                "basis": "verified",
                "path": "docs/design-challenge-v0.md",
                "lines": "1",
                "revision": CHALLENGE_BASELINE_REVISION
            }),
        )
        .expect("create a revision-pinned claim");
        let claim_id = state.read().expect("read").claims[0].id.clone();
        call(
            &state,
            &defs,
            "atlas_claim",
            json!({
                "id": claim_id,
                "about": about,
                "text": "The challenge contract names its board",
                "basis": "verified",
                "path": "docs/design-challenge-v0.md",
                "lines": "1-2",
                "revision": CHALLENGE_BASELINE_REVISION
            }),
        )
        .expect("revise against the same historical source");

        let saved = state.read().expect("read").claims[0].clone();
        assert_eq!(saved.revision, CHALLENGE_BASELINE_REVISION);
        assert_eq!(saved.lines, "1-2");
        assert_eq!(saved.text, "The challenge contract names its board");
    }

    #[test]
    fn atlas_claim_refuses_a_revision_missing_from_git() {
        let dir = temp_dir("challenge-revision-missing");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        let about = place(&state, &defs, "Persistence", 0.0, 0.0);
        let missing = "f".repeat(40);
        let error = call(
            &state,
            &defs,
            "atlas_claim",
            json!({
                "about": about,
                "text": "unreadable revision",
                "basis": "verified",
                "path": "docs/design-challenge-v0.md",
                "lines": "1",
                "revision": missing
            }),
        )
        .expect_err("a missing commit must be refused");
        assert!(
            error.contains("is not a commit in this repository"),
            "{error}"
        );
        assert!(state.read().expect("read").claims.is_empty());
    }

    #[test]
    fn atlas_claim_refuses_a_path_missing_at_a_revision() {
        let dir = temp_dir("challenge-revision-path-missing");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        let about = place(&state, &defs, "Persistence", 0.0, 0.0);
        let error = call(
            &state,
            &defs,
            "atlas_claim",
            json!({
                "about": about,
                "text": "missing path",
                "basis": "verified",
                "path": "docs/not-present-at-the-baseline.md",
                "lines": "1",
                "revision": CHALLENGE_BASELINE_REVISION
            }),
        )
        .expect_err("a missing historical path must be refused");
        assert!(
            error.contains("does not exist as a file at revision"),
            "{error}"
        );
        assert!(state.read().expect("read").claims.is_empty());
    }

    #[test]
    fn atlas_claim_refuses_a_range_past_the_end_at_a_revision() {
        let dir = temp_dir("challenge-revision-range-past-end");
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (state, _ws) = state_rooted(&dir, &workspace, false);
        let defs = actions(&state);
        let about = place(&state, &defs, "Persistence", 0.0, 0.0);
        let error = call(
            &state,
            &defs,
            "atlas_claim",
            json!({
                "about": about,
                "text": "stale historical range",
                "basis": "verified",
                "path": "docs/design-challenge-v0.md",
                "lines": "1-999999",
                "revision": CHALLENGE_BASELINE_REVISION
            }),
        )
        .expect_err("a stale historical range must be refused");
        assert!(error.contains("ends past the end"), "{error}");
        assert!(error.contains(CHALLENGE_BASELINE_REVISION), "{error}");
        assert!(state.read().expect("read").claims.is_empty());
    }

    #[test]
    fn atlas_diff_returns_core_sentences_and_receipt_for_host_snapshots() {
        let dir = temp_dir("atlas-diff");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let id = place(&state, &defs, "Persistence", 0.0, 0.0);
        let before = state.read().expect("read before");
        let before_snapshot = serde_json::to_value(
            SurfaceState::snapshot(state.as_ref()).expect("host snapshot before"),
        )
        .expect("snapshot object before");
        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "id": id, "x": 240, "y": 120 }),
        )
        .expect("move the same node");
        let after = state.read().expect("read after");
        let after_snapshot = serde_json::to_string(
            &SurfaceState::snapshot(state.as_ref()).expect("host snapshot after"),
        )
        .expect("snapshot string after");

        let reply = call(
            &state,
            &defs,
            "atlas_diff",
            json!({ "before": before_snapshot, "after": after_snapshot }),
        )
        .expect("atlas_diff")
        .expect("receipt JSON");
        let receipt: atlas::DiffReceipt = serde_json::from_str(&reply).expect("diff receipt");
        assert_eq!(receipt, before.diff_receipt(&after));
        assert!(
            receipt
                .sentences
                .iter()
                .any(|sentence| sentence.starts_with("moved node \"Persistence\"")),
            "{:?}",
            receipt.sentences
        );
        assert_eq!(receipt.receipt.len(), 64);
    }

    /// The `parent` field the batch action inherits from the single-node one
    /// is a real claim, so it gets its own behavior test rather than riding on
    /// atlas_place's. Nesting goes deeper than one level and the read-back
    /// indents each level.
    #[test]
    fn atlas_draw_fills_a_container_it_creates_in_the_same_call() {
        let dir = temp_dir("draw-containment");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        call(
            &state,
            &defs,
            "atlas_draw",
            json!({
                "nodes": [
                    { "ref": "platform", "label": "Platform" },
                    { "ref": "runtime", "label": "Runtime", "parent": "platform" },
                    { "ref": "queue", "label": "Queue turn", "parent": "runtime" }
                ],
                "links": [{ "from": "runtime", "to": "queue", "label": "holds" }]
            }),
        )
        .expect("batch with containment");

        let atlas = state.read().expect("read");
        let by_label = |label: &str| {
            atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("no {label}: {}", atlas.describe()))
        };
        let platform = by_label("Platform");
        let runtime = by_label("Runtime");
        let queue = by_label("Queue turn");
        assert_eq!(runtime.parent, platform.id, "{}", atlas.describe());
        assert_eq!(queue.parent, runtime.id, "{}", atlas.describe());
        assert_eq!(atlas.depth(&queue.id), 2, "{}", atlas.describe());
        assert!(atlas.contains_node(&platform.id, &queue.id));

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back text");
        let line = |label: &str| {
            read_back
                .lines()
                .find(|line| line.contains(&format!("\"{label}\" tone=")))
                .unwrap_or_else(|| panic!("no line for {label}: {read_back}"))
        };
        assert!(line("Platform").starts_with("- ["), "{read_back}");
        assert!(line("Runtime").starts_with("  - ["), "{read_back}");
        assert!(
            line("Queue turn").starts_with("    - ["),
            "the second level is not indented: {read_back}"
        );
        assert!(
            read_back.contains(
                "\"Runtime\" (inside \"Platform\") -> \"Queue turn\" (inside \"Runtime\") : holds"
            ),
            "{read_back}"
        );

        // A batch that names a container it never creates is refused whole.
        let error = call(
            &state,
            &defs,
            "atlas_draw",
            json!({ "nodes": [{ "ref": "stray", "label": "Stray", "parent": "nowhere" }] }),
        )
        .expect_err("an unknown parent must be refused");
        assert!(error.contains("neither a node on the atlas"), "{error}");
        assert!(
            state.read().expect("read").node("nowhere").is_none(),
            "the refused batch wrote something"
        );
    }

    /// The style register through the tool the agent actually calls.
    #[test]
    fn atlas_place_authors_the_style_register() {
        let dir = temp_dir("style-place");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let id_of = |reply: Option<String>| -> String {
            reply
                .expect("reply")
                .split_whitespace()
                .nth(1)
                .expect("id")
                .to_string()
        };
        let id = id_of(
            call(
                &state,
                &defs,
                "atlas_place",
                json!({
                    "label": "Silent drift",
                    "kind": "diamond",
                    "emphasis": "strong",
                    "size": "hero",
                    "color": "red"
                }),
            )
            .expect("styled place"),
        );
        let atlas = state.read().expect("read");
        let node = atlas.node(&id).expect("node");
        assert_eq!(node.kind, "diamond");
        assert_eq!(node.emphasis, "strong");
        assert_eq!(node.size, "hero");
        assert_eq!(node.color, "red");

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        let line = read_back
            .lines()
            .find(|line| line.contains("Silent drift"))
            .expect("node line");
        assert!(line.contains("kind=diamond"), "{read_back}");
        assert!(line.contains("emphasis=strong"), "{read_back}");
        assert!(line.contains("size=hero"), "{read_back}");
        assert!(line.contains("color=red"), "{read_back}");

        // A word outside the vocabulary is refused rather than stored, and the
        // card keeps the style it had.
        let error = call(
            &state,
            &defs,
            "atlas_place",
            json!({ "id": id, "emphasis": "shouty" }),
        )
        .expect_err("an unknown emphasis must be refused");
        assert!(error.contains("emphasis"), "{error}");
        assert_eq!(
            state
                .read()
                .expect("read")
                .node(&id)
                .expect("node")
                .emphasis,
            "strong"
        );

        // Restyling through the tool changes the card and takes the colour
        // off. The change SENTENCE is not asserted here on purpose: this
        // reader is the agent that just wrote it, and `atlas_read` discounts
        // a reader's own writes by design, so demanding the line would be
        // demanding the surface hand the agent back its own work.
        // `a_restyle_reads_as_its_own_change_naming_who_and_what_moved` in the
        // core crate is where the sentences are proven, including the human
        // restyle that the agent does need told about.
        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "id": id, "emphasis": "muted", "color": "" }),
        )
        .expect("restyle");
        let atlas = state.read().expect("read");
        let node = atlas.node(&id).expect("node");
        assert_eq!(node.emphasis, "muted");
        assert_eq!(node.color, "");
        assert_eq!(node.kind, "diamond", "an unmentioned style was clobbered");
        assert_eq!(node.size, "hero", "an unmentioned style was clobbered");
        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        let line = read_back
            .lines()
            .find(|line| line.contains("Silent drift"))
            .expect("node line");
        assert!(line.contains("emphasis=muted"), "{read_back}");
        assert!(!line.contains("color="), "{read_back}");
    }

    /// The G5b lesson again, on the four fields `atlas_diagram` used to
    /// validate and then throw away. A tool that accepts a field and discards
    /// it teaches the agent that the field works.
    #[test]
    fn atlas_diagram_stores_the_styles_it_used_to_discard() {
        let dir = temp_dir("style-diagram");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let reply = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "nodes": [
                    { "id": "start", "label": "Start", "shape": "ellipse", "size": "hero" },
                    { "id": "branch", "label": "Branch", "shape": "diamond", "color": "amber" },
                    { "id": "risk", "label": "Risk", "color": "#f87171", "filled": true },
                    { "id": "plain", "label": "Plain" }
                ],
                "edges": [
                    { "from": "start", "to": "branch" },
                    { "from": "branch", "to": "risk" },
                    { "from": "risk", "to": "plain" }
                ]
            }),
        )
        .expect("diagram")
        .unwrap_or_default();
        assert!(
            reply.contains("Stored kind, colour, emphasis or size on 3 node(s)"),
            "{reply}"
        );
        assert!(
            !reply.contains("Normalized"),
            "the reply still says the styles were normalized away: {reply}"
        );

        let atlas = state.read().expect("read");
        let by_label = |label: &str| {
            atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("no {label}: {}", atlas.describe()))
        };
        assert_eq!(by_label("Start").kind, "ellipse");
        assert_eq!(by_label("Start").size, "hero");
        assert_eq!(by_label("Branch").kind, "diamond");
        assert_eq!(by_label("Branch").color, "amber");
        // A hex snaps to the nearest palette NAME, so the read-back hands back
        // a word the model can reason about.
        assert_eq!(by_label("Risk").color, "red");
        assert_eq!(by_label("Risk").emphasis, "strong");
        // And a node that chose nothing is left alone.
        assert_eq!(by_label("Plain").kind, atlas::DEFAULT_KIND);
        assert_eq!(by_label("Plain").emphasis, atlas::DEFAULT_EMPHASIS);
        assert_eq!(by_label("Plain").color, "");
        assert!(by_label("Plain").style_words().is_empty());

        // Nothing landed in the drawn register on the way.
        assert!(atlas.shapes.is_empty(), "{}", atlas.describe());

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        assert!(read_back.contains("kind=ellipse size=hero"), "{read_back}");
        assert!(
            read_back.contains("emphasis=strong color=red"),
            "{read_back}"
        );
        assert!(
            !read_back.contains('#'),
            "hex reached the read-back: {read_back}"
        );

        // `filled` and `emphasis` that disagree are refused, not reconciled.
        let error = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "nodes": [{ "id": "x", "label": "X", "filled": true, "emphasis": "muted" }]
            }),
        )
        .expect_err("two explicit style claims that disagree must be refused");
        assert!(error.contains("disagree"), "{error}");

        let error = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({ "nodes": [{ "id": "x", "label": "X", "emphasis": "shouty" }] }),
        )
        .expect_err("an unknown emphasis must be refused");
        assert!(error.contains("emphasis"), "{error}");
    }

    /// A source-backed architecture diagram must be one atomic operation.
    /// Requiring a second `atlas_place` pass to attach evidence changes card
    /// height after layout, which can turn a clean graph into overlapping
    /// cards and links routed under them. It also means a stale source can be
    /// discovered only after the rest of the graph already landed.
    #[test]
    fn atlas_diagram_verifies_and_packs_source_backed_nodes_in_the_original_batch() {
        let dir = temp_dir("source-backed-diagram");
        let (diagram_state, _ws) = state(&dir);
        let defs = actions(&diagram_state);
        call(
            &diagram_state,
            &defs,
            "atlas_diagram",
            json!({
                "direction": "down",
                "nodes": [
                    {
                        "id": "evidence",
                        "label": "Host verifies the source before writing",
                        "path": "Cargo.toml",
                        "lines": "1-3",
                        "note": "The evidence belongs to the card in the same transaction.\nIts extra lines are part of layout, not a later surprise.",
                        "group": "Evidence path"
                    },
                    { "id": "accept", "label": "Accept the claim", "group": "Evidence path" },
                    { "id": "publish", "label": "Publish the excerpt", "group": "Evidence path" },
                    { "id": "inspect", "label": "Human inspects it", "group": "Evidence path" }
                ],
                "edges": [
                    { "from": "evidence", "to": "accept" },
                    { "from": "accept", "to": "publish" },
                    { "from": "publish", "to": "inspect" }
                ]
            }),
        )
        .expect("source-backed diagram");

        let atlas = diagram_state.read().expect("read");
        let evidence = atlas
            .nodes
            .iter()
            .find(|node| node.label == "Host verifies the source before writing")
            .expect("evidence node");
        assert_eq!(evidence.path, "Cargo.toml");
        assert_eq!(evidence.lines, "1-3");
        assert!(
            evidence.w >= 280.0,
            "source cards need room for the evidence before layout"
        );
        assert!(evidence.note.contains("same transaction"));
        assert!(
            atlas::layout(&atlas).problems.is_empty(),
            "source and note height was not included in the original packing: {}",
            atlas.describe()
        );

        let stale_dir = temp_dir("source-backed-diagram-stale");
        let (stale, _ws) = state(&stale_dir);
        let stale_defs = actions(&stale);
        let error = call(
            &stale,
            &stale_defs,
            "atlas_diagram",
            json!({
                "nodes": [
                    { "id": "safe", "label": "Would otherwise land" },
                    {
                        "id": "stale",
                        "label": "Stale evidence",
                        "path": "Cargo.toml",
                        "lines": "999999"
                    }
                ],
                "edges": [{ "from": "safe", "to": "stale" }]
            }),
        )
        .expect_err("a stale source must refuse the whole diagram");
        assert!(error.contains("source reference"), "{error}");
        assert!(
            stale.read().expect("read stale state").nodes.is_empty(),
            "a source failure left a partial diagram behind"
        );

        let schema = &stale_defs
            .iter()
            .find(|def| def.name == "atlas_diagram")
            .expect("atlas_diagram")
            .parameters;
        for field in ["path", "lines", "note"] {
            assert!(
                schema["properties"]["nodes"]["items"]["properties"]
                    .get(field)
                    .is_some(),
                "atlas_diagram schema does not advertise {field}"
            );
        }
    }

    #[test]
    fn atlas_diagram_direction_controls_fixed_rectangle_packing() {
        let dimensions = |direction: &str| {
            let dir = temp_dir(&format!("diagram-direction-{direction}"));
            let (state, _ws) = state(&dir);
            let defs = actions(&state);
            let nodes = (0..7)
                .map(|index| {
                    json!({
                        "id": format!("n{index}"),
                        "label": format!("Stage {index} with enough words")
                    })
                })
                .collect::<Vec<_>>();
            let edges = (0..6)
                .map(|index| {
                    json!({
                        "from": format!("n{index}"),
                        "to": format!("n{}", index + 1)
                    })
                })
                .collect::<Vec<_>>();
            call(
                &state,
                &defs,
                "atlas_diagram",
                json!({ "direction": direction, "nodes": nodes, "edges": edges }),
            )
            .expect("diagram")
            .expect("text");
            let atlas = state.read().expect("read diagram");
            let (left, top, right, bottom) = atlas::layout(&atlas).extent.expect("extent");
            (right - left, bottom - top)
        };

        let (right_width, right_height) = dimensions("right");
        let (down_width, down_height) = dimensions("down");
        let right_verticality = right_height / right_width;
        let down_verticality = down_height / down_width;
        assert!(
            down_verticality > right_verticality + 0.5,
            "right={right_width}x{right_height}, down={down_width}x{down_height}"
        );
        assert_ne!(
            (right_width, right_height),
            (down_width, down_height),
            "direction must change the published geometry"
        );
    }

    /// Item 4. The serving protocol's form-carries-meaning rules, and the
    /// example payload that states them, executed rather than trusted. The
    /// payload is parsed out of AGENTS.md itself, so the document cannot drift
    /// away from what the tools do.
    #[test]
    fn the_serving_protocol_example_payload_lands_the_meaning_it_claims() {
        let guidance = include_str!("../AGENTS.md");
        for rule in [
            "weight is importance",
            "enclosure is ownership",
            "colour is category",
            "shape is what a thing is",
        ] {
            assert!(guidance.contains(rule), "the serving protocol lost: {rule}");
        }
        assert!(
            guidance.contains("enforces the vocabulary and never the interpretation"),
            "the guidance no longer says the meaning is the agent's to choose"
        );
        assert!(
            guidance.contains(
                "Say what your colours and your
   emphasis mean"
            ),
            "the guidance no longer tells the agent to state its scheme"
        );

        // Pull the payload the document shows out of the document.
        let payload = guidance
            .split("```json")
            .nth(1)
            .expect("the serving protocol has no example payload")
            .split("```")
            .next()
            .expect("unterminated example payload");
        let payload: JsonValue =
            serde_json::from_str(payload).expect("the example payload is not valid JSON");

        let dir = temp_dir("serving-example");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        call(&state, &defs, "atlas_diagram", payload).expect("the example payload must author");

        let atlas = state.read().expect("read");
        let by_label = |label: &str| {
            atlas
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("no {label}: {}", atlas.describe()))
        };

        // Enclosure is ownership: two containers own the cards, and it is real
        // containment rather than a drawn box.
        for container in ["Seat", "Engine"] {
            let container = by_label(container);
            assert!(
                !atlas.children_of(&container.id).is_empty(),
                "{}",
                atlas.describe()
            );
        }
        assert!(
            atlas.shapes.is_empty(),
            "a drawn box crept in: {}",
            atlas.describe()
        );

        // Weight is importance: exactly one card is emphasised.
        let emphasised = atlas
            .nodes
            .iter()
            .filter(|node| node.emphasis == "strong")
            .map(|node| node.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(emphasised, vec!["Silent drift"], "{}", atlas.describe());

        // Colour is category: two cards share blue, one is red, and the
        // containers and boundaries are uncoloured.
        let coloured = |color: &str| {
            let mut found = atlas
                .nodes
                .iter()
                .filter(|node| node.color == color)
                .map(|node| node.label.clone())
                .collect::<Vec<_>>();
            found.sort();
            found
        };
        assert_eq!(
            coloured("blue"),
            vec![
                "Author the picture".to_string(),
                "Verify against LAYOUT".to_string()
            ],
            "{}",
            atlas.describe()
        );
        assert_eq!(coloured("red"), vec!["Silent drift".to_string()]);

        // Shape is what a thing is: the loop's boundaries are ellipses and the
        // lane choice is a diamond.
        assert_eq!(by_label("Human ask").kind, "ellipse");
        assert_eq!(by_label("Human sees it").kind, "ellipse");
        assert_eq!(by_label("Pick a lane").kind, "diamond");
        assert_eq!(by_label("Author the picture").kind, atlas::DEFAULT_KIND);
        assert_eq!(by_label("Human sees it").size, "hero");

        // And every one of those reaches the agent's read-back.
        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back");
        for expected in [
            "emphasis=strong",
            "color=red",
            "color=blue",
            "kind=diamond",
            "kind=ellipse",
            "size=hero",
            "CONTAINS",
        ] {
            assert!(
                read_back.contains(expected),
                "{expected} missing: {read_back}"
            );
        }
    }

    /// The G5b pairing for the style register: every claim these descriptions
    /// make is proven by a named behavior test above, and the wording is
    /// pinned so the pair cannot drift apart silently.
    #[test]
    fn atlas_sketch_enums_are_the_core_lists_not_a_second_copy() {
        let dir = temp_dir("sketch-enums");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let sketch = defs
            .iter()
            .find(|def| def.name == "atlas_sketch")
            .expect("atlas_sketch");
        let shape = &sketch.parameters["properties"]["shapes"]["items"]["properties"];
        let enum_of = |field: &str| -> Vec<String> {
            shape[field]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("atlas_sketch {field} has no enum"))
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_eq!(enum_of("form"), atlas::FORMS);
        assert_eq!(enum_of("head"), atlas::HEADS);
        assert_eq!(enum_of("ink"), atlas::INKS);
        assert_eq!(enum_of("stroke_style"), atlas::STROKE_STYLES);
        assert_eq!(enum_of("roundness"), atlas::ROUNDNESS);
        assert_eq!(
            enum_of("fill"),
            std::iter::once("none")
                .chain(atlas::INKS.iter().copied())
                .collect::<Vec<_>>()
        );
        // Every attribute the model stores is authorable, with a description
        // that says what it means, not only what type it is.
        for field in [
            "stroke_width",
            "stroke_style",
            "opacity",
            "roundness",
            "font_size",
            "angle",
            "groups",
            "frame",
        ] {
            let words = shape[field]["description"].as_str().unwrap_or_default();
            assert!(words.len() > 40, "atlas_sketch {field}: {words:?}");
        }
        assert!(enum_of("form").contains(&"frame".to_string()));
    }

    #[test]
    fn style_descriptions_match_style_behavior() {
        let dir = temp_dir("style-descriptions");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let def = |name: &str| -> &ToolDef {
            defs.iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
        };

        // The form-carries-meaning rules, in both authoring tools, stated as
        // the agent's choice rather than as the atlas's rule.
        for name in ["atlas_place", "atlas_diagram"] {
            let text = &def(name).description;
            assert!(text.contains("FORM CARRIES MEANING"), "{name}: {text}");
            assert!(text.contains("weight is importance"), "{name}: {text}");
            assert!(text.contains("enclosure is ownership"), "{name}: {text}");
            assert!(text.contains("colour is category"), "{name}: {text}");
            assert!(
                text.contains("never decides what it means"),
                "{name} claims to own the meaning: {text}"
            );
            assert!(!text.contains('\u{2014}'), "{name}: {text}");
            assert!(!text.contains('\u{2013}'), "{name}: {text}");
        }

        // atlas_diagram's old promise is gone: the hints are stored now.
        let diagram = &def("atlas_diagram").description;
        assert!(
            !diagram.contains("compatibility hints"),
            "the description still calls the style fields hints: {diagram}"
        );
        assert!(
            !diagram.contains("normalized to shared Atlas cards"),
            "the description still promises the styles are discarded: {diagram}"
        );
        assert!(diagram.contains("stored on the node itself"), "{diagram}");

        // Every style field carries an honest schema description, and the
        // enums are the model's closed lists rather than a second copy.
        let place = &def("atlas_place").parameters["properties"];
        let words = |field: &str| -> String {
            place[field]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("atlas_place {field} has no description"))
                .to_string()
        };
        let enum_of = |field: &str| -> Vec<String> {
            place[field]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("atlas_place {field} has no enum"))
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_eq!(enum_of("kind"), atlas::NODE_KINDS);
        assert_eq!(enum_of("emphasis"), atlas::EMPHASES);
        assert_eq!(enum_of("size"), atlas::SIZES);
        assert_eq!(
            enum_of("color"),
            std::iter::once("")
                .chain(atlas::INKS.iter().copied())
                .collect::<Vec<_>>()
        );
        // Proven by atlas_place_authors_the_style_register.
        assert!(
            words("color").contains("empty string"),
            "{}",
            words("color")
        );
        // Proven by the_serving_protocol_example_payload_lands_the_meaning_it_claims.
        assert!(
            words("emphasis").contains("stand out"),
            "{}",
            words("emphasis")
        );
        // emphasis and size stay distinguishable, which is the whole reason
        // there are two fields.
        assert!(
            words("size").contains("emphasis is weight"),
            "{}",
            words("size")
        );
        assert!(
            words("emphasis").contains("that is `size`"),
            "{}",
            words("emphasis")
        );
        // Proven by atlas_diagram_stores_the_styles_it_used_to_discard.
        let filled = def("atlas_diagram").parameters["properties"]["nodes"]["items"]["properties"]
            ["filled"]["description"]
            .as_str()
            .expect("filled description");
        assert!(filled.contains("refused rather than guessed"), "{filled}");
    }

    /// R6. A containment cycle is refused with words that name both ends,
    /// through the tool the agent actually calls.
    #[test]
    fn atlas_place_refuses_a_containment_cycle() {
        let dir = temp_dir("containment-cycle");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let place = |args: JsonValue| call(&state, &defs, "atlas_place", args);

        let outer = place(json!({ "label": "Platform" }))
            .expect("outer")
            .expect("outer reply");
        let outer = outer
            .split_whitespace()
            .nth(1)
            .expect("outer id")
            .to_string();
        let inner = place(json!({ "label": "Runtime", "parent": outer }))
            .expect("inner")
            .expect("inner reply");
        let inner = inner
            .split_whitespace()
            .nth(1)
            .expect("inner id")
            .to_string();
        assert_eq!(
            state
                .read()
                .expect("read")
                .node(&inner)
                .expect("inner node")
                .parent,
            outer
        );

        let error = place(json!({ "id": outer, "parent": inner }))
            .expect_err("a cycle must be refused, not stored");
        assert!(error.contains("its own container"), "{error}");
        assert!(error.contains("Platform"), "{error}");
        assert!(error.contains("Runtime"), "{error}");
        assert!(error.contains("has to stay a tree"), "{error}");

        let error = place(json!({ "id": inner, "parent": "no-such-node" }))
            .expect_err("a parent that does not exist must be refused");
        assert!(error.contains("no node \"no-such-node\""), "{error}");

        // The refusals stored nothing.
        let atlas = state.read().expect("read");
        assert_eq!(atlas.node(&outer).expect("outer").parent, "");
        assert_eq!(atlas.node(&inner).expect("inner").parent, outer);
    }

    /// R7. Deleting a container keeps what was inside it and the next read
    /// says where each member went.
    #[test]
    fn removing_a_container_reads_back_the_orphan_disposition() {
        let dir = temp_dir("container-orphans");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let id_of = |reply: Option<String>| -> String {
            reply
                .expect("reply")
                .split_whitespace()
                .nth(1)
                .expect("id")
                .to_string()
        };
        let platform = id_of(
            call(&state, &defs, "atlas_place", json!({ "label": "Platform" })).expect("platform"),
        );
        let runtime = id_of(
            call(
                &state,
                &defs,
                "atlas_place",
                json!({ "label": "Runtime", "parent": platform }),
            )
            .expect("runtime"),
        );
        let queue = id_of(
            call(
                &state,
                &defs,
                "atlas_place",
                json!({ "label": "Queue turn", "parent": runtime }),
            )
            .expect("queue"),
        );
        let worker = id_of(
            call(
                &state,
                &defs,
                "atlas_place",
                json!({ "label": "Run provider", "parent": runtime }),
            )
            .expect("worker"),
        );

        // Baseline read so the next one carries a delta.
        call(&state, &defs, "atlas_read", json!({})).expect("baseline read");

        let removed = call(&state, &defs, "atlas_remove", json!({ "id": runtime }))
            .expect("remove container")
            .expect("remove reply");
        assert!(removed.contains("removed 1 object(s)"), "{removed}");

        let atlas = state.read().expect("read");
        assert!(
            atlas.node(&queue).is_some() && atlas.node(&worker).is_some(),
            "deleting a container deleted its members: {}",
            atlas.describe()
        );
        assert_eq!(atlas.node(&queue).expect("queue").parent, platform);
        assert_eq!(atlas.node(&worker).expect("worker").parent, platform);

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back text");
        for label in ["Queue turn", "Run provider"] {
            assert!(
                read_back.lines().any(|line| line.contains(label)
                    && line.contains("moved up into")
                    && line.contains("\"Platform\"")
                    && line.contains("its container \"Runtime\" was deleted")),
                "{label} has no orphan disposition: {read_back}"
            );
        }

        // Deleting the outermost container leaves them at the top level, and
        // that disposition is stated too.
        call(&state, &defs, "atlas_remove", json!({ "id": platform })).expect("remove platform");
        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("read-back text");
        for label in ["Queue turn", "Run provider"] {
            assert!(
                read_back.lines().any(|line| line.contains(label)
                    && line.contains("moved up to the top level")
                    && line.contains("its container \"Platform\" was deleted")),
                "{label} has no top-level orphan disposition: {read_back}"
            );
        }
        let atlas = state.read().expect("read");
        assert_eq!(atlas.roots().len(), 2, "{}", atlas.describe());
    }

    /// The G5b lesson, applied: every containment claim these descriptions
    /// make is paired with a behavior test above, and this test pins the
    /// wording so the pair cannot drift apart silently.
    #[test]
    fn containment_descriptions_match_containment_behavior() {
        let dir = temp_dir("containment-descriptions");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let def = |name: &str| -> &ToolDef {
            defs.iter()
                .find(|def| def.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
        };

        // atlas_place: "set `parent` to put this node inside another one",
        // proven by two_diagram_groups_become_two_containers_of_three and
        // removing_a_container_reads_back_the_orphan_disposition; "a cycle is
        // refused rather than stored", proven by
        // atlas_place_refuses_a_containment_cycle.
        let place = &def("atlas_place").description;
        assert!(place.contains("`parent`"), "{place}");
        assert!(place.contains("inside another one"), "{place}");
        assert!(place.contains("cycle is refused"), "{place}");
        let parent_field = def("atlas_place").parameters["properties"]["parent"]["description"]
            .as_str()
            .expect("atlas_place parent field description");
        assert!(parent_field.contains("INSIDE"), "{parent_field}");
        assert!(parent_field.contains("empty string"), "{parent_field}");

        // atlas_diagram: "a `group` becomes a real container node", proven by
        // two_diagram_groups_become_two_containers_of_three; the deletion
        // sentence is proven by
        // removing_a_container_reads_back_the_orphan_disposition.
        let diagram = &def("atlas_diagram").description;
        assert!(diagram.contains("real container node"), "{diagram}");
        assert!(diagram.contains("(inside PARENT)"), "{diagram}");
        assert!(
            !diagram.contains("group constraint"),
            "the description still promises the register it no longer writes: {diagram}"
        );
        let group_field = def("atlas_diagram").parameters["properties"]["nodes"]["items"]
            ["properties"]["group"]["description"]
            .as_str()
            .expect("atlas_diagram group field description");
        assert!(group_field.contains("container node"), "{group_field}");

        // atlas_remove: "removing a container does NOT remove what was inside
        // it", proven by removing_a_container_reads_back_the_orphan_disposition.
        let remove = &def("atlas_remove").description;
        assert!(remove.contains("does NOT"), "{remove}");
        assert!(
            remove.contains("moves up one level") || remove.contains("moves up"),
            "{remove}"
        );

        // atlas_link is unchanged by P1b and still says only what it does.
        let link = &def("atlas_link").description;
        assert!(!link.contains("parent"), "{link}");
        assert!(!link.contains("container"), "{link}");

        // NN-1 holds on every string P1b touched.
        for name in ["atlas_place", "atlas_diagram", "atlas_remove"] {
            let text = &def(name).description;
            assert!(!text.contains('\u{2014}'), "{name}: {text}");
            assert!(!text.contains('\u{2013}'), "{name}: {text}");
        }
    }

    #[test]
    fn diagram_validation_finishes_before_the_first_node_lands() {
        let dir = temp_dir("diagram-atomic");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let error = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "title": "x".repeat(atlas::MAX_LABEL + 1),
                "nodes": [{ "id": "safe", "label": "Safe" }]
            }),
        )
        .expect_err("an oversized late field must reject the whole diagram");
        assert!(error.contains("diagram title"), "{error}");
        assert!(state.read().expect("read").nodes.is_empty());

        let error = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "origin_x": "far away",
                "nodes": [{ "id": "safe", "label": "Safe" }]
            }),
        )
        .expect_err("a mistyped origin must not fall back silently");
        assert!(error.contains("finite number"), "{error}");
        assert!(state.read().expect("read").nodes.is_empty());

        let error = call(
            &state,
            &defs,
            "atlas_diagram",
            json!({
                "origin_x": atlas::WORLD_LIMIT,
                "nodes": [{ "id": "safe", "label": "Safe" }]
            }),
        )
        .expect_err("a clamped diagram would lie about its layout");
        assert!(error.contains("world limit"), "{error}");
        assert!(state.read().expect("read").nodes.is_empty());
    }

    #[test]
    fn a_batch_with_one_bad_source_reference_draws_nothing() {
        let dir = temp_dir("draw-verify");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        let error = call(
            &state,
            &defs,
            "atlas_draw",
            json!({
                "nodes": [
                    { "ref": "good", "label": "manifest", "path": "Cargo.toml", "lines": "1-3" },
                    { "ref": "bad", "label": "ghost", "path": "src/does-not-exist.rs" }
                ]
            }),
        )
        .expect_err("one unverifiable claim must fail the batch");

        assert!(
            error.contains("nodes[1]"),
            "the caller is told which: {error}"
        );
        assert!(
            state.read().expect("read back").nodes.is_empty(),
            "verification happens before any write, so a rejected batch leaves \
             nothing half-drawn on the human's screen"
        );
    }

    #[test]
    fn the_human_edit_is_visible_in_the_agents_next_read() {
        let dir = temp_dir("read-back");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "runtime kernel", "x": 100, "y": 100 }),
        )
        .expect("place");
        let id = state.read().expect("read").nodes[0].id.clone();

        // The human's browser replica: a separate Doc that merges in.
        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        atlas::place_node(
            &mut browser,
            &atlas::NodePatch {
                id: Some(id.clone()),
                x: Some(700.0),
                y: Some(420.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("human drags");
        atlas::mark(
            &mut browser,
            &id,
            "?",
            "why is this the kernel?",
            &Author::Human,
        )
        .expect("human marks");

        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("sv"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts the human's edit");

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(read_back.contains("(700,420)"), "{read_back}");
        assert!(read_back.contains("LAST-EDITED-BY=human"), "{read_back}");
        assert!(read_back.contains("why is this the kernel?"), "{read_back}");
        assert!(read_back.contains("UNANSWERED"), "{read_back}");
    }

    #[test]
    fn atlas_answer_answers_a_shape_mark_in_place() {
        let dir = temp_dir("shape-mark-answer");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_sketch",
            json!({
                "shapes": [{
                    "form": "rect",
                    "x": 100,
                    "y": 100,
                    "w": 240,
                    "h": 120,
                    "label": "stale trust boundary"
                }]
            }),
        )
        .expect("agent draws shape");
        call(&state, &defs, "atlas_read", json!({})).expect("initial read");

        let shape = state.read().expect("read shape").shapes[0].id.clone();
        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        let mark = atlas::mark(
            &mut browser,
            &shape,
            "!",
            "this boundary is stale",
            &Author::Human,
        )
        .expect("human marks agent-drawn shape");
        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("state vector"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts human shape mark");

        let unanswered = call(&state, &defs, "atlas_read", json!({}))
            .expect("read unanswered shape mark")
            .expect("text");
        assert!(unanswered.contains("MARKS (what the human flagged on the map)"));
        assert!(unanswered.contains("stale trust boundary"), "{unanswered}");
        assert!(unanswered.contains("thinks this is wrong"), "{unanswered}");
        assert!(unanswered.contains("UNANSWERED"), "{unanswered}");

        call(
            &state,
            &defs,
            "atlas_answer",
            json!({
                "mark_id": mark,
                "answer": "Agreed. The transport now decides authority."
            }),
        )
        .expect("atlas_answer accepts shape mark");

        let answered = call(&state, &defs, "atlas_read", json!({}))
            .expect("read answered shape mark")
            .expect("text");
        let mark_at = answered
            .find("this boundary is stale")
            .expect("same mark remains in place");
        let answer_at = answered
            .find("answered: Agreed. The transport now decides authority.")
            .expect("answer appears beside the mark");
        assert!(answer_at > mark_at, "{answered}");
        assert!(
            !answered[mark_at..answer_at].contains("UNANSWERED"),
            "{answered}"
        );
    }

    #[test]
    fn the_atlas_survives_a_restart() {
        let dir = temp_dir("persist");
        {
            let (state, _ws) = state(&dir);
            let defs = actions(&state);
            call(&state, &defs, "atlas_place", json!({ "label": "kept" })).expect("place");
            state.persist(true);
        }
        let (reopened, _ws) = state(&dir);
        let atlas = reopened.read().expect("read");
        assert_eq!(atlas.nodes.len(), 1);
        assert_eq!(atlas.nodes[0].label, "kept");
    }

    #[test]
    fn an_agent_write_reaches_the_binary_transport() {
        let dir = temp_dir("broadcast");
        let (state, mut ws) = state(&dir);
        let defs = actions(&state);
        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "broadcast me" }),
        )
        .expect("place");
        let frame = ws.try_recv().expect("a frame was broadcast");
        assert!(matches!(decode_frame(&frame), Ok(Frame::Sync(_))));
    }

    /// An `.excalidraw` file already in the project lands as real shapes the
    /// human can then move, and says plainly what it could not bring.
    #[test]
    fn an_import_lands_a_diagram_that_already_exists_in_the_project() {
        let dir = temp_dir("import");
        let root = temp_dir("import-repo");
        std::fs::write(
            root.join("diagram.excalidraw"),
            r##"{
                "type": "excalidraw", "version": 2, "elements": [
                    { "type": "rectangle", "id": "a", "x": 40, "y": 60, "width": 180, "height": 90,
                      "strokeColor": "#6ea8ff", "backgroundColor": "transparent" },
                    { "type": "text", "id": "a-label", "containerId": "a", "text": "the gate" },
                    { "type": "arrow", "id": "b", "x": 220, "y": 105, "width": 120, "height": 0,
                      "points": [[0, 0], [60, -30], [120, 0]], "strokeColor": "#f87171", "endArrowhead": "arrow",
                      "startBinding": { "elementId": "a" } },
                    { "type": "image", "id": "c", "x": 0, "y": 0, "width": 10, "height": 10 }
                ]
            }"##,
        )
        .expect("write fixture");

        let (state, _ws) = state_rooted(&dir, &root, false);
        let defs = actions(&state);
        let reply = call(
            &state,
            &defs,
            "atlas_import",
            json!({ "path": "diagram.excalidraw", "dx": 1000.0 }),
        )
        .expect("import")
        .unwrap_or_default();

        assert!(reply.contains("imported 2 shape(s)"), "{reply}");
        assert!(
            reply.contains("Preserved bindings on 1 connector(s)"),
            "{reply}"
        );
        // The one element that could not come across is named, with a reason.
        assert!(reply.contains("NOT imported (1)"), "{reply}");
        assert!(reply.contains("image"), "{reply}");

        let atlas = state.read().expect("read");
        let box_shape = atlas
            .shapes
            .iter()
            .find(|shape| shape.form == "rect")
            .expect("the rectangle");
        // A bound text element out there is the container's label in here.
        assert_eq!(box_shape.label, "the gate");
        assert_eq!(box_shape.ink, "blue");
        // `dx` shifted it, so a diagram can land beside the map rather than on it.
        assert_eq!(box_shape.x, 1040.0);
        let arrow = atlas
            .shapes
            .iter()
            .find(|shape| shape.form == "arrow")
            .expect("arrow");
        assert_eq!(arrow.from, box_shape.id);
        assert_eq!(atlas.shape_path(arrow).expect("curved arrow").len(), 3);
    }

    #[test]
    fn boot_import_of_the_same_page_host_preserves_verified_and_inferred_claims() {
        let dir = temp_dir("archify-host-evidence");
        let (state, _ws) = state_rooted(&dir, &workspace_root(), false);
        let defs = actions(&state);
        let fixture = "examples/same-page-atlas/fixtures/archify/same-page-host.architecture.json";

        assert!(state.is_empty().expect("empty boot state"));
        let summary = state.import_file(fixture, 0.0, 0.0).expect("boot import");
        assert_eq!(
            summary,
            format!(
                "imported Archify architecture \"Same Page Atlas Host\" from {fixture}: 6 node(s), 1 frame(s), 6 relation(s), and 6 claim(s)"
            )
        );

        let imported = state.read().expect("imported Atlas");
        let claim_id = |label: &str| {
            let node = imported
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap_or_else(|| panic!("missing imported node {label:?}"));
            let claims = imported.claims_of(&node.id);
            assert_eq!(claims.len(), 1, "{label} must have one imported claim");
            claims[0].id.clone()
        };
        let revision = "38012ed3dd144bfc29b34d4ba508b79e1af9b6b8";
        let expected = format!(
            concat!(
                "\nCLAIMS (what the agent says it understands, and what the human said back)\n",
                "- \"Browser replica\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n",
                "    [{}] inferred \"Browser replica is a frontend architecture component: human CRDT peer over /ws\" open\n",
                "- \"MCP clients\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n",
                "    [{}] inferred \"MCP clients is an external architecture component: attach through /mcp\" open\n",
                "- \"Host composition\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n",
                "    [{}] verified examples/same-page-atlas/src/main.rs:244-269@{revision} \"Host composition is a backend architecture component: surface assembly and guarded boot import\" open\n",
                "- \"Atlas state\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n",
                "    [{}] verified examples/same-page-atlas/src/atlas.rs:140-180@{revision} \"Atlas state is a backend architecture component: shared scene and read cursor\" open\n",
                "- \"Claims read-back\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n",
                "    [{}] verified examples/same-page-atlas/core/src/claims.rs:549-603@{revision} \"Claims read-back is a database architecture component: basis and human verdicts\" open\n",
                "- \"Archify import\" UNREVIEWED: 1 claim, 1 open, 0 rejected, 0 accepted\n",
                "    [{}] verified examples/same-page-atlas/src/atlas.rs:745-793@{revision} \"Archify import is a backend architecture component: validated atomic landing\" open\n",
            ),
            claim_id("Browser replica"),
            claim_id("MCP clients"),
            claim_id("Host composition"),
            claim_id("Atlas state"),
            claim_id("Claims read-back"),
            claim_id("Archify import"),
            revision = revision,
        );
        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("text read-back");
        let start = read_back.find("\nCLAIMS (").expect("CLAIMS section");
        let end = read_back[start..]
            .find("\nLAYOUT\n")
            .map(|offset| start + offset)
            .unwrap_or(read_back.len());
        assert_eq!(&read_back[start..end], expected);
    }

    #[test]
    fn archify_web_app_boot_import_has_no_layout_problems_at_atlas_card_size() {
        let dir = temp_dir("archify-web-app-layout");
        let (state, _ws) = state_rooted(&dir, &workspace_root(), false);
        let defs = actions(&state);
        let fixture = "examples/same-page-atlas/fixtures/archify/web-app.architecture.json";

        assert!(state.is_empty().expect("empty boot state"));
        state.import_file(fixture, 0.0, 0.0).expect("boot import");
        let leaf_ids = {
            let imported = state.read().expect("imported Atlas");
            imported
                .nodes
                .iter()
                .filter(|node| !imported.is_container(&node.id))
                .map(|node| node.id.clone())
                .collect::<Vec<_>>()
        };
        {
            let scene = state.scene.lock();
            for id in &leaf_ids {
                atlas::measure_node(&scene, id, import_archify::ARCHITECTURE_CARD_HEIGHT)
                    .expect("measure imported card at the reserved Atlas height");
            }
        }

        let read_back = call(&state, &defs, "atlas_read", json!({}))
            .expect("atlas_read")
            .expect("text read-back");
        assert!(
            read_back.contains("No layout problems detected in the available geometry."),
            "{read_back}"
        );
        assert!(!read_back.contains("\nPROBLEMS ("), "{read_back}");

        let imported = state.read().expect("measured imported Atlas");
        for child in imported.nodes.iter().filter(|node| !node.parent.is_empty()) {
            let parent = imported.node(&child.parent).expect("imported parent frame");
            let (pl, pt, pr, pb) = parent.bounds();
            let (cl, ct, cr, cb) = child.bounds();
            assert!(
                cl - pl >= atlas::CONTAINER_PAD,
                "{parent:?} does not enclose {child:?}"
            );
            assert!(
                pr - cr >= atlas::CONTAINER_PAD,
                "{parent:?} does not enclose {child:?}"
            );
            assert!(
                ct - pt >= atlas::CONTAINER_PAD + atlas::CONTAINER_TITLE_HEIGHT,
                "{parent:?} does not reserve its title above {child:?}"
            );
            assert!(
                pb - cb >= atlas::CONTAINER_PAD,
                "{parent:?} does not enclose {child:?}"
            );
        }
    }

    /// A file that is not a drawing fails the whole call rather than landing
    /// half of itself and reporting success.
    #[test]
    fn importing_something_that_is_not_a_drawing_fails_loudly() {
        let dir = temp_dir("import-bad");
        let root = temp_dir("import-bad-repo");
        std::fs::write(root.join("notes.md"), "# not a drawing\n").expect("write");
        let (state, _ws) = state_rooted(&dir, &root, false);
        let defs = actions(&state);

        let error = call(&state, &defs, "atlas_import", json!({ "path": "notes.md" }))
            .expect_err("a markdown file is not an .excalidraw document");
        assert!(error.contains("not JSON"), "{error}");
        assert!(
            state.read().expect("read").shapes.is_empty(),
            "a failed import must not leave anything behind"
        );
    }

    /// The export route hands out a document Excalidraw would actually open.
    #[test]
    fn the_export_route_serves_a_real_excalidraw_document() {
        let dir = temp_dir("export");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        call(
            &state,
            &defs,
            "atlas_place",
            json!({ "label": "the runtime", "x": 40, "y": 40 }),
        )
        .expect("place");
        call(
            &state,
            &defs,
            "atlas_sketch",
            json!({ "shapes": [{ "form": "ellipse", "x": 10, "y": 10, "w": 300, "h": 200, "ink": "green" }] }),
        )
        .expect("sketch");

        let document = atlas::excalidraw::to_excalidraw(&state.read().expect("read"));
        let parsed: JsonValue = serde_json::from_str(&document).expect("valid JSON");
        assert_eq!(parsed["type"], "excalidraw");
        let kinds: Vec<&str> = parsed["elements"]
            .as_array()
            .expect("elements")
            .iter()
            .filter_map(|element| element["type"].as_str())
            .collect();
        // The card became a rectangle with its words bound to it, and the
        // drawn ellipse came along as an ellipse.
        assert!(kinds.contains(&"rectangle"), "{kinds:?}");
        assert!(kinds.contains(&"text"), "{kinds:?}");
        assert!(kinds.contains(&"ellipse"), "{kinds:?}");
    }

    /// The read-back's second job: not "what is on the page" but "what did
    /// they do while I was thinking", said in relations rather than positions.
    #[test]
    fn the_second_read_says_what_the_humans_edits_now_mean() {
        let dir = temp_dir("changes");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);

        place(&state, &defs, "the read path", 100.0, 100.0);
        let b = place(&state, &defs, "the write path", 100.0, 400.0);
        // A region well clear of both cards: decoration, for now.
        call(
            &state,
            &defs,
            "atlas_sketch",
            json!({ "shapes": [{ "form": "rect", "x": 2000.0, "y": 2000.0, "w": 100.0, "h": 100.0 }] }),
        )
        .expect("sketch");

        // First read: no delta, because there is nothing to have missed.
        let first = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(
            !first.contains("CHANGED SINCE"),
            "the first read of a session has nothing to diff against:\n{first}"
        );
        assert!(first.contains("touches no node"), "{first}");

        // Now the human drags that region over both cards and disputes one.
        let shape = state.read().expect("read").shapes[0].id.clone();
        {
            let mut scene = state.scene.lock();
            atlas::place_shape(
                &mut scene,
                &atlas::ShapePatch {
                    id: Some(shape.clone()),
                    x: Some(40.0),
                    y: Some(40.0),
                    w: Some(400.0),
                    h: Some(560.0),
                    ..Default::default()
                },
                &Author::Human,
            )
            .expect("move the region");
            atlas::place_node(
                &mut scene,
                &atlas::NodePatch {
                    id: Some(b.clone()),
                    status: Some("disputed".to_string()),
                    ..Default::default()
                },
                &Author::Human,
            )
            .expect("dispute");
        }

        let second = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        let section = second
            .split("CHANGED SINCE YOUR LAST READ")
            .nth(1)
            .unwrap_or_else(|| panic!("no change section in:\n{second}"))
            .to_string();

        // The change is reported as what it now MEANS…
        assert!(
            section.contains("now ENCLOSES") && section.contains("it was touching no node"),
            "the delta has to name the relation, before and after:\n{section}"
        );
        // …with the intent spelled out, not just the geometry…
        assert!(section.contains("grouping them"), "{section}");
        // …and the human's status change alongside it.
        assert!(
            section.contains("is now disputed") && section.contains("it was open"),
            "{section}"
        );
        assert!(section.contains("the read path"), "{section}");
        // The whole point: no coordinates for a shape that relates to nodes.
        let drawn_line = section
            .lines()
            .find(|line| line.contains("now ENCLOSES"))
            .expect("the shape's line")
            .to_string();
        assert!(
            !drawn_line.contains("it was touching no node \u{2014} at ("),
            "leaked the old coordinates onto a line that has relations to report: {drawn_line}"
        );

        // A third read with nothing in between says nothing at all.
        let third = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(
            !third.contains("CHANGED SINCE"),
            "an unchanged page must not manufacture a delta:\n{third}"
        );
    }

    /// The debug route must not eat the agent's delta.
    #[test]
    fn a_plain_describe_does_not_advance_the_read_mark() {
        let dir = temp_dir("describe-probe");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        place(&state, &defs, "first", 10.0, 10.0);
        call(&state, &defs, "atlas_read", json!({})).expect("read");

        // The human writes the second card. The change a probe must not eat
        // is by definition somebody else's: an agent is not told about its
        // own writes, so an agent-authored card would prove nothing here.
        {
            let mut scene = state.scene.lock();
            atlas::place_node(
                &mut scene,
                &atlas::NodePatch {
                    label: Some("second".to_string()),
                    x: Some(10.0),
                    y: Some(300.0),
                    ..Default::default()
                },
                &Author::Human,
            )
            .expect("the human places a card");
        }
        // Something polls the generic state read-back in between.
        SurfaceState::describe(state.as_ref()).expect("describe");

        let read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(
            read.contains("NEW node") && read.contains("second"),
            "an HTTP probe silently consumed the change the agent needed to see:\n{read}"
        );
    }

    #[test]
    fn atlas_constrain_creates_every_op_and_reports_the_document_state() {
        let dir = temp_dir("constrain-five-ops");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let plan = place(&state, &defs, "plan", 100.0, 100.0);
        let build = place(&state, &defs, "build", 100.0, 300.0);
        let verify = place(&state, &defs, "verify", 100.0, 500.0);

        let confirmation = call(
            &state,
            &defs,
            "atlas_constrain",
            json!({
                "constraints": [
                    { "op": "group", "members": ["plan", "build"], "salience": "fore" },
                    { "op": "sequence", "members": [plan, "build", "verify"], "axis": "y" },
                    { "op": "attaches", "members": ["build", verify] },
                    { "op": "voids", "members": ["plan"] },
                    { "op": "labels", "members": [build], "text": "implementation" }
                ]
            }),
        )
        .expect("all five constraints land")
        .expect("confirmation");

        let atlas = state.read().expect("read constraints");
        assert_eq!(atlas.constraints.len(), 5);
        for op in ["group", "sequence", "attaches", "voids", "labels"] {
            let constraint = atlas
                .constraints
                .iter()
                .find(|constraint| constraint.op == op)
                .unwrap_or_else(|| panic!("missing {op}"));
            assert!(
                confirmation.contains(&constraint.id),
                "confirmation omitted {op}: {confirmation}"
            );
        }
        let group = atlas
            .constraints
            .iter()
            .find(|constraint| constraint.op == "group")
            .expect("group");
        assert_eq!(group.members, vec![plan.clone(), build.clone()]);
        assert_eq!(group.salience, "fore");
        let sequence = atlas
            .constraints
            .iter()
            .find(|constraint| constraint.op == "sequence")
            .expect("sequence");
        assert_eq!(sequence.members, vec![plan, build.clone(), verify]);
        assert_eq!(sequence.axis.as_deref(), Some("y"));
        let labels = atlas
            .constraints
            .iter()
            .find(|constraint| constraint.op == "labels")
            .expect("labels");
        assert_eq!(labels.members, vec![build]);
        assert_eq!(labels.text.as_deref(), Some("implementation"));
        assert_eq!(atlas.constraint_state.status, "sat");
        assert!(
            confirmation.contains("constraint_state=SAT"),
            "{confirmation}"
        );
        assert!(confirmation.contains("sequence[y]"), "{confirmation}");
    }

    #[test]
    fn atlas_constrain_refuses_every_invalid_class_and_keeps_the_batch_atomic() {
        let dir = temp_dir("constrain-refusals");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let a = place(&state, &defs, "a", 100.0, 100.0);
        let b = place(&state, &defs, "b", 100.0, 300.0);
        let seed = call(
            &state,
            &defs,
            "atlas_constrain",
            json!({ "constraints": [{ "op": "group", "members": [a, b] }] }),
        )
        .expect("seed constraint")
        .expect("confirmation");
        let seed_id = state.read().expect("read seed").constraints[0].id.clone();
        assert!(seed.contains(&seed_id), "{seed}");
        let before = state.read().expect("before refusals").constraints.len();

        let too_long = "x".repeat(atlas::MAX_TEXT + 1);
        let cases = [
            (
                json!({ "constraints": [{ "op": "align", "members": ["a", "b"] }] }),
                "constraint op must be one of",
            ),
            (
                json!({ "constraints": [{ "op": "group", "members": ["a"] }] }),
                "needs at least two members",
            ),
            (
                json!({ "constraints": [{ "op": "group", "members": ["a", "a"] }] }),
                "cannot contain self or duplicate members",
            ),
            (
                json!({ "constraints": [{ "op": "group", "members": ["a", "missing"] }] }),
                "does not resolve to an atlas object",
            ),
            (
                json!({ "constraints": [{ "op": "group", "members": ["a", seed_id] }] }),
                "names another constraint",
            ),
            (
                json!({ "constraints": [{ "op": "sequence", "members": ["a", "b"], "axis": "z" }] }),
                "constraint axis must be one of",
            ),
            (
                json!({ "constraints": [{ "op": "voids", "members": ["a"], "salience": "middle" }] }),
                "constraint salience must be one of",
            ),
            (
                json!({ "constraints": [{ "op": "labels", "members": ["a"], "text": too_long }] }),
                "constraint text is longer than",
            ),
        ];
        for (args, reason) in cases {
            let error = call(&state, &defs, "atlas_constrain", args)
                .expect_err("invalid constraint must be refused");
            assert!(error.contains("constraints[0]"), "{error}");
            assert!(error.contains(reason), "expected {reason:?} in {error}");
            assert_eq!(
                state.read().expect("after refusal").constraints.len(),
                before,
                "a refusal stored a constraint: {error}"
            );
        }

        let error = call(
            &state,
            &defs,
            "atlas_constrain",
            json!({
                "constraints": [
                    { "op": "voids", "members": ["a"] },
                    { "op": "align", "members": ["a", "b"] }
                ]
            }),
        )
        .expect_err("one bad constraint refuses the batch");
        assert!(error.contains("constraints[1]"), "{error}");
        assert!(error.contains("align"), "{error}");
        assert_eq!(state.read().expect("after batch").constraints.len(), before);
    }

    #[test]
    fn atlas_constrain_refuses_an_ambiguous_title_instead_of_guessing() {
        let dir = temp_dir("constrain-ambiguous-title");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        place(&state, &defs, "duplicate", 100.0, 100.0);
        place(&state, &defs, "duplicate", 100.0, 300.0);
        place(&state, &defs, "other", 100.0, 500.0);

        let error = call(
            &state,
            &defs,
            "atlas_constrain",
            json!({ "constraints": [{ "op": "group", "members": ["duplicate", "other"] }] }),
        )
        .expect_err("ambiguous title must not resolve arbitrarily");
        assert!(error.contains("constraints[0]"), "{error}");
        assert!(error.contains("ambiguous"), "{error}");
        assert!(state.read().expect("read").constraints.is_empty());
    }

    #[test]
    fn atlas_unconstrain_names_the_original_author() {
        let dir = temp_dir("unconstrain-author");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        extension.note_caller(&named_agent("First agent"));
        let a = place(&state, extension.actions(), "a", 100.0, 100.0);
        let b = place(&state, extension.actions(), "b", 100.0, 300.0);
        call(
            &state,
            extension.actions(),
            "atlas_constrain",
            json!({ "constraints": [{ "op": "sequence", "members": [a, b] }] }),
        )
        .expect("create constraint");
        let id = state.read().expect("read").constraints[0].id.clone();

        extension.note_caller(&named_agent("Second agent"));
        let confirmation = call(
            &state,
            extension.actions(),
            "atlas_unconstrain",
            json!({ "ids": [id] }),
        )
        .expect("another author may remove the claim")
        .expect("confirmation");
        assert!(confirmation.contains("First agent"), "{confirmation}");
        assert!(confirmation.contains("sequence[x]"), "{confirmation}");
        assert!(state.read().expect("read").constraints.is_empty());
    }

    #[test]
    fn atlas_read_reports_constraint_create_break_and_removal_deltas() {
        let dir = temp_dir("constraint-read-deltas");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        let defs = extension.actions().to_vec();

        // Two bylines on purpose. A reader is not told about its own writes,
        // so acting and observing as one agent would assert that the surface
        // repeats work back to whoever just did it. The delta an agent needs
        // is somebody else's.
        let acts = |label: &str| extension.note_caller(&named_agent(label));
        acts("Author agent");
        let a = place(&state, &defs, "a", 100.0, 100.0);
        let b = place(&state, &defs, "b", 100.0, 300.0);

        acts("Observing agent");
        call(&state, &defs, "atlas_read", json!({})).expect("baseline read");

        acts("Author agent");
        call(
            &state,
            &defs,
            "atlas_constrain",
            json!({ "constraints": [{ "op": "sequence", "members": [a.clone(), b] }] }),
        )
        .expect("create constraint");
        let id = state.read().expect("read").constraints[0].id.clone();

        acts("Observing agent");
        let created = call(&state, &defs, "atlas_read", json!({}))
            .expect("read creation")
            .expect("text");
        assert!(created.contains("\nCONSTRAINTS\n"), "{created}");
        assert!(created.contains("NEW CONSTRAINT"), "{created}");
        assert!(created.contains(&id), "{created}");

        // A member disappearing breaks the relation. Nobody authored that
        // status change, which is why it reports to the declaring agent too.
        acts("Author agent");
        call(&state, &defs, "atlas_remove", json!({ "id": a })).expect("remove member");
        acts("Observing agent");
        let broken = call(&state, &defs, "atlas_read", json!({}))
            .expect("read broken transition")
            .expect("text");
        assert!(broken.contains("BROKEN"), "{broken}");
        assert!(broken.contains(&id), "{broken}");

        acts("Author agent");
        call(
            &state,
            &defs,
            "atlas_unconstrain",
            json!({ "ids": [id.clone()] }),
        )
        .expect("remove constraint");
        acts("Observing agent");
        let removed = call(&state, &defs, "atlas_read", json!({}))
            .expect("read removal")
            .expect("text");
        assert!(removed.contains("GONE CONSTRAINT"), "{removed}");
        assert!(removed.contains(&id), "{removed}");
    }

    #[test]
    fn a_human_mark_on_a_constraint_round_trips_into_read_back() {
        let dir = temp_dir("constraint-mark-round-trip");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let plan = place(&state, &defs, "plan", 100.0, 100.0);
        let build = place(&state, &defs, "build", 100.0, 300.0);
        call(
            &state,
            &defs,
            "atlas_constrain",
            json!({
                "constraints": [{
                    "op": "sequence",
                    "members": [plan.clone(), build.clone()],
                    "axis": "y"
                }]
            }),
        )
        .expect("create sequence");
        let constraint = state.read().expect("read").constraints[0].clone();

        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        atlas::mark(
            &mut browser,
            &constraint.id,
            "?",
            "does this ordering still hold?",
            &Author::Human,
        )
        .expect("human marks constraint");
        let payload = ag_ui_canvas::sync::update_message(
            &browser
                .encode_diff_v1(&state.scene.lock().state_vector_v1().expect("state vector"))
                .expect("diff"),
        );
        state
            .ws_receive(&encode_sync(&payload))
            .expect("host accepts human constraint mark");

        let read = call(&state, &defs, "atlas_read", json!({}))
            .expect("read mark")
            .expect("text");
        let literal = format!("{} sequence[y]: {} -> {}", constraint.id, plan, build);
        assert!(read.contains(&literal), "{read}");
        assert!(read.contains("? on"), "{read}");
        assert!(read.contains("does this ordering still hold?"), "{read}");
        assert!(read.contains("UNANSWERED"), "{read}");
    }

    #[test]
    fn a_constraint_alone_makes_the_atlas_non_empty() {
        let dir = temp_dir("constraint-is-empty");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let a = place(&state, &defs, "a", 100.0, 100.0);
        let b = place(&state, &defs, "b", 100.0, 300.0);
        let constraint = atlas::create_constraint(
            &mut state.scene.lock(),
            "group",
            &[a.clone(), b.clone()],
            None,
            None,
            None,
            &Author::Agent,
        )
        .expect("create constraint");
        atlas::remove(&state.scene.lock(), &a).expect("remove first member");
        atlas::remove(&state.scene.lock(), &b).expect("remove second member");

        let atlas = state.read().expect("read");
        assert!(atlas.nodes.is_empty());
        assert_eq!(atlas.constraints[0].id, constraint);
        assert!(!state.is_empty().expect("is_empty"));
    }

    #[test]
    fn a_human_mark_can_target_a_constraint() {
        let dir = temp_dir("human-mark-constraint");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let plan = place(&state, &defs, "plan", 100.0, 100.0);
        let build = place(&state, &defs, "build", 100.0, 300.0);
        let constraint = atlas::create_constraint(
            &mut state.scene.lock(),
            "sequence",
            &[plan, build],
            Some("y"),
            None,
            None,
            &Author::Agent,
        )
        .expect("agent creates sequence constraint");

        let mut browser =
            Scene::from_state(&state.scene.lock().encode_full().expect("encode")).expect("replica");
        let refusal = atlas::mark(
            &mut browser,
            &constraint,
            "?",
            "agent-authored mark",
            &Author::Agent,
        )
        .expect_err("agents still cannot create marks");
        assert_eq!(refusal, "only the human can create atlas marks");
        atlas::mark(
            &mut browser,
            &constraint,
            "?",
            "does this ordering still hold?",
            &Author::Human,
        )
        .expect("human mark lands on the constraint");
    }

    #[test]
    fn replica_schema_version_reaches_the_browser_and_mismatch_is_fail_closed() {
        let dir = temp_dir("replica-schema-version");
        let (state, _ws) = state(&dir);
        let snapshot = SurfaceState::snapshot(state.as_ref()).expect("snapshot");
        assert_eq!(
            snapshot.chrome,
            Some(json!({ "doc_schema_version": atlas::DOC_SCHEMA_VERSION })),
            "the host snapshot must carry the document schema version"
        );

        let web = include_str!("../web/src/lib.rs");
        assert!(
            web.contains("pub fn doc_schema_version() -> u32"),
            "the wasm replica does not export its compiled schema version"
        );
        let browser = include_str!("../static/extensions/atlas/index.js");
        for pinned in [
            "STATE_SNAPSHOT",
            "Host document schema",
            "WASM replica schema",
            "world.inert = true",
        ] {
            assert!(
                browser.contains(pinned),
                "missing {pinned:?} in browser boot"
            );
        }
    }

    #[test]
    fn explanation_actions_use_the_shared_core_and_report_the_same_cursor() {
        let dir = temp_dir("explanation-actions");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let inside = place(&state, &defs, "Inside", 120.0, 120.0);
        let boundary = place(&state, &defs, "Boundary", 480.0, 120.0);
        let flow = json!({
            "schema": "atlas-explanation-flow-v1",
            "id": "host-action-proof",
            "title": "A reusable explanation",
            "goal": "Prove that host actions and browser replicas share one finite cursor.",
            "start": "classify",
            "beats": [
                {
                    "id": "classify",
                    "title": "Classify the point",
                    "intent": "Combine the learner's utterance with the selected evidence.",
                    "cue": "A boundary point belongs to a closed epsilon neighborhood.",
                    "evidence": [{
                        "target_id": boundary,
                        "detail": "The boundary is included by the less-than-or-equal comparison."
                    }],
                    "actions": [{ "kind": "point", "target_id": boundary }],
                    "advance": {
                        "mode": "agent",
                        "prompt": "Inside or Beyond?",
                        "transitions": [{
                            "id": "inside",
                            "label": "Inside",
                            "next": "result",
                            "target_ids": [inside],
                            "phrases": ["inside", "yes"]
                        }]
                    }
                },
                {
                    "id": "result",
                    "title": "Read the result",
                    "intent": "Connect the decision to visible evidence.",
                    "cue": "Inside includes the boundary.",
                    "evidence": [{
                        "target_id": inside,
                        "detail": "The visible classification is Inside."
                    }],
                    "actions": [{ "kind": "reveal", "target_id": inside }],
                    "advance": { "mode": "terminal", "prompt": "", "transitions": [] }
                }
            ]
        });

        let defined = call(
            &state,
            &defs,
            "atlas_explanation_define",
            json!({ "flow": flow }),
        )
        .expect("define action")
        .expect("define confirmation");
        assert!(defined.contains("revision=1"), "{defined}");

        let advanced = call(
            &state,
            &defs,
            "atlas_explanation_advance",
            json!({
                "flow_id": "host-action-proof",
                "expected_revision": 1,
                "transition_id": "inside",
                "response": "yes",
                "selected_target_ids": [inside]
            }),
        )
        .expect("advance action")
        .expect("advance confirmation");
        assert!(advanced.contains("revision=2"), "{advanced}");
        assert!(advanced.contains("completed"), "{advanced}");

        let readback = call(&state, &defs, "atlas_read", json!({}))
            .expect("read action")
            .expect("readback");
        assert!(readback.contains("EXPLANATION FLOW"), "{readback}");
        assert!(
            readback.contains("[result] \"Read the result\""),
            "{readback}"
        );
        assert!(readback.contains("response=\"yes\""), "{readback}");
    }

    #[test]
    fn a_constraint_is_a_spatial_semantic_target() {
        let dir = temp_dir("constraint-semantic-target");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        let plan = place(&state, &defs, "plan", 100.0, 100.0);
        let build = place(&state, &defs, "build", 100.0, 300.0);
        call(
            &state,
            &defs,
            "atlas_constrain",
            json!({
                "constraints": [{
                    "op": "sequence",
                    "members": [plan, build],
                    "axis": "y"
                }]
            }),
        )
        .expect("create constraint");
        let constraint = state.read().expect("read").constraints[0].clone();
        let target_ref = SemanticTargetRef::new("atlas", constraint.id.clone());
        let target = SurfaceState::semantic_target(state.as_ref(), &target_ref)
            .expect("resolve target")
            .expect("constraint target");

        assert_eq!(target.target, target_ref);
        assert_eq!(target.label, constraint_literal(&constraint));
        assert!(target.description.contains(&constraint.created_by));
        assert!(target.spatial, "constraint glyph geometry is browser-owned");
    }

    fn place(state: &Arc<AtlasState>, defs: &[ToolDef], label: &str, x: f64, y: f64) -> String {
        call(
            state,
            defs,
            "atlas_place",
            json!({ "label": label, "x": x, "y": y }),
        )
        .expect("place");
        state
            .read()
            .expect("read")
            .nodes
            .iter()
            .find(|node| node.label == label)
            .expect("the node")
            .id
            .clone()
    }

    // ── B4: agent ink behind the flag ──────────────────────────────────

    #[test]
    fn agent_ink_flag_off_hides_the_tools_and_the_route() {
        let dir = temp_dir("agent-ink-flag-off");
        let (state, _ws) = state(&dir);
        let defs = actions(&state);
        assert!(
            defs.iter()
                .all(|def| !def.name.starts_with("atlas_variable")
                    && !def.name.starts_with("atlas_relation")
                    && def.name != "atlas_solve"
                    && def.name != "atlas_mode_set"
                    && def.name != "atlas_state_bind"
                    && !def.name.starts_with("atlas_step")),
            "flag off must not advertise any agent-ink tool: {:?}",
            defs.iter().map(|def| &def.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn agent_ink_flag_on_registers_the_eleven_agent_tools_and_the_human_only_promotion() {
        let dir = temp_dir("agent-ink-flag-on");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);
        for name in [
            "atlas_mode_set",
            "atlas_variable_create",
            "atlas_state_bind",
            "atlas_variable_set",
            "atlas_variable_capture",
            "atlas_relation_create",
            "atlas_solve",
            "atlas_step_record",
            "atlas_step_advance",
            "atlas_step_scrub",
            "atlas_variables_read",
        ] {
            assert!(
                defs.iter().any(|def| def.name == name),
                "expected {name} to be registered when AGUI_AGENT_INK is on"
            );
            assert_eq!(
                defs.iter().find(|def| def.name == name).unwrap().audience,
                ActionAudience::Agent,
                "{name} must be agent-only"
            );
        }
        // The one deliberate exception: promotion is a human act (see
        // `promote_agent_ink_to_alignment`'s doc comment), so it is the
        // single agent-ink tool this catalog registers `.human_only()`
        // rather than `.agent_only()`.
        assert_eq!(
            defs.iter()
                .find(|def| def.name == "atlas_mode_promote_to_alignment")
                .expect("atlas_mode_promote_to_alignment is registered")
                .audience,
            ActionAudience::Human,
            "promotion must be human-only, not agent-only like every other agent-ink tool"
        );
    }

    #[tokio::test]
    async fn agent_ink_variable_create_then_read_round_trips_through_the_tool_layer() {
        let dir = temp_dir("agent-ink-create-read");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);

        let created = call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({
                "name": "swing arm length",
                "value": 120.0,
                "state": "pinned",
                "unit": "mm"
            }),
        )
        .expect("create")
        .expect("text");
        assert!(created.contains("swing arm length"), "{created}");

        let read = call(&state, &defs, "atlas_variables_read", json!({}))
            .expect("read")
            .expect("text");
        assert!(read.contains("VARIABLES"), "{read}");
        assert!(read.contains("swing arm length"), "{read}");
        assert!(read.contains("120"), "{read}");
        assert!(
            read.contains("SOLVE NOT RUN"),
            "a variable that has never been solved must say so plainly: {read}"
        );

        let extension = AtlasExtension::new(state.clone());
        let route = extension
            .routes()
            .into_iter()
            .find(|route| route.method == HttpMethod::Get && route.path == "/atlas/agent-ink")
            .expect("agent-ink route");
        let route_response = (route.handler)(RouteRequest {
            query: std::collections::HashMap::new(),
            body: None,
        })
        .await;
        assert_eq!(route_response.status, 200, "{}", route_response.body);
        let body = route_response.body.as_json().expect("route body is JSON");
        assert_eq!(body["enabled"], JsonValue::Bool(true));
        assert!(body["readback"]
            .as_str()
            .expect("readback is a string")
            .contains("swing arm length"));
        assert_eq!(
            body["variables"].as_array().expect("variables array").len(),
            1
        );
    }

    /// The full five-tool sequence, from a fresh document to a scrub that
    /// returns an earlier value: create the "step" variable, atlas_solve
    /// (record_current_step refuses to persist before the document has a
    /// solve state, so this must come first, not after — the opposite order
    /// from a literal reading of "record then solve"), atlas_step_record to
    /// seed step 0, atlas_step_advance to move forward, then
    /// atlas_step_scrub back.
    ///
    /// The fixture is the same shape core's own step.rs tests use: a
    /// scrubbing "step" index and a free "position" tied to it by an
    /// "equal" relation, so position tracks step exactly and a scrub back
    /// to an earlier step has an unambiguous earlier value to check against.
    #[test]
    fn agent_ink_full_sequence_from_a_fresh_document_through_a_scrub() {
        let dir = temp_dir("agent-ink-full-sequence");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "step", "value": 0.0, "state": "scrubbing" }),
        )
        .expect("create step")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "position", "value": 0.0, "state": "free" }),
        )
        .expect("create position")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_relation_create",
            json!({
                "name": "position follows step",
                "op": "equal",
                "members": ["position", "step"]
            }),
        )
        .expect("create relation")
        .expect("text");

        call(&state, &defs, "atlas_solve", json!({}))
            .expect("solve step 0")
            .expect("text");
        let recorded0 = call(&state, &defs, "atlas_step_record", json!({}))
            .expect("record step 0")
            .expect("text");
        assert!(recorded0.contains("recorded step 0"), "{recorded0}");
        assert_close(
            variable_value(&state, "position"),
            0.0,
            "the equal relation ties position to step at record time",
        );

        // The solver's `equal` residual converges within `residual_tolerance`
        // (1e-12) rather than landing on the exact float `step` holds, so
        // "position tracks step" is checked with a loose tolerance; only the
        // scrub-back comparison below needs (and gets) bit-for-bit equality,
        // since that path never re-solves.
        let mut position_at_2 = None;
        for expected in 1..=5 {
            let advanced = call(&state, &defs, "atlas_step_advance", json!({}))
                .expect("advance")
                .expect("text");
            assert!(
                advanced.contains(&format!("advanced to step {expected}")),
                "{advanced}"
            );
            if expected == 2 {
                position_at_2 = Some(variable_value(&state, "position"));
            }
        }
        let position_at_2 = position_at_2.expect("visited step 2 on the way to step 5");
        assert_eq!(
            variable_value(&state, "step"),
            5.0,
            "five advances land on step 5"
        );
        assert_close(
            variable_value(&state, "position"),
            5.0,
            "position followed step all the way to 5",
        );

        let scrubbed = call(&state, &defs, "atlas_step_scrub", json!({ "index": 2 }))
            .expect("scrub")
            .expect("text");
        assert!(scrubbed.contains("restored step 2"), "{scrubbed}");
        assert_eq!(
            variable_value(&state, "step"),
            2.0,
            "scrub restores the step index to its earlier value"
        );
        assert_eq!(
            variable_value(&state, "position").to_bits(),
            position_at_2.to_bits(),
            "scrub restores the exact bits recorded when step 2 was first visited, \
             not merely a value close to it"
        );

        // What this test does NOT and cannot prove: that atlas_step_scrub
        // did not re-invoke the solver. Core proves exactly that in its own
        // suite with a counter (SOLVE_INVOCATIONS / solve_invocations(),
        // core/src/agent_ink/step.rs:817-825) — but it is a private,
        // `#[cfg(test)]`-gated thread-local inside `same_page_atlas_core`.
        // It is not `pub`, and `#[cfg(test)]` items in a path dependency are
        // not compiled into a downstream crate's test binary at all (they
        // only exist when core itself is built as the crate under test), so
        // there is no way to reach it from this file or this crate.
        //
        // A value-only substitute is not possible either, and not just as a
        // missing convenience: `restore_snapshot` writes every recorded
        // variable's value directly, so by the time any (hypothetical)
        // redundant solve ran afterward, "position" would already equal
        // "step" exactly — an already-zero-residual system is a fixed point,
        // so a solve started from it is idempotent and produces bit-identical
        // output whether or not it actually ran. No sequence of calls through
        // this tool surface can put the live document into a state where a
        // real solve and a real restore would visibly disagree, so the
        // assertions above (which do pass) cannot distinguish the two paths.
        //
        // The real guarantee here is structural, from reading the settled
        // source rather than from a counted assertion: `scrub_step`'s body
        // (core/src/agent_ink/step.rs) calls only `stored_step`,
        // `ensure_snapshot_index_variable`, and `restore_snapshot` — there is
        // no call into `solve`, `solve_with_options`, or `invoke_solve`
        // anywhere in that function or the functions it calls.
    }

    #[test]
    fn agent_ink_timeline_uses_the_unique_scrubbing_variable_not_a_magic_name() {
        let dir = temp_dir("agent-ink-semantic-step-index");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "descent step", "value": 0.0, "state": "scrubbing" }),
        )
        .expect("create semantic step index")
        .expect("text");
        call(&state, &defs, "atlas_solve", json!({}))
            .expect("solve step 0")
            .expect("text");

        let recorded = call(&state, &defs, "atlas_step_record", json!({}))
            .expect("record through unique scrubbing variable")
            .expect("text");
        assert!(recorded.contains("recorded step 0"), "{recorded}");

        let advanced = call(&state, &defs, "atlas_step_advance", json!({}))
            .expect("advance through unique scrubbing variable")
            .expect("text");
        assert!(advanced.contains("advanced to step 1"), "{advanced}");
        assert_eq!(variable_value(&state, "descent step"), 1.0);

        call(&state, &defs, "atlas_step_scrub", json!({ "index": 0 }))
            .expect("scrub through unique scrubbing variable")
            .expect("text");
        assert_eq!(variable_value(&state, "descent step"), 0.0);

        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "competing index", "value": 0.0, "state": "scrubbing" }),
        )
        .expect("create competing scrubbing variable")
        .expect("text");
        let error = call(&state, &defs, "atlas_step_record", json!({}))
            .expect_err("two semantic indices must fail closed");
        assert!(error.contains("timeline index is ambiguous"), "{error}");
    }

    /// `atlas_step_export_citable` and `/atlas/agent-ink/citable` through
    /// the real tool and route layers, not just `citable_step` in core:
    /// alignment mode, one recorded step, a citable export that carries the
    /// mode/values/unit/solve status/author, and a refusal for a
    /// learning-mode document.
    #[tokio::test]
    async fn agent_ink_citable_step_exports_through_the_tool_and_route_layers() {
        let dir = temp_dir("agent-ink-citable-export");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_mode_set",
            json!({ "mode": "alignment" }),
        )
        .expect("set alignment mode")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "step", "value": 0.0, "state": "scrubbing" }),
        )
        .expect("create step index")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "width", "value": 12.0, "state": "pinned", "unit": "px" }),
        )
        .expect("create pinned width")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "height", "value": 0.0, "state": "free", "unit": "px" }),
        )
        .expect("create free height")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_relation_create",
            json!({
                "name": "width equals height",
                "op": "equal",
                "members": ["width", "height"]
            }),
        )
        .expect("create relation")
        .expect("text");
        call(&state, &defs, "atlas_solve", json!({}))
            .expect("solve")
            .expect("text");
        call(&state, &defs, "atlas_step_record", json!({}))
            .expect("record step 0")
            .expect("text");

        let exported = call(
            &state,
            &defs,
            "atlas_step_export_citable",
            json!({ "index": 0 }),
        )
        .expect("export the recorded step")
        .expect("text");
        let document: JsonValue = serde_json::from_str(&exported).expect("citable export is JSON");
        assert_eq!(document["kind"], "same-page-atlas-agent-ink-step-citable");
        assert_eq!(document["step"]["mode"], "alignment");
        assert_eq!(document["step"]["index"], 0);
        assert_eq!(document["step"]["status"], "converged");
        // The tool call carries no authorship of its own, so this landed
        // through the same generic-agent byline every unnamed agent write
        // always has — the point under test is that this is the object's
        // real recorded author, not that it says something more specific.
        assert_eq!(document["step"]["recorded_by"], "agent");
        let width = document["step"]["variables"]
            .as_array()
            .expect("variables array")
            .iter()
            .find(|variable| variable["name"] == "width")
            .expect("width in the export");
        assert_eq!(width["value"], 12.0);
        assert_eq!(width["unit"], "px");
        assert_eq!(width["state"], "pinned");
        let citable_id = document["step"]["citable_id"]
            .as_str()
            .expect("citable_id is a string")
            .to_string();
        assert_eq!(document["source"]["sha256"], citable_id);

        // The route hands back the identical document, reached without MCP.
        let extension = AtlasExtension::new(state.clone());
        let route = extension
            .routes()
            .into_iter()
            .find(|route| {
                route.method == HttpMethod::Get && route.path == "/atlas/agent-ink/citable"
            })
            .expect("citable route");
        let mut query = std::collections::HashMap::new();
        query.insert("index".to_string(), "0".to_string());
        let response = (route.handler)(RouteRequest { query, body: None }).await;
        assert_eq!(response.status, 200, "{}", response.body);
        let body = response.body.as_json().expect("route body is JSON");
        assert_eq!(body["ok"], true);
        assert_eq!(body["citable"]["source"]["sha256"], citable_id);

        // A step recorded in learning mode refuses the same export with a
        // real reason, through the tool layer — not a filtered-out empty
        // result.
        let (learning_state, _ws) = state_with_agent_ink(&temp_dir("agent-ink-citable-learning"));
        let learning_defs = actions(&learning_state);
        call(
            &learning_state,
            &learning_defs,
            "atlas_variable_create",
            json!({ "name": "step", "value": 0.0, "state": "scrubbing" }),
        )
        .expect("create step index")
        .expect("text");
        call(
            &learning_state,
            &learning_defs,
            "atlas_variable_create",
            json!({ "name": "position", "value": 0.0, "state": "free" }),
        )
        .expect("create position")
        .expect("text");
        call(
            &learning_state,
            &learning_defs,
            "atlas_relation_create",
            json!({
                "name": "position follows step",
                "op": "equal",
                "members": ["position", "step"]
            }),
        )
        .expect("create relation")
        .expect("text");
        call(&learning_state, &learning_defs, "atlas_solve", json!({}))
            .expect("solve")
            .expect("text");
        call(
            &learning_state,
            &learning_defs,
            "atlas_step_record",
            json!({}),
        )
        .expect("record step 0")
        .expect("text");
        let refused = call(
            &learning_state,
            &learning_defs,
            "atlas_step_export_citable",
            json!({ "index": 0 }),
        )
        .expect_err("a learning-mode step must refuse citable export");
        assert!(
            refused.contains("learning mode") && refused.contains("promote_to_alignment"),
            "{refused}"
        );
    }

    /// A minimal [`ag_ui_surface::Surface`] wrapping exactly one extension's
    /// tools, so a test can drive [`ag_ui_surface::turn_loop::dispatch_tool`]
    /// for real — the same function every transport in this codebase
    /// (`/mcp`, `POST /surface/action`, the OpenAI adapter) calls, and the
    /// only place `Caller::may_call` is checked. `call()` above cannot prove
    /// an audience refusal: it invokes a `ToolDef`'s `apply` closure
    /// directly, bypassing the authorization boundary entirely.
    struct DispatchSurface {
        state: Arc<AtlasState>,
        tools: Vec<ToolDef>,
    }

    impl ag_ui_surface::Surface for DispatchSurface {
        fn state(&self) -> &dyn SurfaceState {
            self.state.as_ref()
        }

        fn tools(&self) -> &[ToolDef] {
            &self.tools
        }

        fn client_modules(&self) -> Vec<ClientModule> {
            Vec::new()
        }
    }

    /// Call `name` through the real dispatch path — [`ag_ui_surface::turn_loop::dispatch_tool`] —
    /// as `caller`, exactly the boundary MCP (always [`ag_ui_surface::Caller::Agent`],
    /// see `crates/ag-ui-surface/src/mcp.rs`) and a human's `POST
    /// /surface/action` reach. Builds the minimal `RuntimeState` dispatch
    /// needs, following the same construction
    /// `crates/ag-ui-surface/src/lib.rs`'s own
    /// `failed_provider_stays_down_until_an_explicit_retry` test uses.
    async fn dispatch(
        state: &Arc<AtlasState>,
        defs: &[ToolDef],
        caller: ag_ui_surface::Caller,
        name: &str,
        args: JsonValue,
    ) -> (String, bool, bool) {
        let surface = DispatchSurface {
            state: state.clone(),
            tools: defs.to_vec(),
        };
        let (ws_tx, _) = broadcast::channel(8);
        let (sse_tx, _) = broadcast::channel(8);
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let auth_path = temp_dir("agent-ink-dispatch-auth").join("auth.json");
        let auth = Arc::new(
            ag_ui_surface::auth::AuthStore::open(auth_path).expect("open a scratch auth store"),
        );
        let (rt, _channels) = ag_ui_surface::runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history,
            Vec::new(),
            auth,
            "none".to_string(),
            ag_ui_surface::turn_loop::openai::ByokConfig::default(),
            false,
            0,
        );
        rt.install_action_schemas(surface.tools())
            .expect("install this catalog's schemas");
        ag_ui_surface::turn_loop::dispatch_tool(&rt, &surface, caller, name, &args).await
    }

    /// Requirement: promotion is human-only, enforced where every other
    /// audience split in this codebase is enforced — `Caller::may_call`
    /// inside `turn_loop::dispatch_tool` — not by convention in the tool's
    /// description text. An agent (the caller MCP always establishes) must
    /// be refused before its arguments are even inspected; a human must be
    /// let through and actually promote the document.
    #[tokio::test]
    async fn atlas_mode_promote_to_alignment_is_refused_to_an_agent_and_allowed_to_a_human() {
        let dir = temp_dir("agent-ink-promote-dispatch");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "step", "value": 0.0, "state": "scrubbing" }),
        )
        .expect("create step index")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "width", "value": 12.0, "state": "pinned", "unit": "px" }),
        )
        .expect("create pinned width")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "height", "value": 0.0, "state": "free", "unit": "px" }),
        )
        .expect("create free height")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_relation_create",
            json!({
                "name": "width equals height",
                "op": "equal",
                "members": ["width", "height"]
            }),
        )
        .expect("create relation")
        .expect("text");
        call(&state, &defs, "atlas_solve", json!({}))
            .expect("solve")
            .expect("text");
        call(&state, &defs, "atlas_step_record", json!({}))
            .expect("record step 0")
            .expect("text");

        let (agent_result, _, agent_ok) = dispatch(
            &state,
            &defs,
            ag_ui_surface::Caller::Agent,
            "atlas_mode_promote_to_alignment",
            json!({}),
        )
        .await;
        assert!(
            !agent_ok,
            "an agent must not be able to promote: {agent_result}"
        );
        assert!(
            agent_result.contains("not available to this agent"),
            "{agent_result}"
        );
        // Refused before dispatch even runs the tool: the document never
        // called atlas_mode_set, so its mode is still unset (`None`, which
        // reads as learning) with no promotion record.
        let still_learning = state.read().expect("read after the refused agent call");
        assert_eq!(still_learning.agent_ink_mode, None);
        assert!(still_learning.agent_ink_promotion.is_none());

        let (human_result, _, human_ok) = dispatch(
            &state,
            &defs,
            ag_ui_surface::Caller::Human,
            "atlas_mode_promote_to_alignment",
            json!({}),
        )
        .await;
        assert!(human_ok, "a human must be able to promote: {human_result}");
        assert!(
            human_result.contains("promoted the agent-ink timeline"),
            "{human_result}"
        );

        let promoted = state.read().expect("read after the human's promotion");
        assert_eq!(
            promoted.agent_ink_mode,
            Some(atlas::agent_ink::AgentInkMode::Alignment)
        );
        let promotion = promoted
            .agent_ink_promotion
            .as_ref()
            .expect("a promotion record exists");
        assert_eq!(promotion.promoted_by, "human");
        eprintln!(
            "PROMOTE_DISPATCH agent_refused={:?} human_allowed={:?}",
            agent_result, human_result
        );
    }

    /// `atlas_relation_create`'s `expression` argument reaches the same
    /// evaluator core's own tests exercise directly; this only checks that
    /// the tool layer actually plumbs it through and that the read-back
    /// shows the formula, not that the math is right (core covers that).
    #[test]
    fn atlas_relation_create_exposes_the_formula_expression_argument() {
        let dir = temp_dir("agent-ink-formula-tool");
        let (state, _ws) = state_with_agent_ink(&dir);
        let defs = actions(&state);

        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "x", "value": 0.0, "state": "pinned" }),
        )
        .expect("create x")
        .expect("text");
        call(
            &state,
            &defs,
            "atlas_variable_create",
            json!({ "name": "decay", "value": 1.0, "state": "free" }),
        )
        .expect("create decay")
        .expect("text");
        let created = call(
            &state,
            &defs,
            "atlas_relation_create",
            json!({
                "name": "decay follows x",
                "op": "formula",
                "members": ["decay", "x"],
                "expression": "exp(-2*x)"
            }),
        )
        .expect("create formula relation")
        .expect("text");
        assert!(created.contains("exp(-2*x)"), "{created}");

        call(&state, &defs, "atlas_solve", json!({}))
            .expect("solve formula relation")
            .expect("text");
        assert_close(variable_value(&state, "decay"), 1.0, "exp(-2*0) is 1");

        let refused = call(
            &state,
            &defs,
            "atlas_relation_create",
            json!({
                "name": "bad formula",
                "op": "formula",
                "members": ["decay", "x"],
                "expression": "exp(-2*z)"
            }),
        )
        .expect_err("a formula referencing an undeclared variable must be refused");
        assert!(refused.contains('z'), "{refused}");
    }

    fn variable_value(state: &Arc<AtlasState>, name: &str) -> f64 {
        state
            .read()
            .expect("read")
            .variables
            .iter()
            .find(|variable| variable.name == name)
            .unwrap_or_else(|| panic!("variable {name:?} not found"))
            .value
    }

    /// The solver converges within `residual_tolerance`, not to the exact
    /// bits of the target float, so comparisons against a solved value use
    /// this instead of `assert_eq!`.
    fn assert_close(actual: f64, expected: f64, context: &str) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{context}: expected approximately {expected}, got {actual}"
        );
    }

    #[tokio::test]
    async fn agent_ink_route_reports_disabled_when_the_flag_is_off() {
        let dir = temp_dir("agent-ink-route-off");
        let (state, _ws) = state(&dir);
        let extension = AtlasExtension::new(state.clone());
        let route = extension
            .routes()
            .into_iter()
            .find(|route| route.method == HttpMethod::Get && route.path == "/atlas/agent-ink")
            .expect("agent-ink route");
        let response = (route.handler)(RouteRequest {
            query: std::collections::HashMap::new(),
            body: None,
        })
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let body = response.body.as_json().expect("route body is JSON");
        assert_eq!(body, &json!({ "enabled": false }));
    }
}
