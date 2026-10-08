//! Native board adapter: authenticated callers, persistence and notifications.
//! This store never participates in the document's CRDT transaction pipeline.
use ag_ui_surface::{ActionAudience, ActionDef, Actor, Caller, Effect};
use samepage_board::{advisory::CementBundle, Board, Context};
use samepage_capability::{Audience, Call};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

pub const CHANGED_EVENT: &str = "board.changed";
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;
struct Session {
    board: Board,
    agents: BTreeMap<String, u64>,
    humans: BTreeMap<String, u64>,
}
pub struct BoardHost {
    state: Mutex<Session>,
    path: PathBuf,
    emit: Emit,
    changed: Notify,
}
impl BoardHost {
    pub fn open(path: PathBuf, emit: Emit) -> Result<Arc<Self>, String> {
        let board = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("saved board could not be loaded: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Board::default(),
            Err(e) => return Err(format!("could not read {}: {e}", path.display())),
        };
        Ok(Arc::new(Self {
            state: Mutex::new(Session {
                board,
                agents: BTreeMap::new(),
                humans: BTreeMap::new(),
            }),
            path,
            emit,
            changed: Notify::new(),
        }))
    }
    pub fn revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .board
            .revision()
    }
    pub fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        json!({"board":state.board,"revision":state.board.revision()})
    }
    pub fn peek(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .board
            .render(0)
    }
    pub fn resolve(&self, id: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .board
            .get(id)
            .map(|e| {
                format!(
                    "{} [{id}, by {}]",
                    samepage_board::assertion::describe(&e.assertion),
                    e.author.word()
                )
            })
    }
    pub fn changed(&self) -> &Notify {
        &self.changed
    }
    pub fn read(&self, participant: &str) -> Result<String, String> {
        let mut state = self.state.lock().map_err(|_| "board lock poisoned")?;
        let since = state.agents.get(participant).copied().unwrap_or(0);
        let read = state.board.render_for(since, Some(participant));
        let revision = state.board.revision();
        state.agents.insert(participant.into(), revision);
        Ok(read)
    }
    pub fn invoke_as(
        &self,
        actor: &Actor,
        name: &str,
        args: Value,
        document_revision: &str,
    ) -> Result<String, String> {
        let name =
            samepage_board::preferred(name).ok_or_else(|| format!("board has no action {name}"))?;
        let declaration = samepage_board::descriptor()
            .actions
            .into_iter()
            .find(|a| a.name == name)
            .unwrap();
        let audience = match declaration.audience {
            Audience::Human => ActionAudience::Human,
            Audience::Agent => ActionAudience::Agent,
            Audience::Both => ActionAudience::Both,
        };
        if !actor.caller.may_call(audience) {
            return Err(format!(
                "{name}: caller {:?} cannot invoke {:?} action",
                actor.caller, audience
            ));
        }
        let participant = actor.participant_id.as_deref().unwrap_or("agent");
        if actor.caller == Caller::Human && actor.participant_id.is_none() {
            return Err("board requires a host-issued human identity".into());
        }
        match name.as_str() {
            "board_read" => return self.read(participant),
            "board_revision" => return Ok(self.revision().to_string()),
            "board_snapshot" => {
                let state = self.state.lock().map_err(|_| "board lock poisoned")?;
                let seen_at = state.humans.get(participant).copied().unwrap_or(0);
                return Ok(json!({"board":state.board,"seen_at":seen_at,"readback":state.board.render_for(seen_at,Some(participant))}).to_string());
            }
            "board_seen" => {
                let mut state = self.state.lock().map_err(|_| "board lock poisoned")?;
                let revision = state.board.revision();
                state.humans.insert(participant.into(), revision);
                return Ok(json!({"seen_at":revision}).to_string());
            }
            "board_cement_preview" | "board_cement" => {
                let state = self.state.lock().map_err(|_| "board lock poisoned")?;
                let bundle = samepage_board::advisory::propose_at(
                    &state.board,
                    document_revision,
                    &now_iso()?,
                )?;
                if name == "board_cement_preview" {
                    return bundle.preview();
                }
                let directory = args["directory"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("directory is required")?;
                let paths = publish_bundle(&bundle, Path::new(directory))?;
                return Ok(format!(
                    "cemented {} and {} at document revision {document_revision}; advisory only",
                    paths[0].display(),
                    paths[1].display()
                ));
            }
            _ => {}
        }
        let author = if actor.caller == Caller::Human {
            samepage_board::Author::You
        } else {
            actor
                .label
                .clone()
                .map(samepage_board::Author::Named)
                .unwrap_or(samepage_board::Author::Agent)
        };
        let ctx = Context {
            author,
            participant: participant.into(),
            timestamp: now_iso()?,
        };
        let mut state = self.state.lock().map_err(|_| "board lock poisoned")?;
        let mut candidate = state.board.clone();
        if let Some(label) = &actor.label {
            candidate.bind_participant(participant, label);
        }
        let outcome = samepage_board::transition_at(&candidate, &Call { action: name, args }, &ctx)
            .map_err(|e| e.to_string())?;
        if outcome.board.revision() != state.board.revision() {
            // Prefix publication happens even when outcome.error is present.
            // A filesystem error refuses publication visibly and leaves memory
            // at the durable state, unlike the old warning-only save path.
            save_board(&self.path, &outcome.board)?;
            state.board = outcome.board;
            (self.emit)(
                CHANGED_EVENT,
                json!({"board":state.board,"revision":state.board.revision()}),
            );
            self.changed.notify_waiters();
        }
        match outcome.error {
            Some(error) => Err(error),
            None => Ok(outcome.readback),
        }
    }
    pub async fn wait_since(
        self: Arc<Self>,
        participant: String,
        seconds: u64,
        revision: u64,
    ) -> Result<String, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.revision() != revision {
                return self.read(&participant);
            }
            tokio::select! {_ = changed.as_mut()=>{},_ = tokio::time::sleep_until(deadline)=>return Ok(format!("Waited {seconds}s and the board did not change. Call await_board again to keep waiting."))}
        }
    }
    pub fn actions(
        self: &Arc<Self>,
        revision: Arc<dyn Fn() -> Result<String, String> + Send + Sync>,
    ) -> Vec<ActionDef> {
        samepage_board::descriptor()
            .actions
            .into_iter()
            .filter(|a| a.name != "await_input")
            .map(|decl| {
                let name = decl.name.clone();
                let state = self.clone();
                let revision = revision.clone();
                let query = decl.effect == samepage_capability::Effect::Query;
                let mut action = ActionDef::new(
                    decl.name,
                    decl.description,
                    decl.input_schema,
                    move |args| {
                        let actor = match ag_ui_surface::current_actor() {
                            Some(actor) => actor,
                            None => {
                                return Effect::Reject(
                                    "authenticated board caller is missing".into(),
                                )
                            }
                        };
                        let args = args.clone();
                        let state = state.clone();
                        let revision = revision.clone();
                        let name = name.clone();
                        if name == "await_board" {
                            let seconds = args["seconds"].as_u64().unwrap_or(60);
                            let participant =
                                actor.participant_id.unwrap_or_else(|| "agent".into());
                            let starting = state.revision();
                            return Effect::AsyncQuery(Box::pin(state.wait_since(
                                participant,
                                seconds,
                                starting,
                            )));
                        }
                        if query {
                            Effect::Query(Box::new(move |_| {
                                state.invoke_as(&actor, &name, args, &revision()?)
                            }))
                        } else {
                            Effect::Mutate(Box::new(move |_| {
                                state.invoke_as(&actor, &name, args, &revision()?).map(Some)
                            }))
                        }
                    },
                );
                action.audience = match decl.audience {
                    Audience::Human => ActionAudience::Human,
                    Audience::Agent => ActionAudience::Agent,
                    Audience::Both => ActionAudience::Both,
                };
                for alias in decl.aliases {
                    action = action.alias(alias);
                }
                if action.name == "await_board" {
                    action = action.wake_on_chat();
                }
                action
            })
            .collect()
    }
}
fn save_board(path: &Path, board: &Board) -> Result<(), String> {
    let parent = path.parent().ok_or("board path needs a parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let staging = parent.join(format!(".board-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&staging)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(board).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&staging, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(staging);
    }
    result.map_err(|e: String| format!("board persistence failed: {e}"))
}
// Ported publication contract: create a complete pair in a sibling staging
// directory and rename once. All settlement checks happen before any I/O.
pub fn publish_bundle(bundle: &CementBundle, directory: &Path) -> Result<[PathBuf; 2], String> {
    let name = directory
        .file_name()
        .ok_or("cement directory must name a new directory")?;
    if directory.try_exists().map_err(|e| e.to_string())? {
        return Err("cement directory already exists; name a new directory".into());
    }
    let parent = directory
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let staging = parent.join(format!(
        ".{}.board-cement-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    let result = (|| {
        for (name, bytes) in bundle.files() {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(staging.join(name))
                .map_err(|e| e.to_string())?;
            file.write_all(bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        fs::rename(&staging, directory).map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        let cleanup = fs::remove_dir_all(&staging);
        return Err(format!("{error}; staging cleanup: {cleanup:?}"));
    }
    Ok([
        directory.join(samepage_board::advisory::OBLIGATIONS_FILE),
        directory.join(samepage_board::advisory::RECEIPT_FILE),
    ])
}

/// Current UTC time as an RFC 3339 string.
pub fn now_iso() -> Result<String, String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_secs();
    Ok(unix_seconds_iso(secs))
}

fn unix_seconds_iso(secs: u64) -> String {
    // Howard Hinnant's civil_from_days.
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60
    )
}

/// Native signals shared by document commits and the combined input wait.
/// These cursors describe attention, and never go into either stored document.
#[derive(Default)]
pub struct InputSignals {
    pub changed: Notify,
    seen: Mutex<BTreeMap<String, u64>>,
    attention: Mutex<Option<Arc<ag_ui_surface::SemanticTargetService>>>,
}
impl InputSignals {
    pub fn seen(&self, participant: &str) -> u64 {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(participant)
            .copied()
            .unwrap_or(0)
    }
    pub fn acknowledge(&self, participant: &str, seq: u64) {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(participant.into(), seq);
    }
    pub fn bind_attention(&self, service: Arc<ag_ui_surface::SemanticTargetService>) {
        *self.attention.lock().unwrap_or_else(|p| p.into_inner()) = Some(service);
    }
    pub fn attention(&self) -> Option<Arc<ag_ui_surface::SemanticTargetService>> {
        self.attention
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}
