use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use crate::host::review::ReviewHost;
use ag_ui_surface::{
    current_actor, ActionAudience, ActionDef, Actor as SurfaceActor, ArtifactFile,
    CapabilityArtifact, ClientModule, DocumentMount, Effect as SurfaceEffect, StateBacking,
    StateSnapshot, Surface, SurfaceState, Transport, WsInbound, WsRefusal,
};
use samepage_capability::{Capability, Composition, Effect as CapabilityEffect};
use samepage_diagrams::Diagrams;
use samepage_doc::host::{Commit, Host};
use samepage_doc::{Actor, Caller};
use samepage_map::Map;
use samepage_review::Review;
use samepage_sketch::Sketch;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildManifest {
    schema_version: u32,
    build_owner: String,
    artifacts: Vec<BuildArtifact>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildArtifact {
    id: String,
    kind: String,
    abi: String,
    glue: FileIdentity,
    wasm: FileIdentity,
    entry: Option<FileIdentity>,
    descriptor: Option<Value>,
    layout_version: Option<u32>,
}

#[derive(Deserialize)]
struct FileIdentity {
    url: String,
    sha256: String,
    bytes: u64,
}

impl FileIdentity {
    fn verify(&self, root: &Path) -> Result<ArtifactFile, String> {
        let relative = self
            .url
            .strip_prefix("/capabilities/")
            .ok_or_else(|| format!("artifact URL outside /capabilities: {}", self.url))?;
        if relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(format!("invalid artifact URL: {}", self.url));
        }
        let data = std::fs::read(root.join(relative))
            .map_err(|error| format!("cannot read {}: {error}", self.url))?;
        let observed = format!("{:x}", Sha256::digest(&data));
        if data.len() as u64 != self.bytes || observed != self.sha256 {
            return Err(format!("artifact identity mismatch: {}", self.url));
        }
        Ok(ArtifactFile {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        })
    }
}

/// Only the real document and compiled Map capability may enter C4's page.
pub struct SelectedArtifacts {
    document: DocumentMount,
    map: CapabilityArtifact,
    diagrams: Option<CapabilityArtifact>,
    review: Option<CapabilityArtifact>,
    architecture: Option<CapabilityArtifact>,
    board: Option<CapabilityArtifact>,
    sketch: Option<CapabilityArtifact>,
}

impl SelectedArtifacts {
    pub fn open(root: &Path) -> Result<Self, String> {
        let path = root.join("capabilities.json");
        let data = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let manifest: BuildManifest = serde_json::from_slice(&data)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        if manifest.schema_version != 1 || manifest.build_owner != "samepage-capabilities.v1" {
            return Err("unsupported selected capability manifest".to_string());
        }
        if !(2..=5).contains(&manifest.artifacts.len()) {
            return Err("requires doc, map and optional diagrams/review/architecture/board/sketch artifacts".to_string());
        }
        let mut document = None;
        let mut map = None;
        let mut diagrams = None;
        let mut review = None;
        let mut architecture = None;
        let mut board = None;
        let mut sketch = None;
        let mut seen = std::collections::BTreeSet::new();
        for artifact in manifest.artifacts {
            if !seen.insert(artifact.id.clone()) {
                return Err(format!("duplicate artifact {}", artifact.id));
            }
            match artifact.id.as_str() {
                "doc"
                    if artifact.kind == "document"
                        && artifact.abi == "samepage.document.v1"
                        && artifact.layout_version == Some(1)
                        && artifact.entry.is_none()
                        && artifact.descriptor.is_none() =>
                {
                    document = Some(DocumentMount {
                        pkg_name: "samepage_doc_web".to_string(),
                        js: artifact.glue.url.clone(),
                        js_sha256: artifact.glue.verify(root)?.sha256,
                        js_bytes: artifact.glue.bytes,
                        wasm: artifact.wasm.url.clone(),
                        sha256: artifact.wasm.verify(root)?.sha256,
                        bytes: artifact.wasm.bytes,
                        abi: artifact.abi,
                        layout_version: 1,
                    });
                }
                "map" | "diagrams" | "review" | "architecture" | "board" | "sketch"
                    if artifact.kind == "capability"
                        && artifact.abi == "samepage.capability.v2" =>
                {
                    let compiled = serde_json::to_value(match artifact.id.as_str() {
                        "map" => samepage_map::descriptor(),
                        "review" => samepage_review::descriptor(),
                        "architecture" => samepage_architecture::descriptor(),
                        "board" => samepage_board::descriptor(),
                        "sketch" => samepage_sketch::descriptor(),
                        _ => samepage_diagrams::descriptor(),
                    })
                    .map_err(|error| error.to_string())?;
                    if artifact.descriptor.as_ref() != Some(&compiled) {
                        return Err(
                            "emitted descriptor differs from compiled native capability".into()
                        );
                    }
                    let entry = artifact
                        .entry
                        .ok_or("capability entry is missing")?
                        .verify(root)?;
                    let capability = CapabilityArtifact {
                        kind: artifact.kind,
                        abi: artifact.abi,
                        entry,
                        glue: artifact.glue.verify(root)?,
                        wasm: artifact.wasm.verify(root)?,
                        descriptor: compiled,
                    };
                    match artifact.id.as_str() {
                        "map" => map = Some(capability),
                        "review" => review = Some(capability),
                        "architecture" => architecture = Some(capability),
                        "board" => board = Some(capability),
                        "sketch" => sketch = Some(capability),
                        _ => diagrams = Some(capability),
                    }
                }
                _ => return Err(format!("unsupported C4 artifact {}", artifact.id)),
            }
        }
        Ok(Self {
            document: document.ok_or("document artifact is missing")?,
            map: map.ok_or("map artifact is missing")?,
            diagrams,
            review,
            architecture,
            board,
            sketch,
        })
    }
}

struct SaveProgress {
    completed: u64,
    fault: Option<String>,
    slow_target: Option<u64>,
}

type HostSaver = Arc<dyn Fn(&Host) -> Result<(), String> + Send + Sync>;
/// Persistence status delivery is synchronous and may run while the progress
/// mutex is held. Implementations must not reenter `SamePageState`.
type EventEmitter = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct SamePageState {
    pub(crate) host: Arc<Host>,
    review: Option<Arc<ReviewHost>>,
    pub(crate) validation: Arc<crate::host::page_validation::PageValidation>,
    pub(crate) input: Arc<crate::host::board::InputSignals>,
    board: Mutex<Option<Arc<crate::host::board::BoardHost>>>,
    save_progress: Arc<(Mutex<SaveProgress>, Condvar)>,
    refusal_tx: broadcast::Sender<WsRefusal>,
    /// A Host commit is live before its observer returns. Keep every external
    /// read behind the committing operation until that commit is durable.
    operation: Mutex<()>,
    save_wait: Duration,
    emit_event: EventEmitter,
}

