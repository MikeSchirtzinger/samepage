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
use samepage_capability::Scalar;
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
    trace: Option<CapabilityArtifact>,
    pub(crate) panes: Option<CapabilityArtifact>,
    sources: Option<CapabilityArtifact>,
    lanes: Option<CapabilityArtifact>,
    cement: Option<CapabilityArtifact>,
    explain: Option<CapabilityArtifact>,
}

impl SelectedArtifacts {
    pub fn trace_enabled(&self) -> bool { self.trace.is_some() }
    pub fn open(root: &Path) -> Result<Self, String> {
        let path = root.join("capabilities.json");
        let data = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let manifest: BuildManifest = serde_json::from_slice(&data)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        if manifest.schema_version != 1 || manifest.build_owner != "samepage-capabilities.v1" {
            return Err("unsupported selected capability manifest".to_string());
        }
        if !(2..=13).contains(&manifest.artifacts.len()) {
            return Err("requires doc, map and optional diagrams/review/architecture/board/sketch/trace/panes/sources/lanes/cement/explain artifacts".to_string());
        }
        let mut document = None;
        let mut map = None;
        let mut diagrams = None;
        let mut review = None;
        let mut architecture = None;
        let mut board = None;
        let mut sketch = None;
        let mut trace = None;
        let mut panes = None;
        let mut sources = None;
        let mut lanes = None;
        let mut cement = None;
        let mut explain = None;
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
                "map" | "diagrams" | "review" | "architecture" | "board" | "sketch" | "trace" | "panes" | "sources" | "lanes" | "cement" | "explain"
                    if artifact.kind == "capability"
                        && artifact.abi == "samepage.capability.v2" =>
                {
                    let compiled = serde_json::to_value(match artifact.id.as_str() {
                        "map" => samepage_map::descriptor(),
                        "review" => samepage_review::descriptor(),
                        "architecture" => samepage_architecture::descriptor(),
                        "board" => samepage_board::descriptor(),
                        "sketch" => samepage_sketch::descriptor(),
                        "trace" => samepage_trace::descriptor(),
                        "panes" => samepage_panes::descriptor(),
                        "sources" => samepage_sources::descriptor(),
                        "lanes" => samepage_lanes::descriptor(),
                        "cement" => samepage_cement::descriptor(),
                        "diagrams"
                            if artifact.descriptor.as_ref().is_some_and(|d| {
                                d["depends_on"].as_array().is_some_and(|deps| {
                                    deps.iter().any(|dep| dep["id"] == "sources")
                                })
                            }) =>
                        {
                            samepage_diagrams::WithSources.descriptor()
                        }
                        "explain" => samepage_explain::descriptor(),
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
                        "trace" => trace = Some(capability),
                        "panes" => panes = Some(capability),
                        "sources" => sources = Some(capability),
                        "lanes" => lanes = Some(capability),
                        "cement" => cement = Some(capability),
                        "explain" => explain = Some(capability),
                        _ => diagrams = Some(capability),
                    }
                }
                _ => return Err(format!("unsupported C4 artifact {}", artifact.id)),
            }
        }
        if cement.is_some() && (sources.is_none() || lanes.is_none() || review.is_none()) {
            return Err("cement requires Sources, Lanes and Review artifacts".into());
        }
        if lanes.is_some() && sources.is_none() {
            return Err("lanes requires the selected sources artifact".into());
        }
        if let Some(diagram) = &diagrams {
            let integrated = diagram.descriptor["depends_on"]
                .as_array()
                .is_some_and(|deps| deps.iter().any(|dep| dep["id"] == "sources"));
            if integrated != sources.is_some() {
                return Err("diagrams source ownership differs from the selected composition; rebuild the selected artifacts together".into());
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
            trace,
            panes,
            sources,
            lanes,
            cement,
            explain,
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
    panes: Option<Arc<crate::host::panes::PanesHost>>,
    sources: Option<Arc<crate::host::sources::SourcesHost>>,
    lanes: Option<Arc<crate::host::lanes::LanesHost>>,
    cement: Option<Arc<crate::host::cement::CementHost>>,
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
            path, ws_tx, saver, emit_event, save_wait, project, with_diagrams, with_review, false, false, false, false, false, false, false,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn open_selected_architecture(
        path: PathBuf, ws_tx: broadcast::Sender<Vec<u8>>, saver: HostSaver, emit_event: EventEmitter,
        save_wait: Duration, project: Option<&Path>, with_diagrams: bool, with_review: bool, with_architecture: bool,
    ) -> Result<Arc<Self>, String> {
        Self::open_selected_capabilities(
            path, ws_tx, saver, emit_event, save_wait, project, with_diagrams, with_review, with_architecture, false, false, false, false, false, false,
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
        with_panes: bool,
        with_sources: bool,
        with_lanes: bool,
        with_cement: bool,
        with_explain: bool,
    ) -> Result<Arc<Self>, String> {
        let sources = if with_sources {
            Some(Arc::new(crate::host::sources::SourcesHost::open(
                project.ok_or("sources needs a project root")?,
                &path,
            )?))
        } else {
            None
        };
        let lanes = if with_lanes {
            Some(Arc::new(crate::host::lanes::LanesHost::open(
                project.ok_or("lanes needs a project root")?,
                &path,
            )?))
        } else {
            None
        };
        let mut descriptors = vec![samepage_map::descriptor()];
        let mut capabilities: Vec<Arc<dyn Capability>> = vec![Arc::new(Map)];
        if with_sources {
            descriptors.push(samepage_sources::descriptor());
            capabilities.push(Arc::new(samepage_sources::Sources));
        }
        if with_diagrams {
            if with_sources {
                descriptors.push(samepage_diagrams::WithSources.descriptor());
                capabilities.push(Arc::new(samepage_diagrams::WithSources));
            } else {
                descriptors.push(samepage_diagrams::descriptor());
                capabilities.push(Arc::new(Diagrams));
            }
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
        let panes=if with_panes {
            descriptors.push(samepage_panes::descriptor());
            capabilities.push(Arc::new(samepage_panes::Panes));
            Some(Arc::new(crate::host::panes::PanesHost::open(project.ok_or("panes needs a project root")?)?))
        } else { None };
        if with_lanes {
            descriptors.push(samepage_lanes::descriptor());
            capabilities.push(Arc::new(samepage_lanes::Lanes));
        }
        let cement = if with_cement {
            descriptors.push(samepage_cement::descriptor());
            capabilities.push(Arc::new(samepage_cement::Cement));
            Some(Arc::new(crate::host::cement::CementHost::open(
                project.ok_or("cement needs project root")?,
                &path,
            )?))
        } else {
            None
        };
        if with_explain {
            descriptors.push(samepage_explain::descriptor());
            capabilities.push(Arc::new(samepage_explain::Explain));
        }
        let composition = Composition::new(descriptors).map_err(|error| error.to_string())?;
        let mut host = Host::open(path, composition, capabilities).map_err(|e| e.to_string())?;
        if with_diagrams && !with_sources {
            let project = project.ok_or("diagrams needs a project root")?;
            host = host
                .with_verifier(
                    "diagrams",
                    Arc::new(crate::diagram_sources::DiagramSources::open(project)?),
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(sources) = &sources {
            host = host
                .with_verifier(
                    "sources",
                    Arc::new(crate::host::sources::Repository::open(&sources.repo.root)?),
                )
                .map_err(|e| e.to_string())?;
        }
        if let Some(review) = &review {
            host = host
                .with_verifier("review", review.clone())
                .map_err(|e| e.to_string())?;
        }
        if !with_review || !with_sketch || !with_explain {
            host = host.with_verifier("map", Arc::new(crate::host::explain::MissingOwners { review: with_review, sketch: with_sketch, explain: with_explain }))
                .map_err(|e| e.to_string())?;
        }
        if with_sketch && !with_explain {
            host = host.with_verifier("sketch", Arc::new(crate::host::explain::ExplainUnavailable)).map_err(|e|e.to_string())?;
        }
        if with_sketch {
            host = host.with_composer("sketch", Arc::new(crate::host::sketch::SketchHost { review: with_review }))
                .map_err(|e| e.to_string())?;
        }
        if with_sketch {
            host = host.with_plan_context("map", &["shape"]).map_err(|e| e.to_string())?;
            if with_review { host = host.with_plan_context("review", &["shape"]).map_err(|e| e.to_string())?; }
        }
        if let Some(lanes) = &lanes {
            host = host
                .with_verifier("lanes", lanes.clone())
                .map_err(|e| e.to_string())?;
        }
        if let Some(cement) = &cement {
            host = host
                .with_verifier("cement", cement.clone())
                .map_err(|e| e.to_string())?;
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
            panes,
            sources,
            lanes,
            cement,
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
        let pane_wake=state.panes.clone();
        host.on_commit(move |commit| {
            input.changed.notify_waiters();
            if let Some(panes)=&pane_wake { panes.changed.notify_waiters(); }
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
        })
        .await
        .map_err(|e| format!("validation worker failed: {e}"))?
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
        if actor.caller==Caller::Agent && (action=="pane_put" || action=="room_put_pane") && (args.get("spot").is_some() || args.get("raise").is_some()) {
            return Err("the agent cannot send a rectangle or raise; use relations and named sizes".into());
        }
        let mut cement_result = None;
        let mut publication = None;
        let invoked = if matches!(action, "decision_cement" | "atlas_cement_decision") {
            if actor.caller != Caller::Human {
                return Err("only a human may publish a reviewed decision".into());
            }
            let cement = self.cement.as_ref().ok_or("cement unavailable")?;
            let mut pending = cement.publish(&self.host.view(), &args)?;
            let result = self.host.invoke_composed(&actor, action, &pending.batch);
            if let Err(e) = &result {
                let cleanup = pending.rollback();
                return Err(format!("{e}; rollback: {cleanup:?}"));
            }
            cement_result = Some(pending.result.clone());
            publication = Some(pending);
            result
        } else if matches!(action, "lane_claim" | "atlas_claim_lane") {
            let lanes = self.lanes.as_ref().ok_or("lanes unavailable")?;
            let sources = self.sources.as_ref().ok_or("sources unavailable")?;
            let batch = lanes.prepare(
                args.clone(),
                &self.host.view(),
                sources.facts(&self.host.view())?,
            )?;
            self.host.invoke_composed(&actor, action, &batch)
        } else if self.sources.is_some() && matches!(action, "card_place" | "atlas_place") {
            let batch = samepage_sources::place_batch(
                &samepage_capability::Call {
                    action: "card_place".into(),
                    args: args.clone(),
                },
                &self.host.view(),
            )
            .map_err(|e| e.to_string())?;
            self.host.invoke_composed(&actor, action, &batch)
        } else {
            self.host.invoke(&actor, action, args.clone())
        }
        .map_err(|e| e.to_string())?;
        if let Some(pending) = publication {
            pending.accept();
        }
        let flow_id = args["flow_id"].as_str().or_else(|| args["flow"]["id"].as_str()).unwrap_or("").to_string();
        let reenter = args["command"] == "restart";
        if invoked.commit.is_some() {
            // One accepted change can release quarantined changes and announce
            // several commits. Do not release the operation gate until the
            // complete resulting journal prefix is durable.
            self.wait_saved(self.host.journal().seq)?;
        }
        if self.panes.is_some()
            && (invoked.action.starts_with("pane_") || invoked.action.starts_with("panes_"))
            && invoked.action != "panes_present"
        {
            (self.emit_event)(
                "samepage.capability-used",
                json!({"id":"panes","tool":invoked.action}),
            );
        }
        if self.cement.is_some()
            && samepage_cement::descriptor()
                .actions
                .iter()
                .any(|a| a.name == invoked.action)
        {
            let cement = self.cement.as_ref().ok_or("cement unavailable")?;
            if cement_result.is_none() {
                cement_result = Some(match invoked.action.as_str() {
                    "decision_preview" => cement.preview(&self.host.view(), &args, true)?,
                    "decision_cement_propose" => cement.preview(&self.host.view(), &args, false)?,
                    "decision_list" => cement.history()?,
                    "map_cement" => cement.map_cement(
                        &self.host.view(),
                        &args,
                        self.sources.as_ref().ok_or("sources unavailable")?,
                    )?,
                    "lane_remove" => cement.remove(
                        &self.host.view(),
                        &args,
                        self.lanes.as_ref().ok_or("lanes unavailable")?,
                        self.sources.as_ref().ok_or("sources unavailable")?,
                    )?,
                    _ => samepage_cement::Cement
                        .project(&self.host.view())
                        .map_err(|e| e.to_string())?,
                });
            }
            (self.emit_event)("samepage.capability-used", json!({"id":"cement"}));
        }
        let validation = self.validation.report(&self.host.view())?;
        if invoked.commit.is_some() && validation["status"] == "failed" {
            return Err(json!({"code":"PAGE_VALIDATION_FAILED","operation_applied":true,"action":invoked.action,"ids":invoked.ids,"applied_ids":invoked.commit.as_ref().map(|c|c.created.iter().chain(&c.modified).chain(&c.deleted).collect::<Vec<_>>()),"revision":self.host.revision(),"validation":validation}).to_string());
        }
        let pane_read=if invoked.action=="panes_read" {
            Some(self.panes.as_ref().ok_or("panes is unavailable")?.read(&self.host.view(),&self.host.journal(),&actor.participant)?)
        } else { None };
        // Source facts are large. Only source actions return them; a write
        // that touched cards gets those cards' tiers as one read-back line
        // each, and review_read uses tiers internally.
        let source_action = matches!(
            invoked.action.as_str(),
            "source_read" | "source_list" | "sources_read"
        );
        let touched_cards: Vec<String> = {
            let view = self.host.view();
            invoked
                .commit
                .as_ref()
                .map(|c| c.created.iter().chain(&c.modified).cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .filter(|id| view.get(id).is_some_and(|r| r.kind == "node"))
                .collect()
        };
        let mut source_result = match &self.sources {
            Some(s) if source_action => Some(s.action(&invoked.action, &args, &self.host.view())?),
            Some(s) if invoked.action == "review_read" || !touched_cards.is_empty() => {
                Some(s.inspect(&self.host.view())?)
            }
            _ => None,
        };
        if let (Some(cement), Some(source)) = (&self.cement, &mut source_result) {
            if source.get("facts").is_some() {
                let (gates, check) = cement.gates(&self.host.view())?;
                source["facts"]["gates"] =
                    serde_json::to_value(gates).map_err(|e| e.to_string())?;
                let facts =
                    serde_json::from_value(source["facts"].clone()).map_err(|e| e.to_string())?;
                source["projection"] = samepage_sources::project(&self.host.view(), &facts)
                    .map_err(|e| e.to_string())?;
                source["g8_check"] = check;
            }
        }
        if matches!(
            invoked.action.as_str(),
            "source_read" | "source_list" | "sources_read"
        ) {
            (self.emit_event)("samepage.capability-used", json!({"id":"sources"}));
        }
        let lane_result = if matches!(invoked.action.as_str(), "lane_claim" | "lanes_read") {
            let lanes = self.lanes.as_ref().ok_or("lanes unavailable")?;
            let source = self.sources.as_ref().ok_or("sources unavailable")?;
            let result = self.lanes_inspect(lanes, source, &self.host.view())?;
            (self.emit_event)("samepage.capability-used", json!({"id":"lanes"}));
            Some(result)
        } else {
            None
        };
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
        let mut report = report;
        if let (Some(report), Some(source)) = (&mut report, &source_result) {
            if let Some(nodes) = report["nodes"].as_array_mut() {
                for node in nodes {
                    let id = node["id"].as_str().unwrap_or_default().to_string();
                    node["tier"] = source["projection"]["cards"][&id].clone();
                }
            }
        }
        let mut readback = report
            .as_ref()
            .and_then(|r| r["readback"].as_str())
            .map(|s| vec![s.to_string()])
            .or_else(||pane_read.map(|text|vec![text]))
            .unwrap_or(invoked.readback);
        let architecture = if invoked.action == "architecture_read" {
            Some(samepage_architecture::inspect(&self.host.view()).map_err(|e| e.to_string())?)
        } else {
            None
        };
        if let Some(text) = source_result
            .as_ref()
            .and_then(|s| s["projection"]["readback"].as_str())
        {
            if source_action {
                readback.push(text.into());
            } else {
                let lines: Vec<_> = text
                    .lines()
                    .filter(|line| touched_cards.iter().any(|id| line.starts_with(&format!("[{id}]"))))
                    .collect();
                if !lines.is_empty() {
                    readback.push(format!("SOURCES\n{}", lines.join("\n")));
                }
            }
        }
        if let Some(text) = lane_result
            .as_ref()
            .and_then(|r| r["projection"]["readback"].as_str())
        {
            readback.push(text.into());
        }
        if invoked.action == "source_read" {
            if let Some(excerpt) = &source_result {
                let excerpt = serde_json::from_value::<samepage_sources::Excerpt>(excerpt.clone())
                    .map_err(|e| e.to_string())?;
                readback.push(
                    samepage_sources::viewer(&excerpt).map_err(|e| e.to_string())?["readback"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                );
            }
        }
        let walkthrough = if invoked.action.starts_with("walkthrough_") {
            Some(samepage_explain::read(&self.host.view(), if invoked.action == "walkthrough_read" && !flow_id.is_empty() { Some(&flow_id) } else { None }).map_err(|e| e.to_string())?)
        } else { None };
        let mut reply = json!({
            "walkthrough": walkthrough,
            "architecture": architecture,
            "action": invoked.action,
            "readback": readback,
            "review": report,
            "sources": if source_action { source_result } else { None },
            "lanes": lane_result,
            "cement": cement_result,
            "validation": validation,
            "ids": invoked.ids,
            "touched": invoked.commit.as_ref().map(|c| c.created.iter().chain(&c.modified).chain(&c.deleted).collect::<Vec<_>>()),
            "revision": self.host.revision(),
        });
        // Absent sections are omitted rather than sent as null.
        if let Some(fields) = reply.as_object_mut() {
            fields.retain(|_, value| !value.is_null());
        }
        let result = serde_json::to_string(&reply).map_err(|error| error.to_string());
        drop(_operation);
        if invoked.commit.is_some() && (matches!(invoked.action.as_str(), "walkthrough_define" | "walkthrough_advance") || (invoked.action == "walkthrough_control" && reenter)) {
            if let Err(error) = crate::host::explain::attend(&self.host.view(), &flow_id, surface_actor, self.input.attention().as_deref()) {
                // Attention is ephemeral and occurs after the durable commit.
                // Report its failure without calling the accepted write refused.
                return result.and_then(|raw| {
                    let mut value: Value = serde_json::from_str(&raw).map_err(|e|e.to_string())?;
                    value["attention_error"] = json!(error);
                    serde_json::to_string(&value).map_err(|e|e.to_string())
                });
            }
        }
        result
    }
}

impl SamePageState {
    /// The page as one plain-text read-back. Structured facts stay behind each
    /// capability's own read tool so this reply stays small as a page grows.
    fn page_text(
        &self,
        actor: &Actor,
        trace: Option<&crate::host::trace::TraceHost>,
    ) -> Result<String, String> {
        let view = self.host.view();
        let mut out = format!("PAGE revision {}\n", self.host.revision());
        let section = |out: &mut String, header: &str, body: &str| {
            let body = body.trim();
            if body.is_empty() {
                return;
            }
            out.push('\n');
            if !body.starts_with(header) {
                out.push_str(header);
                out.push('\n');
            }
            out.push_str(body);
            out.push('\n');
        };
        for (id, action, header) in [
            ("map", "map_read", "MAP"),
            ("diagrams", "diagram_read", "DIAGRAMS"),
            ("architecture", "architecture_read", "ARCHITECTURE"),
            ("sketch", "sketch_read", "SKETCH"),
        ] {
            if self.host.composition().descriptor(id).is_some() {
                let result = self
                    .host
                    .invoke(actor, action, json!({}))
                    .map_err(|e| e.to_string())?;
                section(&mut out, header, &result.readback.join("\n"));
            }
        }
        if let Some(review) = &self.review {
            let report = review.read(&view, &actor.participant)?;
            section(&mut out, "REVIEW", report["readback"].as_str().unwrap_or_default());
        }
        if let Some(panes) = &self.panes {
            if view.records().any(|r| r.kind == "pane") {
                let text = panes.read(&view, &self.host.journal(), &actor.participant)?;
                section(&mut out, "PANES", &text);
            }
        }
        if let Some(sources) = &self.sources {
            let mut result = sources.action("sources_read", &json!({}), &view)?;
            if let Some(cement) = &self.cement {
                let (gates, _) = cement.gates(&view)?;
                result["facts"]["gates"] = serde_json::to_value(gates).map_err(|e| e.to_string())?;
                let facts = serde_json::from_value(result["facts"].clone()).map_err(|e| e.to_string())?;
                result["projection"] = samepage_sources::project(&view, &facts).map_err(|e| e.to_string())?;
            }
            section(&mut out, "SOURCES", result["projection"]["readback"].as_str().unwrap_or_default());
            if let Some(lanes) = &self.lanes {
                let result = self.lanes_inspect(lanes, sources, &view)?;
                section(&mut out, "LANES", result["projection"]["readback"].as_str().unwrap_or_default());
            }
        }
        if let Some(cement) = &self.cement {
            let projection = samepage_cement::Cement.project(&view).map_err(|e| e.to_string())?;
            section(&mut out, "CEMENT", &cement_text(&projection, &cement.history()?));
        }
        if self.host.composition().descriptor("explain").is_some() {
            let result = samepage_explain::read(&view, None).map_err(|e| e.to_string())?;
            if result["flows"].as_array().is_some_and(|f| !f.is_empty()) {
                section(&mut out, "EXPLANATION FLOWS", result["readback"].as_str().unwrap_or_default());
            }
        }
        let board = self.board.lock().map_err(|_| "board binding lock poisoned")?.clone();
        if let Some(board) = board {
            if board.revision() == 0 {
                section(&mut out, "BOARD", "BOARD: empty. board_read lists its vocabulary.");
            } else {
                section(&mut out, "BOARD", &board.read(&actor.participant)?);
            }
        }
        if let Some(trace) = trace {
            let result: Value = serde_json::from_str(&trace.invoke("trace_read", json!({}))?)
                .map_err(|e| e.to_string())?;
            let ok = &result["ok"];
            section(
                &mut out,
                "TRACE",
                &format!(
                    "TRACE: {} spans held, {} dropped; trace_read returns them.",
                    ok["records"].as_array().map_or(0, Vec::len),
                    ok["dropped"].as_u64().unwrap_or(0)
                ),
            );
        }
        section(&mut out, "VALIDATION", &validation_text(&self.validation.report(&view)?));
        self.input.acknowledge(&actor.participant, self.host.journal().seq);
        Ok(out)
    }

    /// What woke a waiter: one line per change another participant made
    /// since `since`, then the read-back of each capability whose records
    /// changed (cards and links, review, drawings, walkthroughs).
    pub(crate) fn wake_text(&self, reader: &str, since: u64) -> Result<String, String> {
        let view = self.host.view();
        let journal = self.host.journal();
        let entries: Vec<_> = journal
            .recent
            .iter()
            .filter(|e| e.seq > since && e.actor.participant != reader)
            .collect();
        let name = |actor: &samepage_doc::Actor| {
            actor.label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| {
                if actor.caller == Caller::Human { "a person".into() } else { "an agent".into() }
            })
        };
        let describe = |id: &String| match view.get(id) {
            Some(r) => {
                let words = ["label", "text", "title"]
                    .iter()
                    .find_map(|k| r.props.get(*k).and_then(Scalar::as_str).filter(|s| !s.is_empty()))
                    .unwrap_or("");
                let about = ["about", "target"]
                    .iter()
                    .find_map(|k| r.props.get(*k).and_then(Scalar::as_str))
                    .and_then(|t| view.get(t))
                    .and_then(|t| t.props.get("label").and_then(Scalar::as_str))
                    .map(|l| format!(" on {l:?}"))
                    .unwrap_or_default();
                format!("{} {:?}{about} [{id}]", r.kind, words.chars().take(80).collect::<String>())
            }
            None => format!("removed [{id}]"),
        };
        let mut kinds = std::collections::BTreeSet::new();
        let mut out = String::new();
        for entry in entries.iter().rev().take(8).rev() {
            let ids: Vec<_> = entry.created.iter().chain(&entry.modified).chain(&entry.deleted).collect();
            for id in &ids {
                if let Some(r) = view.get(id) {
                    kinds.insert(r.kind.clone());
                } else {
                    kinds.insert("node".into());
                }
            }
            let what: Vec<_> = ids.iter().take(3).map(|id| describe(id)).collect();
            out.push_str(&format!(
                "woken by: {} {}{} by {}\n",
                entry.action.as_deref().unwrap_or("edit"),
                what.join(", "),
                if ids.len() > 3 { format!(" and {} more", ids.len() - 3) } else { String::new() },
                name(&entry.actor)
            ));
        }
        if entries.len() > 8 {
            out.push_str(&format!("({} earlier changes not listed)\n", entries.len() - 8));
        }
        let touches = |set: &[&str]| kinds.iter().any(|k| set.contains(&k.as_str()));
        if touches(&["mark", "claim", "understanding-context"]) {
            if let Some(review) = &self.review {
                let report = review.read(&view, reader)?;
                out.push('\n');
                out.push_str(report["readback"].as_str().unwrap_or_default());
            }
        }
        if touches(&["shape"]) && self.host.composition().descriptor("sketch").is_some() {
            let read = samepage_sketch::Sketch
                .plan(&samepage_capability::Call { action: "sketch_read".into(), args: json!({}) }, &view)
                .map_err(|e| e.to_string())?;
            out.push('\n');
            out.push_str(&read.readback);
        }
        if touches(&["explanation-flow"]) && self.host.composition().descriptor("explain").is_some() {
            let read = samepage_explain::read(&view, None).map_err(|e| e.to_string())?;
            out.push('\n');
            out.push_str(read["readback"].as_str().unwrap_or_default());
        }
        if touches(&["node", "edge", "constraint", "constraint-member", "binding"]) || out.lines().all(|l| l.starts_with("woken by") || l.starts_with('(')) {
            let read = Map
                .plan(&samepage_capability::Call { action: "map_read".into(), args: json!({}) }, &view)
                .map_err(|e| e.to_string())?;
            out.push('\n');
            out.push_str(&read.readback);
        }
        self.input.acknowledge(reader, journal.seq);
        Ok(out)
    }

    /// Lane facts reconciled with cement removals, shared by every lane read.
    fn lanes_inspect(
        &self,
        lanes: &crate::host::lanes::LanesHost,
        sources: &crate::host::sources::SourcesHost,
        view: &samepage_capability::View,
    ) -> Result<Value, String> {
        let mut result = lanes.inspect(view, sources.facts(view)?)?;
        if let Some(cement) = self.cement.as_ref().filter(|_| {
            result["facts"]["registry"]["removals"].as_object().is_some_and(|r| {
                r.values().any(|removal| removal["obligation_id"].as_str().is_some_and(|id| !id.is_empty()))
            })
        }) {
            let check = cement.check()?;
            result["facts"]["clean"] = json!(check["obligations"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|o| o["status"] == "passed")
                .map(|o| o["id"].clone())
                .collect::<Vec<_>>());
            let facts = serde_json::from_value(result["facts"].clone()).map_err(|e| e.to_string())?;
            result["projection"] = samepage_lanes::reconcile(view, &facts);
        }
        Ok(result)
    }
}

/// The records a journaled action touched: what its commit created, changed
/// or removed, else the record ids its arguments named.
fn activity_targets(args: &Value, result: &str) -> Vec<String> {
    let mut found: Vec<&Value> = Vec::new();
    let reply = serde_json::from_str::<Value>(result).unwrap_or(Value::Null);
    for key in ["touched", "applied_ids"] {
        found.extend(reply[key].as_array().into_iter().flatten());
    }
    if found.is_empty() {
        for key in ["id", "about", "from", "to", "target", "card", "draft_id", "lane_id", "flow_id"] {
            found.push(&args[key]);
        }
        found.extend(args["ids"].as_array().into_iter().flatten());
    }
    let mut out: Vec<String> = Vec::new();
    for id in found.into_iter().filter_map(Value::as_str).filter(|id| !id.is_empty()) {
        if !out.iter().any(|seen| seen == id) {
            out.push(id.to_string());
        }
    }
    out
}

/// Decisions as text: mode, drafts, cemented coverage and published records.
fn cement_text(projection: &Value, history: &Value) -> String {
    let mut out = format!(
        "CEMENT: mode {}.",
        projection["mode"].as_str().unwrap_or("learning")
    );
    let empty = Vec::new();
    let drafts = projection["drafts"].as_array().unwrap_or(&empty);
    if drafts.is_empty() {
        out.push_str(" No decision drafts.");
    }
    for draft in drafts {
        let ids: Vec<_> = draft["input"]["ids"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(Value::as_str)
            .collect();
        out.push_str(&format!(
            "\n- draft [{}] {:?} for [{}] by {}",
            draft["id"].as_str().unwrap_or_default(),
            draft["input"]["title"].as_str().unwrap_or_default(),
            ids.join(", "),
            draft["created_by"].as_str().filter(|s| !s.is_empty()).unwrap_or("someone")
        ));
    }
    for covered in projection["coverage"].as_array().unwrap_or(&empty) {
        out.push_str(&format!(
            "\n- cemented [{}] {}",
            covered["id"].as_str().unwrap_or_default(),
            if covered["current"] == true { "current" } else { "STALE: the card changed after cement" }
        ));
    }
    for entry in history.as_array().unwrap_or(&empty) {
        let entry = &entry["entry"];
        out.push_str(&format!(
            "\n- decision {:?} ({}) at {}",
            entry["title"].as_str().unwrap_or_default(),
            entry["status"].as_str().unwrap_or_default(),
            entry["path"].as_str().unwrap_or_default()
        ));
    }
    out
}

/// A stable name for a set of page problems: code and ids, or code and
/// message when a browser error names no card. Measured numbers in messages
/// change between reports and do not make a new problem.
pub(crate) fn validation_signature(report: &Value) -> String {
    let mut keys: Vec<String> = report["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|error| {
            let ids: Vec<_> = error["ids"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if ids.is_empty() {
                format!("{}:{}", error["code"].as_str().unwrap_or_default(), error["message"].as_str().unwrap_or_default())
            } else {
                format!("{}:{}", error["code"].as_str().unwrap_or_default(), ids.join(","))
            }
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys.join("\n")
}

/// Page validation as text: status and one line per problem with its ids.
pub(crate) fn validation_text(report: &Value) -> String {
    let status = report["status"].as_str().unwrap_or("pending");
    let empty = Vec::new();
    let errors = report["errors"].as_array().unwrap_or(&empty);
    let mut out = match status {
        "pending" => "VALIDATION: pending; no open browser has measured this revision yet.".to_string(),
        "failed" => format!("VALIDATION: failed, {} problem(s).", errors.len()),
        other => format!("VALIDATION: {other}."),
    };
    for error in errors {
        let ids: Vec<_> = error["ids"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(Value::as_str)
            .collect();
        out.push_str(&format!(
            "\n- {}: {}{}",
            error["code"].as_str().unwrap_or("error"),
            error["message"].as_str().unwrap_or_default(),
            if ids.is_empty() { String::new() } else { format!(" [{}]", ids.join(", ")) }
        ));
    }
    out
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
    fn activity_dropped(&self) -> u64 { samepage_capability::activity::dropped() }
    fn activity_state_revision(&self) -> Result<Vec<ag_ui_surface::ActivityStateRevision>, String> {
        Ok(vec![ag_ui_surface::ActivityStateRevision::new("document", self.host.revision())])
    }
    fn activity_targets(&self, _action: &str, args: &Value, result: &str) -> Vec<String> {
        activity_targets(args, result)
    }
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
            "explain": if self.host.composition().descriptor("explain").is_some() { Some(samepage_explain::read(&view,None).map_err(|e|e.to_string())?) } else { None },
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
        let with_panes = artifacts.panes.is_some();
        let with_sources = artifacts.sources.is_some();
        let with_lanes = artifacts.lanes.is_some();
        let with_cement = artifacts.cement.is_some();
        let with_explain = artifacts.explain.is_some();
        let state = if with_diagrams || with_review || with_architecture || with_sketch || with_panes || with_sources || with_lanes || with_cement || with_explain {
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
                with_panes,
                with_sources,
                with_lanes,
                with_cement,
                with_explain,
            )?
        } else {
            SamePageState::open(path, transport)?
        };
        *state.board.lock().map_err(|_|"board binding lock poisoned")?=board.clone();
        let trace_host = artifacts.trace.as_ref().map(|_| crate::host::trace::TraceHost::new());
        samepage_capability::activity::install_sink(trace_host.clone().map(|h| h as Arc<dyn samepage_capability::activity::ActivitySink>));
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
        if with_panes { declarations.extend(samepage_panes::descriptor().actions.into_iter().filter(|a|a.name!="panes_await"&&a.name!="panes_present")); }
        if with_sources {
            declarations.extend(samepage_sources::descriptor().actions);
        }
        if with_lanes {
            declarations.extend(samepage_lanes::descriptor().actions);
        }
        if with_cement {
            declarations.extend(samepage_cement::descriptor().actions);
        }
        if with_explain { declarations.extend(samepage_explain::descriptor().actions); }
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
        let page_state = state.clone();
        let page_trace = trace_host.clone();
        tools.push(
            ActionDef::new(
                "page_read",
                "Start here. Read the whole shared page as plain text: cards with notes, colours and bound files, links, diagrams, drawings, claims and marks, source tiers, lanes, decisions, walkthroughs, the board and page problems. One section per capability; each section's own read tool (map_read, review_read, sketch_read, sources_read, lanes_read, board_read, trace_read) returns its structured form.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
                move |_| {
                    let state = page_state.clone();
                    let trace = page_trace.clone();
                    SurfaceEffect::Query(Box::new(move |_| {
                        let _operation = state.durable_operation()?;
                        let actor = map_actor(&current_actor().ok_or("authenticated reader missing")?)?;
                        state.page_text(&actor, trace.as_deref())
                    }))
                },
            )
            .audience(ActionAudience::Agent)
            .alias("atlas_read"),
        );
        if let Some(trace_host) = trace_host {
            for decl in samepage_trace::descriptor().actions {
                let host = trace_host.clone(); let name = decl.name.clone();
                tools.push(ActionDef::new(decl.name, decl.description, decl.input_schema, move |args| {
                    let args = args.clone(); let host = host.clone(); let name = name.clone();
                    SurfaceEffect::Query(Box::new(move |_| host.invoke(&name, args)))
                }));
            }
        }
        if with_panes {
            let present_state=state.clone();
            let mut present=ActionDef::new("panes_present","Resolve current pane source and workspace facts for the human renderer.",json!({"type":"object","required":[],"properties":{},"additionalProperties":false}),move |_| {
                let state=present_state.clone();SurfaceEffect::Query(Box::new(move |_| {
                    let _operation=state.durable_operation()?;
                    state.panes.as_ref().ok_or("panes is unavailable")?.inspect(&state.host.view(),&state.host.journal()).map(|v|v.to_string())
                }))
            });present.audience=ActionAudience::Human;tools.push(present);
            let wait_state=state.clone();
            let mut wait=ActionDef::new("panes_await","Wait for unread pane changes from other participants.",json!({"type":"object","required":[],"properties":{"seconds":{"type":"integer","minimum":0,"maximum":60}},"additionalProperties":false}),move |args| {
                let state=wait_state.clone();let seconds=args["seconds"].as_u64().unwrap_or(30);
                let actor=current_actor().and_then(|a|map_actor(&a).ok());
                SurfaceEffect::AsyncQuery(Box::pin(async move {
                    let actor=actor.ok_or("authenticated actor missing")?;
                    (state.emit_event)("samepage.capability-used",json!({"id":"panes","tool":"panes_await"}));
                    let panes=state.panes.as_ref().ok_or("panes is unavailable")?;
                    let deadline=tokio::time::Instant::now()+Duration::from_secs(seconds);
                    loop {
                        let notified=panes.changed.notified();tokio::pin!(notified);notified.as_mut().enable();
                        let check=state.clone();let participant=actor.participant.clone();
                        let unread=tokio::task::spawn_blocking(move || {
                            let _operation=check.durable_operation()?;
                            check.panes.as_ref().unwrap().unread_from_others(&check.host.view(),&check.host.journal(),&participant)
                        }).await.map_err(|e|e.to_string())??;
                        if unread {break;}
                        if tokio::time::timeout_at(deadline,notified).await.is_err(){return Ok("The panes are quiet; no unread changes from another participant.".into());}
                    }
                    let state=state.clone();tokio::task::spawn_blocking(move || {let _operation=state.durable_operation()?;state.panes.as_ref().unwrap().read(&state.host.view(),&state.host.journal(),&actor.participant)}).await.map_err(|e|e.to_string())?
                }))
            }).alias("await_room");wait.audience=ActionAudience::Agent;tools.push(wait);
        }
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
    fn focus_events(&self) -> &[&str] { &["panes.pane.focused"] }
    fn resolve_focus(&self,event:&str,id:&str)->Result<Option<String>,String> {
        if event!="panes.pane.focused" {return Ok(None)}
        let _operation=self.state.durable_operation()?;
        Ok(self.state.host.view().records().find(|r|r.kind=="pane"&&r.props.get("pane_id").and_then(Scalar::as_str)==Some(id)).map(|r|format!("Pane [{}] {}",id,r.props.get("title").and_then(Scalar::as_str).unwrap_or(""))))
    }

    fn routes(&self)->Vec<ag_ui_surface::RouteDef> {
        let mut routes=Vec::new();
        if self.state.panes.is_some(){
        let state=self.state.clone();
        routes.push(ag_ui_surface::RouteDef{method:ag_ui_surface::HttpMethod::Get,path:"/panes/html",handler:Box::new(move |request:ag_ui_surface::RouteRequest|{
            let state=state.clone();Box::pin(async move {
                let markup=(||->Result<String,String>{
                    let _operation=state.durable_operation()?;let view=state.host.view();
                    let slug=request.query.get("pane").map(String::as_str).unwrap_or("");
                    let record=view.records().find(|r|r.kind=="pane"&&r.props.get("pane_id").and_then(|s|s.as_str())==Some(slug)).ok_or("no such pane")?;
                    let tree:samepage_panes::view::Node=serde_json::from_str(record.props.get("tree").and_then(|s|s.as_str()).ok_or("no tree")?).map_err(|e|e.to_string())?;
                    let mut html=vec![];samepage_panes::view::collect_html(&tree,&mut html);
                    let index=request.query.get("index").and_then(|s|s.parse::<usize>().ok()).unwrap_or(0);
                    Ok(format!("{}{}",crate::host::panes_html::POINTER_BRIDGE_DOC,html.get(index).ok_or("no such html node")?))
                })();
                match markup {Ok(body)=>ag_ui_surface::RouteResponse{status:200,body:ag_ui_surface::RouteBody::Html(body)},Err(_)=>ag_ui_surface::RouteResponse{status:404,body:ag_ui_surface::RouteBody::Html("<!doctype html><p>panes: no such html node</p>".into())}}
            })
        })});
        }
        if let Some(board)=&self.board {
        let board=board.clone();
        routes.push(ag_ui_surface::RouteDef{method:ag_ui_surface::HttpMethod::Get,path:"/board/state",handler:Box::new(move |_|{
            let board=board.clone(); Box::pin(async move { ag_ui_surface::RouteResponse::json(200,board.snapshot()) })
        })});
        routes.push(ag_ui_surface::RouteDef{method:ag_ui_surface::HttpMethod::Get,path:"/board/primitives",handler:Box::new(move |_|Box::pin(async move { ag_ui_surface::RouteResponse::json(200,samepage_board::assertion::catalog()) }))});
        }
        routes
    }
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
        if let Some(artifact) = &self.artifacts.sources {
            let mut m = ClientModule::lazy(
                "sources",
                samepage_sources::VERSION,
                artifact.entry.url.clone(),
                "sources",
            )
            .verified_artifact(artifact.clone())
            .optional()
            .capability("samepage.sources");
            for a in samepage_sources::descriptor().actions {
                m = m.action(a.name);
            }
            modules.push(m);
        }
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
        if let Some(artifact) = &self.artifacts.lanes {
            let mut module = ClientModule::lazy(
                "lanes",
                samepage_lanes::VERSION,
                artifact.entry.url.clone(),
                "lanes",
            )
            .verified_artifact(artifact.clone())
            .optional()
            .capability("samepage.lanes");
            for action in samepage_lanes::descriptor().actions {
                module = module.action(action.name);
            }
            modules.push(module);
        }
        if let Some(artifact) = &self.artifacts.cement {
            let mut module = ClientModule::lazy(
                "cement",
                samepage_cement::VERSION,
                artifact.entry.url.clone(),
                "cement",
            )
            .verified_artifact(artifact.clone())
            .optional()
            .capability("samepage.cement");
            for action in samepage_cement::descriptor().actions {
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
        if let Some(artifact) = &self.artifacts.trace {
            let mut module = ClientModule::lazy("trace",samepage_trace::VERSION,artifact.entry.url.clone(),"trace")
                .verified_artifact(artifact.clone()).optional().capability("samepage.trace");
            for action in samepage_trace::descriptor().actions { module = module.action(action.name); }
            modules.push(module);
        }
        if let Some(artifact)=&self.artifacts.panes {
            let mut module=ClientModule::lazy("panes",samepage_panes::VERSION,artifact.entry.url.clone(),"panes").verified_artifact(artifact.clone()).optional().capability("samepage.panes");
            for action in samepage_panes::descriptor().actions {if action.audience!=samepage_capability::Audience::Agent {module=module.action(action.name);}}
            modules.push(module);
        }
        if let Some(artifact) = &self.artifacts.explain {
            let mut module = ClientModule::lazy("explain", samepage_explain::VERSION, artifact.entry.url.clone(), "explain")
                .verified_artifact(artifact.clone()).optional().capability("samepage.explain");
            for action in samepage_explain::descriptor().actions.into_iter().filter(|a| a.audience != samepage_capability::Audience::Agent) { module = module.action(action.name); }
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