impl SamePageState {
    fn open(path: PathBuf, transport: Transport) -> Result<Arc<Self>, String> {
        let ws_tx = transport.ws_tx.clone();
        let emit_transport = transport.clone();
        Self::open_with_io(
            path,
            ws_tx,
            Arc::new(|host| host.save().map(|_| ()).map_err(|error| error.to_string())),
            Arc::new(move |name, value| emit_transport.emit(name, value)),
            Duration::from_secs(15),
        )
    }

    fn open_with_io(
        path: PathBuf,
        ws_tx: broadcast::Sender<Vec<u8>>,
        saver: HostSaver,
        emit_event: EventEmitter,
        save_wait: Duration,
    ) -> Result<Arc<Self>, String> {
        Self::open_selected(
            path, ws_tx, saver, emit_event, save_wait, None, false, false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open_selected(
        path: PathBuf,
        ws_tx: broadcast::Sender<Vec<u8>>,
        saver: HostSaver,
        emit_event: EventEmitter,
        save_wait: Duration,
        project: Option<&Path>,
        with_diagrams: bool,
        with_review: bool,
    ) -> Result<Arc<Self>, String> {
        Self::open_selected_capabilities(
            path, ws_tx, saver, emit_event, save_wait, project, with_diagrams, with_review, false, false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn open_selected_architecture(
        path: PathBuf, ws_tx: broadcast::Sender<Vec<u8>>, saver: HostSaver, emit_event: EventEmitter,
        save_wait: Duration, project: Option<&Path>, with_diagrams: bool, with_review: bool, with_architecture: bool,
    ) -> Result<Arc<Self>, String> {
        Self::open_selected_capabilities(
            path, ws_tx, saver, emit_event, save_wait, project, with_diagrams, with_review, with_architecture, false,
        )
    }

    /// One opener for every optional capability; the named wrappers above keep
    /// the per-lane call sites and tests unchanged.
    #[allow(clippy::too_many_arguments)]
    fn open_selected_capabilities(
        path: PathBuf,
        ws_tx: broadcast::Sender<Vec<u8>>,
        saver: HostSaver,
        emit_event: EventEmitter,
        save_wait: Duration,
        project: Option<&Path>,
        with_diagrams: bool,
        with_review: bool,
        with_architecture: bool,
        with_sketch: bool,
    ) -> Result<Arc<Self>, String> {
        let mut descriptors = vec![samepage_map::descriptor()];
        let mut capabilities: Vec<Arc<dyn Capability>> = vec![Arc::new(Map)];
        if with_diagrams {
            descriptors.push(samepage_diagrams::descriptor());
            capabilities.push(Arc::new(Diagrams));
        }
        let review = if with_review {
            let root = project.ok_or("review needs a project root")?;
            descriptors.push(samepage_review::descriptor());
            capabilities.push(Arc::new(Review));
            Some(Arc::new(ReviewHost::open(root)?))
        } else {
            None
        };
        if with_architecture {
            descriptors.push(samepage_architecture::descriptor());
            capabilities.push(Arc::new(samepage_architecture::Architecture));
        }
        if with_sketch {
            descriptors.push(samepage_sketch::descriptor());
            capabilities.push(Arc::new(Sketch));
        }
        let composition = Composition::new(descriptors).map_err(|error| error.to_string())?;
        let mut host = Host::open(path, composition, capabilities).map_err(|e| e.to_string())?;
        if with_diagrams {
            let project = project.ok_or("diagrams needs a project root")?;
            host = host
                .with_verifier(
                    "diagrams",
                    Arc::new(crate::diagram_sources::DiagramSources::open(project)?),
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(review) = &review {
            host = host
                .with_verifier("review", review.clone())
                .map_err(|e| e.to_string())?;
        }
        if !with_review || !with_sketch {
            host = host.with_verifier("map", Arc::new(crate::host::sketch::MissingOwners { review: with_review, sketch: with_sketch }))
                .map_err(|e| e.to_string())?;
        }
        if with_sketch {
            host = host.with_composer("sketch", Arc::new(crate::host::sketch::SketchHost { review: with_review }))
                .map_err(|e| e.to_string())?;
        }
        if with_sketch {
            host = host.with_plan_context("map", &["shape"]).map_err(|e| e.to_string())?;
            if with_review { host = host.with_plan_context("review", &["shape"]).map_err(|e| e.to_string())?; }
        }
        let host = Arc::new(host);
        let (commit_tx, commit_rx) = mpsc::channel::<Commit>();
        let (refusal_tx, _) = broadcast::channel(128);
        let save_progress = Arc::new((
            Mutex::new(SaveProgress {
                completed: host.journal().seq,
                fault: None,
                slow_target: None,
            }),
            Condvar::new(),
        ));
        let input = Arc::new(crate::host::board::InputSignals::default());
        let state = Arc::new(Self {
            host: host.clone(),
            review,
            validation: Arc::default(),
            input: input.clone(),
            board: Mutex::new(None),
            save_progress: save_progress.clone(),
            refusal_tx: refusal_tx.clone(),
            operation: Mutex::new(()),
            save_wait,
            emit_event: emit_event.clone(),
        });
        let callback_progress = save_progress.clone();
        let callback_emit = emit_event.clone();
        host.on_commit(move |commit| {
            input.changed.notify_waiters();
            if commit_tx.send(commit.clone()).is_err() {
                let message = "commit persistence worker stopped".to_string();
                let mut emit = |message| {
                    callback_emit("samepage.persistence-error", json!({"error":message}));
                };
                set_fault(&callback_progress, message, &mut emit);
            }
        });
        host.on_refusal(move |actor, refusal| {
            let _ = refusal_tx.send(WsRefusal {
                participant_id: actor.participant.clone(),
                reason: format!("samepage.refused: {refusal}"),
            });
        });
        let weak_host: Weak<Host> = Arc::downgrade(&host);
        let expected = host.journal().seq.saturating_add(1);
        std::thread::Builder::new()
            .name("samepage-persistence".into())
            .spawn(move || {
                run_persistence_worker(
                    commit_rx,
                    expected,
                    save_progress,
                    move |commit| {
                        let host = weak_host
                            .upgrade()
                            .ok_or_else(|| "document host stopped".to_string())?;
                        saver(&host).map_err(|error| {
                            format!("document save failed after commit {}: {error}", commit.seq)
                        })
                    },
                    move |commit| {
                        let _ = ws_tx.send(commit.update.clone());
                    },
                    {
                        let emit_event = emit_event.clone();
                        move |message| {
                            emit_event("samepage.persistence-error", json!({"error":message}));
                        }
                    },
                    move |seq| {
                        emit_event("samepage.persistence-resolved", json!({"seq":seq}));
                    },
                );
            })
            .map_err(|error| format!("cannot start persistence worker: {error}"))?;
        Ok(state)
    }

    async fn validation_report_async(self: Arc<Self>) -> Result<Value, String> {
        tokio::task::spawn_blocking(move || {
            let _operation = self.durable_operation()?;
            self.validation.report(&self.host.view())
        }).await.map_err(|e|format!("validation worker failed: {e}"))?
    }

    pub(crate) fn durable_operation(&self) -> Result<MutexGuard<'_, ()>, String> {
        let guard = self
            .operation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(error) = self.fault() {
            return Err(format!("samepage.persistence: {error}"));
        }
        Ok(guard)
    }

    fn fault(&self) -> Option<String> {
        self.save_progress
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fault
            .clone()
    }

    fn wait_saved(&self, seq: u64) -> Result<(), String> {
        let (lock, ready) = &*self.save_progress;
        let mut progress = lock.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(error) = &progress.fault {
                return Err(format!("samepage.persistence: {error}"));
            }
            if progress.completed >= seq {
                return Ok(());
            }
            let (next, timeout) = ready
                .wait_timeout_while(progress, self.save_wait, |progress| {
                    progress.completed < seq && progress.fault.is_none()
                })
                .map_err(|_| "persistence coordination lock poisoned".to_string())?;
            progress = next;
            if timeout.timed_out()
                && progress.completed < seq
                && progress.fault.is_none()
                && progress.slow_target.is_none()
            {
                progress.slow_target = Some(seq);
                // Keep status publication inside the progress critical
                // section. A completed or failed save may finish its I/O in
                // parallel, but it cannot clear `slow_target` and publish a
                // resolved/error event before this wait event is delivered.
                (self.emit_event)(
                    "samepage.persistence-wait",
                    json!({"seq":seq,"message":format!("commit {seq} is still saving")}),
                );
            }
        }
    }

    fn invoke(&self, action: &str, args: Value) -> Result<String, String> {
        let surface_actor = current_actor().ok_or("authenticated action actor is missing")?;
        self.invoke_as(&surface_actor, action, args)
    }

    fn invoke_as(
        &self,
        surface_actor: &SurfaceActor,
        action: &str,
        args: Value,
    ) -> Result<String, String> {
        let _operation = self.durable_operation()?;
        let actor = map_actor(surface_actor)?;
        let invoked = self
            .host
            .invoke(&actor, action, args)
            .map_err(|error| error.to_string())?;
        if invoked.commit.is_some() {
            // One accepted change can release quarantined changes and announce
            // several commits. Do not release the operation gate until the
            // complete resulting journal prefix is durable.
            self.wait_saved(self.host.journal().seq)?;
        }
        let validation = self.validation.report(&self.host.view())?;
        if invoked.commit.is_some() && validation["status"] == "failed" {
            return Err(json!({"code":"PAGE_VALIDATION_FAILED","operation_applied":true,"action":invoked.action,"ids":invoked.ids,"applied_ids":invoked.commit.as_ref().map(|c|c.created.iter().chain(&c.modified).chain(&c.deleted).collect::<Vec<_>>()),"revision":self.host.revision(),"validation":validation}).to_string());
        }
        let report = if invoked.action == "review_read" {
            let mut report = self.review.as_ref().ok_or("review is unavailable")?
                .read(&self.host.view(), &actor.participant)?;
            if self.host.composition().descriptor("architecture").is_some() {
                report["architecture"] = samepage_architecture::inspect(&self.host.view())
                    .map_err(|e| e.to_string())?;
            }
            Some(report)
        } else {
            None
        };
        if invoked.action == "map_read" { self.input.acknowledge(&actor.participant, self.host.journal().seq); }
        let readback = report
            .as_ref()
            .and_then(|r| r["readback"].as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or(invoked.readback);
        let architecture = if invoked.action == "architecture_read" {
            Some(samepage_architecture::inspect(&self.host.view()).map_err(|e| e.to_string())?)
        } else {
            None
        };
        serde_json::to_string(&json!({
            "architecture": architecture,
            "action": invoked.action,
            "readback": readback,
            "review": report,
            "validation": validation,
            "ids": invoked.ids,
            "revision": self.host.revision(),
        }))
        .map_err(|error| error.to_string())
    }
}

fn run_persistence_worker<Save, Publish, Fault, Resolved>(
    commit_rx: mpsc::Receiver<Commit>,
    mut expected: u64,
    progress: Arc<(Mutex<SaveProgress>, Condvar)>,
    mut save: Save,
    mut publish: Publish,
    mut emit_fault: Fault,
    mut emit_resolved: Resolved,
) where
    Save: FnMut(&Commit) -> Result<(), String>,
    Publish: FnMut(&Commit),
    Fault: FnMut(String),
    Resolved: FnMut(u64),
{
    let mut pending = BTreeMap::<u64, Commit>::new();
    while let Ok(commit) = commit_rx.recv() {
        if commit.seq < expected {
            continue;
        }
        if pending.insert(commit.seq, commit).is_some() {
            let message = "duplicate document commit sequence".to_string();
            set_fault(&progress, message, &mut emit_fault);
            return;
        }
        while let Some(commit) = pending.remove(&expected) {
            if let Err(message) = save(&commit) {
                set_fault(&progress, message, &mut emit_fault);
                return;
            }
            let (lock, ready) = &*progress;
            let mut saved = lock.lock().unwrap_or_else(|poison| poison.into_inner());
            if saved.fault.is_some() {
                return;
            }
            saved.completed = saved.completed.max(commit.seq);
            let resolved = saved
                .slow_target
                .is_some_and(|target| saved.completed >= target);
            if resolved {
                saved.slow_target = None;
                emit_resolved(commit.seq);
            }
            ready.notify_all();
            drop(saved);
            publish(&commit);
            expected = expected.saturating_add(1);
        }
    }
    if !pending.is_empty() {
        let message = format!("document commit sequence {expected} never arrived");
        set_fault(&progress, message, &mut emit_fault);
    }
}

fn set_fault<F>(
    progress: &Arc<(Mutex<SaveProgress>, Condvar)>,
    message: String,
    emit_fault: &mut F,
) -> bool
where
    F: FnMut(String),
{
    let (lock, ready) = &**progress;
    let mut progress = lock.lock().unwrap_or_else(|p| p.into_inner());
    let inserted = progress.fault.is_none();
    progress.fault.get_or_insert(message.clone());
    if inserted {
        emit_fault(message);
    }
    ready.notify_all();
    inserted
}

fn map_actor(actor: &SurfaceActor) -> Result<Actor, String> {
    let participant = actor
        .participant_id
        .as_ref()
        .ok_or("host-minted participant is required")?;
    let caller = match actor.caller {
        ag_ui_surface::Caller::Human => Caller::Human,
        ag_ui_surface::Caller::Agent | ag_ui_surface::Caller::Companion => Caller::Agent,
        ag_ui_surface::Caller::Unknown => {
            return Err("unknown caller has no document authority".into())
        }
    };
    Ok(Actor {
        participant: participant.clone(),
        caller,
        label: actor.label.clone(),
    })
}

impl SurfaceState for SamePageState {
    fn backing(&self) -> StateBacking {
        StateBacking::Crdt
    }

    fn describe(&self) -> Result<String, String> {
        let _operation = self.durable_operation()?;
        let view = self.host.view();
        let projection = Map.project(&view).map_err(|error| error.to_string())?;
        serde_json::to_string(&json!({
            "revision": self.host.revision(),
            "map": projection,
            "board": self.board.lock().map_err(|_|"board binding lock poisoned")?.as_ref().map(|b|b.peek()),
            "sketch": if self.host.composition().descriptor("sketch").is_some() { Some(samepage_sketch::Sketch.digest(&view).map_err(|e| e.to_string())?) } else { None },
            "persistence_error": self.fault(),
        }))
        .map_err(|error| error.to_string())
    }

    fn reconnect_events(&self)->Vec<(String,Value)> {
        self.board.lock().unwrap_or_else(|p|p.into_inner()).as_ref().map(|b|vec![(crate::host::board::CHANGED_EVENT.into(),b.snapshot())]).unwrap_or_default()
    }
    fn snapshot(&self) -> Result<StateSnapshot, String> {
        let _operation = self.durable_operation()?;
        Ok(StateSnapshot {
            backing: StateBacking::Crdt,
            body: json!({"revision":self.host.revision(), "persistence_error": null}),
            chrome: None,
        })
    }

    fn ws_hello(&self) -> Result<Vec<Vec<u8>>, String> {
        let _operation = self.durable_operation()?;
        Ok(self.host.hello())
    }

    fn ws_refusals(&self) -> Option<broadcast::Receiver<WsRefusal>> {
        Some(self.refusal_tx.subscribe())
    }

    fn ws_receive_as_split(&self, actor: &SurfaceActor, data: &[u8]) -> Result<WsInbound, String> {
        let _operation = self.durable_operation()?;
        let actor = map_actor(actor)?;
        let reply = self
            .host
            .receive_frame(&actor, data)
            .map_err(|error| format!("samepage.refused: {error}"))?;
        self.wait_saved(self.host.journal().seq)?;
        Ok(WsInbound {
            replies: reply.replies,
            broadcasts: Vec::new(),
        })
    }
}

pub struct SamePageSurface {
    state: Arc<SamePageState>,
    tools: Vec<ActionDef>,
    artifacts: SelectedArtifacts,
    board: Option<Arc<crate::host::board::BoardHost>>,
}

impl SamePageSurface {
    pub fn open(
        path: PathBuf,
        artifacts: SelectedArtifacts,
        transport: Transport,
        project: &Path,
    ) -> Result<Self, String> {
        let board = if artifacts.board.is_some() {
            let emit=transport.clone();
            Some(crate::host::board::BoardHost::open(path.with_file_name("board.json"),Arc::new(move |name,value|emit.emit(name,value)))?)
        } else { None };
        let with_diagrams = artifacts.diagrams.is_some();
        let with_review = artifacts.review.is_some();
        let with_architecture = artifacts.architecture.is_some();
        let with_sketch = artifacts.sketch.is_some();
        let state = if with_diagrams || with_review || with_architecture || with_sketch {
            let emit = transport.clone();
            SamePageState::open_selected_capabilities(
                path,
                transport.ws_tx.clone(),
                Arc::new(|h| h.save().map(|_| ()).map_err(|e| e.to_string())),
                Arc::new(move |name, value| emit.emit(name, value)),
                Duration::from_secs(15),
                Some(project),
                with_diagrams,
                with_review,
                with_architecture,
                with_sketch,
            )?
        } else {
            SamePageState::open(path, transport)?
        };
        *state.board.lock().map_err(|_|"board binding lock poisoned")?=board.clone();
        let mut declarations = samepage_map::descriptor().actions;
        if with_diagrams {
            declarations.extend(samepage_diagrams::descriptor().actions);
        }
        if with_review {
            declarations.extend(samepage_review::descriptor().actions);
        }
        if with_architecture {
            declarations.extend(samepage_architecture::descriptor().actions);
        }
        if with_sketch {
            declarations.extend(samepage_sketch::descriptor().actions);
        }
        let mut tools: Vec<ActionDef> = declarations
            .into_iter()
            .map(|decl| {
                let name = decl.name.clone();
                let query = decl.effect == CapabilityEffect::Query;
                let state = state.clone();
                let mut action = ActionDef::new(
                    decl.name,
                    decl.description,
                    decl.input_schema,
                    move |args| {
                        let args = args.clone();
                        let state = state.clone();
                        let name = name.clone();
                        if query {
                            SurfaceEffect::Query(Box::new(move |_| state.invoke(&name, args)))
                        } else {
                            SurfaceEffect::Mutate(Box::new(move |_| {
                                state.invoke(&name, args).map(Some)
                            }))
                        }
                    },
                );
                action.audience = match decl.audience {
                    samepage_capability::Audience::Human => ActionAudience::Human,
                    samepage_capability::Audience::Agent => ActionAudience::Agent,
                    samepage_capability::Audience::Both => ActionAudience::Both,
                };
                for alias in decl.aliases {
                    action = action.alias(alias);
                }
                action
            })
            .collect();
        let observe_state = state.clone();
        let mut observe = ActionDef::new(
            "page_observe",
            "Report fresh browser layout and errors without changing shared records.",
            json!({"type":"object","required":["stream","revision","sequence","bounds","errors"],"properties":{"stream":{"type":"string","minLength":1,"maxLength":128},"revision":{"type":"string"},"sequence":{"type":"integer","minimum":1},"bounds":{"type":"object","additionalProperties":{"type":"array","minItems":4,"maxItems":4,"items":{"type":"number"}}},"errors":{"type":"array","maxItems":32,"items":{"type":"object","required":["code","message"],"properties":{"code":{"type":"string"},"message":{"type":"string"}},"additionalProperties":false}}},"additionalProperties":false}),
            move |args| {
                let args = args.clone();
                let state = observe_state.clone();
                SurfaceEffect::Query(Box::new(move |_| {
                    let _operation = state.durable_operation()?;
                    let actor =
                        map_actor(&current_actor().ok_or("authenticated browser actor missing")?)?;
                    let input = serde_json::from_value(args)
                        .map_err(|e| format!("invalid browser validation: {e}"))?;
                    state
                        .validation
                        .observe(&actor.participant, input, &state.host.view())?;
                    Ok(state.validation.report(&state.host.view())?.to_string())
                }))
            },
        );
        observe.audience = ActionAudience::Human;
        tools.push(observe);
        let validate_state = state.clone();
        tools.push(
            ActionDef::new(
                "page_validate",
                "Read native diagnostics and fresh browser validation.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                move |_| {
                    let state = validate_state.clone();
                    SurfaceEffect::Query(Box::new(move |_| {
                        let _operation = state.durable_operation()?;
                        Ok(state.validation.report(&state.host.view())?.to_string())
                    }))
                },
            )
            .alias("atlas_validate"),
        );
        let wait_state = state.clone();
        tools.push(ActionDef::new("review_wait","Wait for fresh browser validation, including late measurement errors from your own write.",json!({"type":"object","properties":{"seconds":{"type":"integer","minimum":0,"maximum":60}},"additionalProperties":false}),move |args|{
            let state=wait_state.clone();let seconds=args["seconds"].as_u64().unwrap_or(30);
            SurfaceEffect::AsyncQuery(Box::pin(async move {
                let generation=state.validation.generation();
                let report=state.clone().validation_report_async().await?;
                if report["status"]=="failed"{return Err(report.to_string());}
                state.validation.wait_since(generation,seconds).await;
                let report=state.clone().validation_report_async().await?;
                if report["status"]=="failed"{Err(report.to_string())}else{Ok(report.to_string())}
            }))
        }));
        if let Some(board) = &board {
            let revision_state=state.clone();
            tools.extend(board.actions(Arc::new(move || {
                let _operation=revision_state.durable_operation()?;
                Ok(revision_state.host.revision())
            })));
            tools.push(crate::host::board_input::input_action(state.clone(),board.clone()));
        }
        Ok(Self {
            state,
            tools,
            artifacts,
            board,
        })
    }
}

impl Surface for SamePageSurface {
    fn state(&self) -> &dyn SurfaceState {
        self.state.as_ref()
    }
    fn bind_semantic_targets(&self, service: &Arc<ag_ui_surface::SemanticTargetService>) {
        self.state.input.bind_attention(service.clone());
    }
    fn semantic_target_namespaces(&self) -> Vec<String> {
        let mut namespaces=vec!["map".into()];
        if self.board.is_some() { namespaces.push("board".into()); }
        namespaces
    }
    fn resolve_semantic_target(&self,target:&ag_ui_surface::SemanticTargetRef)->Result<Option<ag_ui_surface::SemanticTarget>,String> {
        let description=if target.extension_id=="board" { self.board.as_ref().and_then(|b|b.resolve(&target.target_id)) }
        else if target.extension_id=="map" {
            let _operation=self.state.durable_operation()?;
            self.state.host.view().get(&target.target_id).map(|r| format!("{} [{}]",samepage_map::prop_str(r,"label").unwrap_or("object"),r.id))
        } else { None };
        Ok(description.map(|text|ag_ui_surface::SemanticTarget::new(target.clone(),text.clone(),text)))
    }
    fn routes(&self)->Vec<ag_ui_surface::RouteDef> {
        let Some(board)=&self.board else { return vec![] };
        let board=board.clone();
        vec![ag_ui_surface::RouteDef{method:ag_ui_surface::HttpMethod::Get,path:"/board/state",handler:Box::new(move |_|{
            let board=board.clone(); Box::pin(async move { ag_ui_surface::RouteResponse::json(200,board.snapshot()) })
        })},ag_ui_surface::RouteDef{method:ag_ui_surface::HttpMethod::Get,path:"/board/primitives",handler:Box::new(move |_|Box::pin(async move { ag_ui_surface::RouteResponse::json(200,samepage_board::assertion::catalog()) }))}]
    }
    fn tools(&self) -> &[ActionDef] {
        &self.tools
    }
    fn document_mount(&self) -> Option<DocumentMount> {
        Some(self.artifacts.document.clone())
    }
    fn client_modules(&self) -> Vec<ClientModule> {
        let descriptor = samepage_map::descriptor();
        let mut module = ClientModule::lazy(
            descriptor.id,
            descriptor.version,
            self.artifacts.map.entry.url.clone(),
            "map",
        )
        .verified_artifact(self.artifacts.map.clone())
        .capability("samepage.map");
        for action in descriptor.actions {
            if action.audience != samepage_capability::Audience::Agent {
                module = module.action(action.name);
            }
        }
        let mut modules = vec![module];
        if let Some(artifact) = &self.artifacts.diagrams {
            let mut module = ClientModule::lazy(
                "diagrams",
                samepage_diagrams::VERSION,
                artifact.entry.url.clone(),
                "diagrams",
            )
            .verified_artifact(artifact.clone())
            .optional()
            .capability("samepage.diagrams");
            for action in samepage_diagrams::descriptor().actions {
                module = module.action(action.name);
            }
            modules.push(module);
        }
        if let Some(artifact) = &self.artifacts.review {
            let mut module = ClientModule::lazy(
                "review",
                samepage_review::VERSION,
                artifact.entry.url.clone(),
                "review",
            )
            .verified_artifact(artifact.clone())
            .optional()
            .capability("samepage.review");
            for action in samepage_review::descriptor().actions {
                if action.audience != samepage_capability::Audience::Agent {
                    module = module.action(action.name);
                }
            }
            modules.push(module);
        }
        if let Some(artifact) = &self.artifacts.architecture {
            let mut module = ClientModule::lazy(
                "architecture", samepage_architecture::VERSION,
                artifact.entry.url.clone(), "architecture",
            )
            .verified_artifact(artifact.clone())
            .optional()
            .capability("samepage.architecture");
            for action in samepage_architecture::descriptor().actions {
                if action.audience != samepage_capability::Audience::Agent {
                    module = module.action(action.name);
                }
            }
            modules.push(module);
        }
        if let Some(artifact) = &self.artifacts.board {
            let mut module=ClientModule::lazy("board",samepage_board::VERSION,artifact.entry.url.clone(),"board")
                .verified_artifact(artifact.clone()).optional().capability("samepage.board").event(crate::host::board::CHANGED_EVENT);
            for action in samepage_board::descriptor().actions {
                if action.audience != samepage_capability::Audience::Agent { module=module.action(action.name); }
            }
            modules.push(module);
        }
        if let Some(artifact) = &self.artifacts.sketch {
            let mut module = ClientModule::lazy("sketch", samepage_sketch::VERSION, artifact.entry.url.clone(), "sketch")
                .verified_artifact(artifact.clone()).optional().capability("samepage.sketch");
            for action in samepage_sketch::descriptor().actions { module = module.action(action.name); }
            modules.push(module);
        }
        modules
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact_identity(root: &Path, relative: &str, contents: &[u8]) -> Value {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        json!({
            "url": format!("/capabilities/{relative}"),
            "sha256": format!("{:x}", Sha256::digest(contents)),
            "bytes": contents.len()
        })
    }

    #[test]
    fn selected_artifacts_require_v2_capability_and_document_v1() {
        let unique = format!(
            "samepage-v2-artifacts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let doc_js = artifact_identity(&root, "doc/doc.js", b"doc-js");
        let doc_wasm = artifact_identity(&root, "doc/doc_bg.wasm", b"doc-wasm");
        let map_entry = artifact_identity(&root, "map/index.js", b"map-entry");
        let map_glue = artifact_identity(&root, "map/map.js", b"map-js");
        let map_wasm = artifact_identity(&root, "map/map_bg.wasm", b"map-wasm");
        let mut manifest = json!({
            "schemaVersion": 1,
            "buildOwner": "samepage-capabilities.v1",
            "artifacts": [
                {
                    "id":"doc", "kind":"document", "abi":"samepage.document.v1",
                    "glue":doc_js, "wasm":doc_wasm, "layoutVersion":1
                },
                {
                    "id":"map", "kind":"capability", "abi":"samepage.capability.v2",
                    "glue":map_glue, "wasm":map_wasm, "entry":map_entry,
                    "descriptor":samepage_map::descriptor()
                }
            ]
        });
        std::fs::write(
            root.join("capabilities.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(SelectedArtifacts::open(&root).is_ok());

        manifest["artifacts"][1]["abi"] = json!("samepage.capability.v1");
        manifest["artifacts"][1]["descriptor"]["abi"] = json!("samepage.capability.v1");
        std::fs::write(
            root.join("capabilities.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let error = match SelectedArtifacts::open(&root) {
            Ok(_) => panic!("v1 capability artifact was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("unsupported C4 artifact map"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn test_progress(completed: u64) -> Arc<(Mutex<SaveProgress>, Condvar)> {
        Arc::new((
            Mutex::new(SaveProgress {
                completed,
                fault: None,
                slow_target: None,
            }),
            Condvar::new(),
        ))
    }

    fn test_commit(seq: u64) -> Commit {
        Commit {
            seq,
            actor: Actor {
                participant: "persistence-test".into(),
                caller: Caller::Agent,
                label: None,
            },
            action: Some("card_place".into()),
            revision: format!("revision-{seq}"),
            created: Vec::new(),
            modified: Vec::new(),
            deleted: Vec::new(),
            visible_change: true,
            update: vec![seq as u8],
        }
    }

    #[test]
    fn unauthenticated_and_unknown_surface_actors_have_no_document_authority() {
        let anonymous = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        assert!(map_actor(&anonymous).is_err());
        let mut unknown = SurfaceActor::anonymous(ag_ui_surface::Caller::Unknown);
        unknown.participant_id = Some("unknown-c4-test".into());
        assert!(map_actor(&unknown).is_err());
        let mut human = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        human.participant_id = Some("person-c4-test".into());
        human.label = Some("Real Person".into());
        let mapped = map_actor(&human).unwrap();
        assert_eq!(mapped.participant, "person-c4-test");
        assert_eq!(mapped.label.as_deref(), Some("Real Person"));
        assert_eq!(mapped.caller, Caller::Human);
        let mut agent = SurfaceActor::anonymous(ag_ui_surface::Caller::Agent);
        agent.participant_id = Some("agent-c4-test".into());
        assert_eq!(map_actor(&agent).unwrap().caller, Caller::Agent);
    }

    #[test]
    fn persistence_worker_reorders_callbacks_and_never_regresses_watermark() {
        let (commit_tx, commit_rx) = mpsc::channel();
        let (saved_tx, saved_rx) = mpsc::channel();
        let (published_tx, published_rx) = mpsc::channel();
        let progress = test_progress(0);
        let worker_progress = progress.clone();
        let worker = std::thread::spawn(move || {
            run_persistence_worker(
                commit_rx,
                1,
                worker_progress,
                move |commit| {
                    saved_tx.send(commit.seq).unwrap();
                    Ok(())
                },
                move |commit| published_tx.send(commit.seq).unwrap(),
                |_| {},
                |_| {},
            );
        });

        commit_tx.send(test_commit(2)).unwrap();
        assert!(saved_rx.recv_timeout(Duration::from_millis(75)).is_err());
        commit_tx.send(test_commit(1)).unwrap();
        assert_eq!(saved_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        assert_eq!(saved_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
        assert_eq!(
            published_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            1
        );
        assert_eq!(
            published_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            2
        );
        drop(commit_tx);
        worker.join().unwrap();
        let saved = progress.0.lock().unwrap();
        assert_eq!(saved.completed, 2);
        assert!(saved.fault.is_none());
    }

    #[test]
    fn first_persistence_fault_is_terminal_for_queued_commits() {
        let (commit_tx, commit_rx) = mpsc::channel();
        commit_tx.send(test_commit(1)).unwrap();
        commit_tx.send(test_commit(2)).unwrap();
        drop(commit_tx);
        let progress = test_progress(0);
        let saved = Arc::new(Mutex::new(Vec::new()));
        let published = Arc::new(Mutex::new(Vec::new()));
        let faults = Arc::new(Mutex::new(Vec::new()));

        run_persistence_worker(
            commit_rx,
            1,
            progress.clone(),
            {
                let saved = saved.clone();
                move |commit| {
                    saved.lock().unwrap().push(commit.seq);
                    Err("forced permanent failure".into())
                }
            },
            {
                let published = published.clone();
                move |commit| published.lock().unwrap().push(commit.seq)
            },
            {
                let faults = faults.clone();
                move |message| faults.lock().unwrap().push(message)
            },
            |_| {},
        );

        assert_eq!(*saved.lock().unwrap(), vec![1]);
        assert!(published.lock().unwrap().is_empty());
        assert_eq!(
            faults.lock().unwrap().as_slice(),
            ["forced permanent failure"]
        );
        let progress = progress.0.lock().unwrap();
        assert_eq!(progress.completed, 0);
        assert_eq!(progress.fault.as_deref(), Some("forced permanent failure"));
    }

    #[test]
    fn stalled_failed_save_blocks_every_authoritative_read() {
        let unique = format!(
            "samepage-c4-persistence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("document.json");
        let (ws_tx, mut ws_rx) = broadcast::channel(8);
        let (save_entered_tx, save_entered_rx) = mpsc::channel();
        let (release_save_tx, release_save_rx) = mpsc::channel();
        let release_save_rx = Arc::new(Mutex::new(release_save_rx));
        let state = SamePageState::open_with_io(
            path.clone(),
            ws_tx,
            Arc::new(move |_| {
                save_entered_tx.send(()).unwrap();
                release_save_rx.lock().unwrap().recv().unwrap();
                Err("forced stalled save failure".into())
            }),
            Arc::new(|_, _| {}),
            Duration::from_secs(1),
        )
        .unwrap();
        let mut actor = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        actor.participant_id = Some("persistence-human".into());
        actor.label = Some("Persistence Human".into());

        let (mutation_tx, mutation_rx) = mpsc::channel();
        let mutation_state = state.clone();
        let mutation_actor = actor.clone();
        std::thread::spawn(move || {
            let result = mutation_state.invoke_as(
                &mutation_actor,
                "card_place",
                json!({"label":"must remain externally invisible"}),
            );
            mutation_tx.send(result).unwrap();
        });
        save_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let (read_tx, read_rx) = mpsc::channel();
        for read in ["describe", "hello", "query"] {
            let state = state.clone();
            let actor = actor.clone();
            let started_tx = started_tx.clone();
            let read_tx = read_tx.clone();
            std::thread::spawn(move || {
                started_tx.send(read).unwrap();
                let result = match read {
                    "describe" => state.describe().map(|_| ()),
                    "hello" => state.ws_hello().map(|_| ()),
                    "query" => state.invoke_as(&actor, "map_read", json!({})).map(|_| ()),
                    _ => unreachable!(),
                };
                read_tx.send((read, result)).unwrap();
            });
        }
        drop(started_tx);
        drop(read_tx);
        for _ in 0..3 {
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        assert!(read_rx.recv_timeout(Duration::from_millis(75)).is_err());

        release_save_tx.send(()).unwrap();
        let mutation = mutation_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(mutation
            .unwrap_err()
            .contains("forced stalled save failure"));
        for _ in 0..3 {
            let (_, result) = read_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(result.unwrap_err().contains("forced stalled save failure"));
        }
        assert!(matches!(
            ws_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)
        ));
        assert!(!path.exists());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_that_succeeds_after_slow_threshold_stays_gated_then_persists() {
        let root = std::env::temp_dir().join(format!(
            "samepage-c4-late-success-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("document.json");
        let (ws_tx, mut ws_rx) = broadcast::channel(8);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (event_tx, event_rx) = mpsc::channel();
        let state = SamePageState::open_with_io(
            path.clone(),
            ws_tx,
            Arc::new(move |host| {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                host.save().map(|_| ()).map_err(|error| error.to_string())
            }),
            Arc::new(move |name, value| event_tx.send((name.to_string(), value)).unwrap()),
            Duration::from_millis(30),
        )
        .unwrap();
        let mut actor = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        actor.participant_id = Some("late-success-human".into());
        actor.label = Some("Late Success Human".into());

        let mutation_state = state.clone();
        let mutation_actor = actor.clone();
        let (mutation_tx, mutation_rx) = mpsc::channel();
        std::thread::spawn(move || {
            mutation_tx
                .send(mutation_state.invoke_as(
                    &mutation_actor,
                    "card_place",
                    json!({"label":"durable after slow save"}),
                ))
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (event, value) = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event, "samepage.persistence-wait");
        assert_eq!(value["seq"], 1);

        let read_state = state.clone();
        let (read_tx, read_rx) = mpsc::channel();
        std::thread::spawn(move || read_tx.send(read_state.describe()).unwrap());
        assert!(read_rx.recv_timeout(Duration::from_millis(75)).is_err());

        release_tx.send(()).unwrap();
        assert!(mutation_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_ok());
        let described = read_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(described.contains("durable after slow save"));
        let (event, value) = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event, "samepage.persistence-resolved");
        assert_eq!(value["seq"], 1);
        let broadcast_deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match ws_rx.try_recv() {
                Ok(_) => break,
                Err(broadcast::error::TryRecvError::Empty)
                    if std::time::Instant::now() < broadcast_deadline =>
                {
                    std::thread::yield_now();
                }
                other => panic!("durable commit was not broadcast: {other:?}"),
            }
        }
        drop(state);

        let reopened = Host::open(
            &path,
            Composition::new(vec![samepage_map::descriptor()]).unwrap(),
            vec![Arc::new(Map)],
        )
        .unwrap();
        let projection = Map.project(&reopened.view()).unwrap();
        assert_eq!(projection["cards"][0]["label"], "durable after slow save");
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn held_wait_notification_cannot_be_overtaken_by_resolution() {
        let root = std::env::temp_dir().join(format!(
            "samepage-c4-wait-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("document.json");
        let (ws_tx, _) = broadcast::channel(8);
        let (save_entered_tx, save_entered_rx) = mpsc::channel();
        let (release_save_tx, release_save_rx) = mpsc::channel();
        let release_save_rx = Arc::new(Mutex::new(release_save_rx));
        let (wait_entered_tx, wait_entered_rx) = mpsc::channel();
        let (release_wait_tx, release_wait_rx) = mpsc::channel();
        let release_wait_rx = Arc::new(Mutex::new(release_wait_rx));
        let (resolved_tx, resolved_rx) = mpsc::channel();
        let events = Arc::new(Mutex::new(Vec::new()));
        let event_log = events.clone();
        let state = SamePageState::open_with_io(
            path,
            ws_tx,
            Arc::new(move |host| {
                save_entered_tx.send(()).unwrap();
                release_save_rx.lock().unwrap().recv().unwrap();
                host.save().map(|_| ()).map_err(|error| error.to_string())
            }),
            Arc::new(move |name, _| {
                if name == "samepage.persistence-wait" {
                    wait_entered_tx.send(()).unwrap();
                    release_wait_rx.lock().unwrap().recv().unwrap();
                }
                event_log.lock().unwrap().push(name.to_string());
                if name == "samepage.persistence-resolved" {
                    resolved_tx.send(()).unwrap();
                }
            }),
            Duration::from_millis(20),
        )
        .unwrap();
        let mut actor = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        actor.participant_id = Some("wait-order-human".into());
        let mutation_state = state.clone();
        let (mutation_tx, mutation_rx) = mpsc::channel();
        std::thread::spawn(move || {
            mutation_tx
                .send(mutation_state.invoke_as(
                    &actor,
                    "card_place",
                    json!({"label":"ordered status"}),
                ))
                .unwrap();
        });

        save_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        wait_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        // The save finishes while delivery of the wait notification is held.
        // Resolved must remain blocked behind that notification.
        release_save_tx.send(()).unwrap();
        assert!(resolved_rx.recv_timeout(Duration::from_millis(75)).is_err());
        release_wait_tx.send(()).unwrap();
        resolved_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(mutation_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok());
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["samepage.persistence-wait", "samepage.persistence-resolved"]
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn held_resolution_cannot_be_overtaken_by_a_newer_wait() {
        let root = std::env::temp_dir().join(format!(
            "samepage-c4-resolved-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("document.json");
        let (ws_tx, _) = broadcast::channel(8);
        let (save_entered_tx, save_entered_rx) = mpsc::channel();
        let (release_save_tx, release_save_rx) = mpsc::channel();
        let release_save_rx = Arc::new(Mutex::new(release_save_rx));
        let save_number = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (wait_tx, wait_rx) = mpsc::channel();
        let (resolved_entered_tx, resolved_entered_rx) = mpsc::channel();
        let (release_resolved_tx, release_resolved_rx) = mpsc::channel();
        let release_resolved_rx = Arc::new(Mutex::new(Some(release_resolved_rx)));
        let resolved_number = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let event_log = events.clone();
        let state = SamePageState::open_with_io(
            path,
            ws_tx,
            Arc::new(move |host| {
                let number = save_number.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                save_entered_tx.send(number).unwrap();
                release_save_rx.lock().unwrap().recv().unwrap();
                host.save().map(|_| ()).map_err(|error| error.to_string())
            }),
            Arc::new(move |name, value| {
                if name == "samepage.persistence-resolved"
                    && resolved_number.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                {
                    resolved_entered_tx.send(()).unwrap();
                    release_resolved_rx
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .recv()
                        .unwrap();
                }
                event_log.lock().unwrap().push(name.to_string());
                if name == "samepage.persistence-wait" {
                    wait_tx.send(value["seq"].as_u64().unwrap()).unwrap();
                }
            }),
            Duration::from_millis(20),
        )
        .unwrap();
        let mut actor = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        actor.participant_id = Some("resolved-order-human".into());

        let first_state = state.clone();
        let first_actor = actor.clone();
        let (first_tx, first_rx) = mpsc::channel();
        std::thread::spawn(move || {
            first_tx
                .send(first_state.invoke_as(
                    &first_actor,
                    "card_place",
                    json!({"label":"first ordered status"}),
                ))
                .unwrap();
        });
        assert_eq!(
            save_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            1
        );
        assert_eq!(wait_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        release_save_tx.send(()).unwrap();
        resolved_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let second_state = state.clone();
        let (second_tx, second_rx) = mpsc::channel();
        std::thread::spawn(move || {
            second_tx
                .send(second_state.invoke_as(
                    &actor,
                    "card_place",
                    json!({"label":"second ordered status"}),
                ))
                .unwrap();
        });
        assert!(first_rx.recv_timeout(Duration::from_millis(75)).is_err());
        assert!(save_entered_rx
            .recv_timeout(Duration::from_millis(75))
            .is_err());
        assert!(wait_rx.recv_timeout(Duration::from_millis(75)).is_err());

        release_resolved_tx.send(()).unwrap();
        assert!(first_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok());
        assert_eq!(
            save_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            2
        );
        assert_eq!(wait_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
        assert_eq!(
            events.lock().unwrap().as_slice(),
            [
                "samepage.persistence-wait",
                "samepage.persistence-resolved",
                "samepage.persistence-wait"
            ]
        );
        release_save_tx.send(()).unwrap();
        assert!(second_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok());
        assert_eq!(
            events.lock().unwrap().as_slice(),
            [
                "samepage.persistence-wait",
                "samepage.persistence-resolved",
                "samepage.persistence-wait",
                "samepage.persistence-resolved"
            ]
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_that_fails_after_slow_threshold_never_reads_broadcasts_or_replays() {
        let root = std::env::temp_dir().join(format!(
            "samepage-c4-late-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("document.json");
        let (ws_tx, mut ws_rx) = broadcast::channel(8);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (event_tx, event_rx) = mpsc::channel();
        let (wait_entered_tx, wait_entered_rx) = mpsc::channel();
        let (release_wait_tx, release_wait_rx) = mpsc::channel();
        let release_wait_rx = Arc::new(Mutex::new(release_wait_rx));
        let (error_delivered_tx, error_delivered_rx) = mpsc::channel();
        let state = SamePageState::open_with_io(
            path.clone(),
            ws_tx,
            Arc::new(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                Err("late permanent failure".into())
            }),
            Arc::new(move |name, value| {
                if name == "samepage.persistence-wait" {
                    wait_entered_tx.send(()).unwrap();
                    release_wait_rx.lock().unwrap().recv().unwrap();
                }
                event_tx.send((name.to_string(), value)).unwrap();
                if name == "samepage.persistence-error" {
                    error_delivered_tx.send(()).unwrap();
                }
            }),
            Duration::from_millis(30),
        )
        .unwrap();
        let mut actor = SurfaceActor::anonymous(ag_ui_surface::Caller::Human);
        actor.participant_id = Some("late-failure-human".into());

        let mutation_state = state.clone();
        let mutation_actor = actor.clone();
        let (mutation_tx, mutation_rx) = mpsc::channel();
        std::thread::spawn(move || {
            mutation_tx
                .send(mutation_state.invoke_as(
                    &mutation_actor,
                    "card_place",
                    json!({"label":"must never replay"}),
                ))
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        wait_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let read_state = state.clone();
        let (read_tx, read_rx) = mpsc::channel();
        std::thread::spawn(move || read_tx.send(read_state.describe()).unwrap());
        assert!(read_rx.recv_timeout(Duration::from_millis(75)).is_err());
        // Finish the failed save while the wait event itself is deliberately
        // held. The terminal error must not overtake it.
        release_tx.send(()).unwrap();
        assert!(error_delivered_rx
            .recv_timeout(Duration::from_millis(75))
            .is_err());
        release_wait_tx.send(()).unwrap();
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap().0,
            "samepage.persistence-wait"
        );

        assert!(mutation_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .contains("late permanent failure"));
        assert!(read_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .contains("late permanent failure"));
        assert_eq!(
            event_rx.recv_timeout(Duration::from_secs(1)).unwrap().0,
            "samepage.persistence-error"
        );
        assert!(matches!(
            ws_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)
        ));
        assert!(!path.exists());
        assert!(state.invoke_as(&actor, "map_read", json!({})).is_err());
        drop(state);

        let reopened = Host::open(
            &path,
            Composition::new(vec![samepage_map::descriptor()]).unwrap(),
            vec![Arc::new(Map)],
        )
        .unwrap();
        let projection = Map.project(&reopened.view()).unwrap();
        assert_eq!(projection["cards"].as_array().unwrap().len(), 0);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod review_tests;

#[cfg(test)]
#[path = "architecture_tests.rs"]
mod architecture_tests;
#[cfg(test)]
#[path = "board_tests.rs"]
mod board_tests;
