//! The assertion board, as an extension of the same host that holds the map.
//!
//! The atlas answers "what is this project made of". This answers "what are we
//! saying about it" — claims, questions, choices, the relations between them,
//! and a decision when a fork closes. They are different artifacts, so they are
//! different extensions; they are the *same conversation*, so they are one
//! process, one action catalog, one event stream, and one `/mcp`.
//!
//! That last part is the whole point of the port. A board that ran as its own
//! server had its own everything, and the only thing that could drive it was
//! whatever happened to be holding a curl command. Composed here, a terminal
//! agent that attaches to the atlas gets the board in the same `tools/list` it
//! already had, and a human watching `/events` sees both halves interleaved in
//! the order they actually happened.
//!
//! **Audience is the enforcement.** `board_assert` is the agent's and signs the
//! host-stamped participant label when one attached, falling back to `agent`;
//! `board_compose` is the human's and always signs `you`; `board_mark`,
//! `board_unmark`, `board_seen`, `board_clear` and `atlas_cement` have no agent
//! twin at all. `atlas_cement_propose` is an agent query and cannot write. The
//! check happens in the runtime's dispatcher before the arguments are even
//! parsed, so "who wrote this" and "who cemented this" are facts about which
//! audience the call arrived through — not fields a caller can fill in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_ui_surface::{
    ActionRouteDef, ActionRouteRequest, ClientModule, Effect, Extension, HttpMethod, RouteDef,
    RouteRequest, RouteResponse, SemanticTarget, SemanticTargetRef, StateBacking, StateSnapshot,
    SurfaceState, ToolDef, Transport,
};
use parking_lot::Mutex;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use tokio::sync::Notify;

use crate::assertion::{self, Assertion, Author, Board, Entry, Mark};
use crate::atlas::AtlasState;
use crate::cement;

const CHANGED_EVENT: &str = "board.changed";
const FOCUS_EVENT: &str = "board.focus";
const DEFAULT_WAIT_SECONDS: u64 = 60;
const MAX_WAIT_SECONDS: u64 = 600;

pub struct BoardState {
    board: Mutex<Board>,
    transport: Transport,
    state_path: PathBuf,
    atlas: Arc<AtlasState>,
    /// The display name of the agent whose action is currently being
    /// dispatched. It is session attribution only and is never persisted as
    /// authority.
    actor_label: Mutex<Option<String>>,
    /// Document changes wake parked agents without polling. Attention and
    /// ordinary reads never notify this channel because neither changes the
    /// shared board.
    changed: Notify,
}

impl BoardState {
    /// Sign an agent write with its announced room label when one exists,
    /// preserving the generic byline for anonymous callers.
    fn agent_author(&self) -> Author {
        match self.actor_label.lock().clone() {
            Some(label) => Author::Named(label),
            None => Author::Agent,
        }
    }

    pub fn open(
        transport: Transport,
        state_path: PathBuf,
        atlas: Arc<AtlasState>,
    ) -> Result<Arc<Self>, String> {
        let board = match std::fs::read_to_string(&state_path) {
            Ok(text) => serde_json::from_str::<Board>(&text)
                .map_err(|error| format!("saved board could not be loaded: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Board::default(),
            Err(error) => return Err(format!("could not read {}: {error}", state_path.display())),
        };
        Ok(Arc::new(Self {
            board: Mutex::new(board),
            transport,
            state_path,
            atlas,
            actor_label: Mutex::new(None),
            changed: Notify::new(),
        }))
    }

    /// Persist, then tell every open page. Both happen after the mutation and
    /// while the lock is still held, so a reader can never observe a board the
    /// file and the page disagree about.
    fn published(&self, board: &Board) {
        if let Some(parent) = self.state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(board) {
            Ok(text) => {
                if let Err(error) = std::fs::write(&self.state_path, text) {
                    // A failed save is not a failed write: the board in memory
                    // is real and the page is about to show it. Say so loudly
                    // rather than pretending the action failed.
                    tracing::warn!(path = %self.state_path.display(), %error, "could not save the board");
                }
            }
            Err(error) => tracing::warn!(%error, "could not serialize the board"),
        }
        self.transport.emit(CHANGED_EVENT, view(board));
    }

    /// Publish on any change, including a change made by a call that then
    /// failed. A batch that wrote three assertions and was refused the fourth
    /// really did write three; leaving them out of the file and off the page
    /// because the call returned `Err` would make the surface disagree with
    /// the error message the agent was just handed.
    fn write<T>(&self, body: impl FnOnce(&mut Board) -> Result<T, String>) -> Result<T, String> {
        let mut board = self.board.lock();
        let before = board.revision();
        let outcome = body(&mut board);
        if board.revision() != before {
            self.published(&board);
            self.changed.notify_waiters();
        }
        outcome
    }
}

fn wait_seconds(args: &JsonValue) -> Result<u64, String> {
    let seconds = match args.get("seconds") {
        None => DEFAULT_WAIT_SECONDS,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| "seconds must be an integer".to_string())?,
    };
    if !(1..=MAX_WAIT_SECONDS).contains(&seconds) {
        return Err(format!("seconds must be between 1 and {MAX_WAIT_SECONDS}"));
    }
    Ok(seconds)
}

impl SurfaceState for BoardState {
    fn backing(&self) -> StateBacking {
        // Not a CRDT: there is exactly one authority for an assertion, and two
        // people writing the same claim at the same instant is a conversation
        // to have, not a merge to perform.
        StateBacking::LastWriterWins
    }

    fn describe(&self) -> Result<String, String> {
        Ok(self.board.lock().peek())
    }

    fn snapshot(&self) -> Result<StateSnapshot, String> {
        Ok(StateSnapshot {
            backing: StateBacking::LastWriterWins,
            body: view(&self.board.lock()),
            chrome: None,
        })
    }

    fn activity_state_revision(&self) -> Result<Vec<ag_ui_surface::ActivityStateRevision>, String> {
        Ok(vec![ag_ui_surface::ActivityStateRevision::new(
            "document",
            self.board.lock().revision().to_string(),
        )])
    }

    /// Deixis: the human clicks an assertion and says "this". What comes back
    /// is the sentence, not the id — the model is being told what they pointed
    /// at, and an id is not a thing anyone points at.
    fn resolve(&self, id: &str) -> Result<Option<String>, String> {
        let board = self.board.lock();
        Ok(board.get(id).map(|entry| {
            let mut said = assertion::describe(&entry.assertion);
            said.push_str(&format!(" [{id}, by {}]", entry.author.word()));
            said
        }))
    }

    fn semantic_target(
        &self,
        target: &SemanticTargetRef,
    ) -> Result<Option<SemanticTarget>, String> {
        if target.extension_id != "board" {
            return Ok(None);
        }
        let board = self.board.lock();
        Ok(board.get(&target.target_id).map(|entry| {
            let said = assertion::describe(&entry.assertion);
            let label = if said.chars().count() > 60 {
                format!("{}…", said.chars().take(57).collect::<String>())
            } else {
                said.clone()
            };
            SemanticTarget::new(
                target.clone(),
                label,
                format!("{said} [{}, by {}]", target.target_id, entry.author.word()),
            )
        }))
    }

    fn reconnect_events(&self) -> Vec<(String, JsonValue)> {
        vec![(CHANGED_EVENT.to_string(), view(&self.board.lock()))]
    }
}

// ── the browser's view of the board ───────────────────────────────────────

/// One flat shape for the page: the assertion's own fields, plus who wrote it
/// and what a human did to it. Built here rather than derived so the browser
/// contract is visible in one place and does not shift when a serde attribute
/// changes.
fn entry_view(entry: &Entry) -> JsonValue {
    let mut object = match serde_json::to_value(&entry.assertion) {
        Ok(JsonValue::Object(object)) => object,
        _ => JsonMap::new(),
    };
    object.insert("author".to_string(), json!(entry.author.word()));
    object.insert("authored_at".to_string(), json!(entry.authored_at));
    object.insert(
        "authored_at_revision".to_string(),
        json!(entry.authored_at_revision),
    );
    object.insert("changed_at".to_string(), json!(entry.changed_at));
    object.insert(
        "mark".to_string(),
        match &entry.mark {
            Some((mark, note)) => json!({
                "as": mark.word(),
                "glyph": mark.glyph(),
                "note": note,
                "by": entry.marked_by.as_ref().map(Author::word),
                "at": entry.marked_at,
                "board_revision": entry.marked_at_revision,
            }),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn view(board: &Board) -> JsonValue {
    let delta = board.human_delta();
    json!({
        "revision": board.revision(),
        "subject": board.subject(),
        // The revision the person had seen when they last said so — what
        // "unseen" is measured from, rather than an unexplained highlight.
        "seen_at": delta.since,
        // What is new *to the person looking*, which is the only kind of
        // "new" worth putting a dot next to.
        "unseen": delta
            .changed
            .iter()
            .chain(delta.marked.iter())
            .collect::<Vec<_>>(),
        "entries": board.entries().iter().map(entry_view).collect::<Vec<_>>(),
    })
}

// ── actions ───────────────────────────────────────────────────────────────

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
        .filter(|value| !value.trim().is_empty())
}

/// One assertion's arguments. Every kind's fields in one schema, because the
/// model writes `{"kind": "...", ...}` and a `oneOf` of seven branches reads
/// worse in a tool catalog than one list with the required fields named in the
/// description.
fn assertion_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["claim", "question", "choice", "relation", "group", "evidence", "decision"]
            },
            "id": {
                "type": "string",
                "description": "your name for this assertion; writing the same id again replaces it, and a human mark on it survives"
            },
            "text": { "type": "string", "description": "claim / question / choice: what is being said" },
            "status": { "type": "string", "enum": ["proposed", "supported", "contested"], "description": "claim only. `settled` is not yours to write — only a human ✓ settles a claim" },
            "blocking": { "type": "boolean", "description": "question only: work stops until it is answered" },
            "tradeoff": { "type": "string", "description": "choice only: what it costs. A choice with no tradeoff is a preference in disguise" },
            "from": { "type": "string", "description": "relation only: the id it starts at" },
            "to": { "type": "string", "description": "relation only: the id it points at" },
            "how": { "type": "string", "enum": ["supports", "contradicts", "depends_on", "refines", "answers"], "description": "relation only" },
            "label": { "type": "string", "description": "group only: what the set is called" },
            "members": { "type": "array", "items": { "type": "string" }, "description": "group only: ids in the set" },
            "about": { "type": "string", "description": "evidence only: the id it bears on" },
            "source": { "type": "string", "description": "evidence only: what you actually checked — a command and its result, a file and its lines" },
            "verdict": { "type": "string", "enum": ["unverified", "confirmed", "refuted"], "description": "evidence only; defaults to unverified, because citing is not checking" },
            "chose": { "type": "string", "description": "decision only: the id that won" },
            "over": { "type": "array", "items": { "type": "string" }, "description": "decision only: the ids that lost" },
            "because": { "type": "string", "description": "decision only, required: why. A decision without it is unreviewable later" }
        },
        "required": ["kind", "id"],
        "additionalProperties": false
    })
}

fn actions(state: &Arc<BoardState>) -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "board_read",
            "Read the shared board: every claim, question, choice, relation, \
             mark and decision, with who wrote each one. Call this FIRST in a \
             turn and again after the human says they marked something. It \
             ends with what changed since YOU last read — a `?` next to a \
             claim of yours is a person asking you to answer it, and a `✓` is \
             the only thing that settles anything.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| Ok(state.board.lock().read())))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "await_board",
            "Wait until the shared assertion board changes, then return the same current read-back as board_read. A human board question, claim, mark, or decision wakes the call immediately. Incoming human chat also wakes the call; call chat_read to collect that turn, then chat_reply to answer. Use await_board when you deliberately want only the board and chat lanes. Use await_input when you also need Atlas changes and canvas marks. Returns after `seconds` (default 60, max 600) with no change if the board stayed quiet; call it again to keep waiting.",
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
                        Ok(seconds) => seconds,
                        Err(error) => return Effect::Reject(error),
                    };
                    let starting_revision = state.board.lock().revision();
                    let state = state.clone();
                    Effect::AsyncQuery(Box::pin(async move {
                        let deadline =
                            tokio::time::Instant::now() + Duration::from_secs(seconds);
                        loop {
                            // Register before checking so a write between the
                            // revision check and the await cannot be lost.
                            let notified = state.changed.notified();
                            if state.board.lock().revision() != starting_revision {
                                return Ok(state.board.lock().read());
                            }
                            tokio::select! {
                                _ = notified => {}
                                _ = tokio::time::sleep_until(deadline) => {
                                    return Ok(format!(
                                        "Waited {seconds}s and the board did not change. Call await_board again to keep waiting."
                                    ));
                                }
                            }
                        }
                    }))
                }
            },
        )
        .wake_on_chat()
        .agent_only(),
        ToolDef::new(
            "await_input",
            "Page validation failures return a tool error, including errors from your own edits or browser exceptions. Wait for the next inbound change across the shared Atlas, assertion board, or conversation panel. Atlas document changes including canvas marks, board questions, claims, board marks and decisions, incoming human chat, and the human changing what they have selected on the canvas all wake this call. A marquee over several cards is one selection and wakes this once, with the whole set named in WHERE THE HUMAN IS. It returns the current read-back from the lane that changed. For chat, call chat_read to collect the turn, then chat_reply to answer. Use this after atlas_read and board_read when serving the whole page, so you do not have to guess which lane the human will use. The existing await_atlas and await_board tools remain available for lane-specific waits. Returns after `seconds` (default 60, max 600) if every lane stays quiet; call await_input again to keep waiting.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_WAIT_SECONDS,
                        "description": "How long to wait for Atlas, board, or chat input."
                    }
                }
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let seconds = match wait_seconds(args) {
                        Ok(seconds) => seconds,
                        Err(error) => return Effect::Reject(error),
                    };
                    let starting_board_revision = state.board.lock().revision();
                    // Pointing at something is an inbound act even though it
                    // writes nothing to the document. Before this lane existed
                    // a human could marquee half the board and every parked
                    // agent slept through it, because the only wake signals
                    // were CRDT commits and board revisions.
                    let starting_attention = state.atlas.human_attention_revision();
                    let me = state.atlas.agent_byline();
                    let state = state.clone();
                    Effect::AsyncQuery(Box::pin(async move {
                        let deadline =
                            tokio::time::Instant::now() + Duration::from_secs(seconds);
                        loop {
                            // Register every notification before checking any
                            // predicate so a commit between the check and the
                            // await cannot be lost.
                            let board_woken = state.changed.notified();
                            let atlas_woken = state.atlas.changed().notified();
                            let attention_woken = state.atlas.attention_notifier().notified();
                            tokio::pin!(attention_woken, atlas_woken, board_woken);
                            // Enrol this waiter BEFORE the checks below rather
                            // than when the select first polls it. A `Notified`
                            // registers on its first poll, and a pointing act
                            // has no second signal to fall back on, so a
                            // gesture landing in that window would be lost
                            // until the next unrelated wake.
                            attention_woken.as_mut().enable();
                            atlas_woken.as_mut().enable();
                            board_woken.as_mut().enable();
                            state.atlas.validation_failure()?;
                            if state.board.lock().revision() != starting_board_revision {
                                return Ok(state.board.lock().read());
                            }
                            if state.atlas.unread_from_others(&me) {
                                return state.atlas.read_back(state.atlas.as_ref(), &me);
                            }
                            if state.atlas.human_attention_revision() != starting_attention {
                                return state.atlas.read_back(state.atlas.as_ref(), &me);
                            }
                            tokio::select! {
                                _ = board_woken.as_mut() => {}
                                _ = atlas_woken.as_mut() => {}
                                _ = attention_woken.as_mut() => {}
                                _ = tokio::time::sleep_until(deadline) => {
                                    return Ok(format!(
                                        "Waited {seconds}s and no Atlas, board, or chat input arrived. Call await_input again to keep waiting."
                                    ));
                                }
                            }
                        }
                    }))
                }
            },
        )
        .wake_on_chat()
        .agent_only(),
        ToolDef::new(
            "board_assert",
            "Say something on the shared board — one assertion or a whole \
             argument at once. Relations may point at ids created earlier in \
             the same call. Assertions carry meaning, never position: you say \
             what it IS and the page decides how it looks. Writing an existing \
             id replaces it and keeps any human mark, which is how you answer \
             a question someone put on your own claim.",
            json!({
                "type": "object",
                "properties": {
                    "assertions": {
                        "type": "array",
                        "minItems": 1,
                        "items": assertion_schema()
                    }
                },
                "required": ["assertions"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let raw = match args.get("assertions").and_then(JsonValue::as_array) {
                        Some(items) => items.clone(),
                        None => return Effect::Reject("`assertions` must be an array".to_string()),
                    };
                    let mut parsed = Vec::with_capacity(raw.len());
                    for item in raw {
                        match serde_json::from_value::<Assertion>(item.clone()) {
                            Ok(assertion) => parsed.push(assertion),
                            // Rejected before anything is written: a malformed
                            // item in a batch must not half-apply the batch.
                            Err(error) => {
                                return Effect::Reject(format!(
                                    "{item} is not a valid assertion: {error}"
                                ))
                            }
                        }
                    }
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let author = state.agent_author();
                        state.write(|board| {
                            let mut written = Vec::new();
                            for assertion in parsed {
                                match board.assert(assertion, author.clone()) {
                                    Ok(message) => written.push(message),
                                    // Stop at the first refusal and say what
                                    // already landed. Reporting a clean failure
                                    // after writing four things would be a lie.
                                    Err(error) => {
                                        return Err(if written.is_empty() {
                                            error
                                        } else {
                                            format!("{error}. Already written: {}", written.join(", "))
                                        })
                                    }
                                }
                            }
                            Ok(Some(format!(
                                "{}. The human can mark or answer any of it.",
                                written.join(", ")
                            )))
                        })
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "board_retract",
            "Take something back. Anything left pointing at it goes too, \
             because a relation to nothing is worse than no relation.",
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
                        state.write(|board| board.retract(&id).map(Some))
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "board_subject",
            "Name what the board is about. One line, in the human's words \
             where you have them.",
            json!({
                "type": "object",
                "properties": { "subject": { "type": "string" } },
                "required": ["subject"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let subject = string(args, "subject");
                    if subject.trim().is_empty() {
                        return Effect::Reject("`subject` is empty".to_string());
                    }
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        state.write(|board| {
                            board.set_subject(subject);
                            Ok(None)
                        })
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            "atlas_cement_propose",
            "Propose the Atlas exit without writing anything. Returns the \
             exact Govern-shaped obligations-v0.1.json draft and \
             cement-receipt.json that the CURRENT settled board and Atlas \
             revision would produce. Any assertion without a fresh human ✓ \
             refuses the whole proposal and names every unsettled id. Only \
             the human can invoke atlas_cement and name its output directory.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Query(Box::new(move |_| {
                        let atlas_revision = state.atlas.document_revision()?;
                        let cemented_at = crate::timestamp::now_iso()?;
                        cement::propose_at(
                            &state.board.lock(),
                            &atlas_revision,
                            &cemented_at,
                        )?
                        .preview()
                    }))
                }
            },
        )
        .agent_only(),
        // ── the human's half. No agent twin exists for any of these. ────────
        ToolDef::new(
            "board_compose",
            "Write a claim or a question in your own name.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["claim", "question"] },
                    "text": { "type": "string" }
                },
                "required": ["kind", "text"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let (kind, text) = (string(args, "kind"), string(args, "text"));
                    if text.trim().is_empty() {
                        return Effect::Reject("say something first".to_string());
                    }
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        state.write(|board| {
                            // The id is minted from the revision rather than
                            // taken from the page: a browser that could choose
                            // an id could overwrite the agent's work by naming
                            // it, and this endpoint's whole job is that the
                            // byline is not negotiable.
                            let id = format!("you-{}", board.revision() + 1);
                            let assertion = if kind == "question" {
                                Assertion::Question { id, text, blocking: false }
                            } else {
                                Assertion::Claim { id, text, status: Default::default() }
                            };
                            board.assert(assertion, Author::You).map(Some)
                        })
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "board_mark",
            "Put ?, !, ✓ or ✗ on anything. Yours alone — the agent has no \
             action that writes one, so a ✓ is always something a person put \
             there, and it survives the agent rewriting the thing underneath.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "mark": { "type": "string", "enum": ["question", "important", "agree", "disagree"] },
                    "note": { "type": "string" }
                },
                "required": ["id", "mark"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let id = string(args, "id");
                    let note = optional_string(args, "note");
                    let mark = match Mark::parse(&string(args, "mark")) {
                        Ok(mark) => mark,
                        Err(error) => return Effect::Reject(error),
                    };
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        state.write(|board| board.mark(&id, mark, note).map(Some))
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "board_unmark",
            "Take a mark back.",
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
                        state.write(|board| board.unmark(&id).map(Some))
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "board_seen",
            "Say you have looked. This is what moves your side of the delta — \
             the page repainting does not, because a repaint is not reading.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        state.write(|board| {
                            board.acknowledge_human();
                            Ok(None)
                        })
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "board_clear",
            "Wipe the board and start over.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            {
                let state = state.clone();
                move |_args: &JsonValue| {
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        state.write(|board| Ok(Some(board.clear())))
                    }))
                }
            },
        )
        .human_only(),
        ToolDef::new(
            "atlas_cement",
            "Cement every settled assertion into an advisory Govern draft and \
             a session receipt. Human-only: the model may preview this with \
             atlas_cement_propose but cannot write it. `directory` must name \
             a new directory; the two files publish together or not at all.",
            json!({
                "type": "object",
                "properties": {
                    "directory": {
                        "type": "string",
                        "minLength": 1,
                        "description": "new output directory for obligations-v0.1.json and cement-receipt.json"
                    }
                },
                "required": ["directory"],
                "additionalProperties": false
            }),
            {
                let state = state.clone();
                move |args: &JsonValue| {
                    let directory = string(args, "directory");
                    if directory.trim().is_empty() {
                        return Effect::Reject("`directory` is empty".to_string());
                    }
                    let state = state.clone();
                    Effect::Mutate(Box::new(move |_| {
                        let atlas_revision = state.atlas.document_revision()?;
                        let cemented_at = crate::timestamp::now_iso()?;
                        let written = cement::cement_at(
                            &state.board.lock(),
                            &atlas_revision,
                            &cemented_at,
                            std::path::Path::new(&directory),
                        )?;
                        Ok(Some(format!(
                            "cemented {} and {} at Atlas revision {atlas_revision}",
                            written[0].display(),
                            written[1].display()
                        )))
                    }))
                }
            },
        )
        .human_only(),
    ]
}

// ── the extension ─────────────────────────────────────────────────────────

pub struct BoardExtension {
    state: Arc<BoardState>,
    actions: Vec<ToolDef>,
}

impl BoardExtension {
    pub fn new(state: Arc<BoardState>) -> Self {
        let actions = actions(&state);
        Self { state, actions }
    }
}

impl Extension for BoardExtension {
    fn id(&self) -> &str {
        "board"
    }

    /// Remember the host-stamped attribution for the action about to run.
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
        Some(
            ClientModule::lazy("board", "0.1.0", "/extensions/board/index.js", "board-view")
                .event(CHANGED_EVENT)
                // Exactly the human's half of the catalog. The loader refuses
                // any other name from this module, so the browser cannot call
                // an agent action even if its own code asked to.
                .action("board_compose")
                .action("board_mark")
                .action("board_unmark")
                .action("board_seen")
                .action("board_clear")
                .action("atlas_cement")
                .capability("dom"),
        )
    }

    fn capabilities(&self) -> Vec<&str> {
        vec!["dom"]
    }

    fn focus_events(&self) -> &[&str] {
        &[FOCUS_EVENT]
    }

    fn semantic_targets(&self) -> bool {
        true
    }

    fn binary_transport(&self) -> bool {
        // The atlas owns `/ws`. Frames on that transport are not namespaced,
        // so composition rejects a second claimant rather than broadcasting
        // ambiguous bytes — and the board has nothing to put there anyway.
        false
    }

    fn routes(&self) -> Vec<RouteDef> {
        let mut routes = vec![RouteDef {
            method: HttpMethod::Get,
            // The vocabulary, generated from the enum so it cannot drift from
            // what the board will actually accept. A read, so it is a route
            // rather than an action: anything that can reach the page can ask
            // what the words mean.
            path: "/board/primitives",
            handler: Box::new(move |_request: RouteRequest| {
                Box::pin(async move { RouteResponse::json(200, assertion::catalog()) })
            }),
        }];
        routes.extend(paper_routes(&self.state));
        routes
    }

    fn action_routes(&self) -> Vec<ActionRouteDef> {
        paper_action_routes(&self.state)
    }
}

// ── the paper view ────────────────────────────────────────────────────────

/// A second rendering of the same board, made entirely by the server.
///
/// The browser-module view and this one share no code, no framework, and no
/// opinion about each other. They share a `Board`. That is the claim
/// [`crate::assertion`] opens with — *the same assertion stream can drive a very
/// different picture without the writer knowing or caring* — and it is worth
/// having something that stands on it rather than a comment that asserts it.
///
/// It is also the tier this runtime could not serve until routes learned to
/// carry markup: no build step, no bundle, no hand-written client. The cost of
/// adding it to an app that already has `/mcp` and `/events` is this function.
///
/// The read routes below are ordinary extension routes. Every write is an
/// [`ActionRouteDef`] over the board's existing human-only action, so reaching
/// a POST path is not itself evidence of authorship. The runtime must first
/// establish [`ag_ui_surface::Caller::Human`] from its browser-session
/// credential, then the shared dispatcher enforces audience and schema before
/// the action signs anything `you`.
fn paper_routes(state: &Arc<BoardState>) -> Vec<RouteDef> {
    let page = state.clone();
    let rows = state.clone();
    let primitives = state.clone();

    vec![
        RouteDef {
            method: HttpMethod::Get,
            path: "/board/paper",
            handler: Box::new(move |_request: RouteRequest| {
                let _ = &page;
                Box::pin(async move { RouteResponse::html(200, crate::paper_page::PAGE) })
            }),
        },
        RouteDef {
            method: HttpMethod::Get,
            path: "/board/paper/rows",
            handler: Box::new(move |request: RouteRequest| {
                let state = rows.clone();
                Box::pin(async move {
                    let board = state.board.lock();
                    // The poll carries the revision it already has. Answering
                    // 204 when nothing changed is what stops the document
                    // being destroyed and rebuilt under the pointer once a
                    // second.
                    let since = request
                        .query
                        .get("since")
                        .and_then(|since| since.parse::<u64>().ok());
                    if since == Some(board.revision()) {
                        return RouteResponse::html(204, "");
                    }
                    RouteResponse::html(200, crate::paper::board(&board))
                })
            }),
        },
        RouteDef {
            method: HttpMethod::Get,
            path: "/board/paper/primitives",
            handler: Box::new(move |_request: RouteRequest| {
                let _ = &primitives;
                Box::pin(async move {
                    RouteResponse::html(200, crate::paper::primitives(&assertion::catalog()))
                })
            }),
        },
    ]
}

/// HTML adapters for the paper lane's human actions. Each callback only
/// renders the state after the shared dispatcher has applied the named action;
/// it contains no second mutation implementation and cannot turn a refusal
/// into a healthy board response.
fn paper_action_routes(state: &Arc<BoardState>) -> Vec<ActionRouteDef> {
    fn rendered(state: &Arc<BoardState>) -> RouteResponse {
        RouteResponse::html(200, crate::paper::board(&state.board.lock()))
    }

    let route = |path, action, state: Arc<BoardState>| {
        ActionRouteDef::post(path, action, move |_request: ActionRouteRequest| {
            let state = state.clone();
            Box::pin(async move { rendered(&state) })
        })
    };

    vec![
        route("/board/paper/compose", "board_compose", state.clone()),
        route("/board/paper/mark", "board_mark", state.clone()),
        route("/board/paper/unmark", "board_unmark", state.clone()),
        route("/board/paper/seen", "board_seen", state.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ag_ui_surface::services::ServiceRegistry;
    use ag_ui_surface::AsyncQueryEffect;
    use ag_ui_surface::{ActionAudience, Actor, AppRecipe, Caller, CompositeSurface, Surface};
    use serde::Deserialize;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::broadcast;

    const OLD_BOARD_JSON: &str = r#"{
  "entries": [
    {
      "assertion": {
        "kind": "claim",
        "id": "old-agent",
        "text": "written before named agents",
        "status": "supported"
      },
      "author": "agent",
      "changed_at": 1,
      "mark": null
    },
    {
      "assertion": {
        "kind": "question",
        "id": "old-you",
        "text": "does this still load?",
        "blocking": false
      },
      "author": "you",
      "changed_at": 2,
      "mark": null
    }
  ],
  "revision": 2,
  "subject": "legacy board"
}"#;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum LegacyAuthor {
        Agent,
        You,
    }

    #[derive(Debug, Deserialize)]
    struct LegacyEntry {
        author: LegacyAuthor,
    }

    #[derive(Debug, Deserialize)]
    struct LegacyBoard {
        entries: Vec<LegacyEntry>,
    }

    fn transport() -> (Transport, broadcast::Receiver<String>) {
        let (ws_tx, _) = broadcast::channel(32);
        let (sse_tx, sse_rx) = broadcast::channel(32);
        (
            Transport {
                ws_tx,
                sse_tx,
                history: Arc::new(Mutex::new(VecDeque::new())),
                awaiting: Arc::new(AtomicBool::new(false)),
                transcript_replay_lock: Arc::new(Mutex::new(())),
            },
            sse_rx,
        )
    }

    fn state(dir: &std::path::Path) -> (Arc<BoardState>, broadcast::Receiver<String>) {
        let (transport, sse_rx) = transport();
        let atlas = AtlasState::open(transport.clone(), dir.join("atlas.json"), false)
            .expect("atlas state");
        let state =
            BoardState::open(transport, dir.join("board.json"), atlas).expect("board state");
        (state, sse_rx)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("same-page-board-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn call(
        state: &Arc<BoardState>,
        defs: &[ToolDef],
        name: &str,
        args: JsonValue,
    ) -> Result<Option<String>, String> {
        let def = defs.iter().find(|def| def.name == name).expect("action");
        match (def.apply)(&args) {
            Effect::Mutate(apply) => apply(state.as_ref()),
            Effect::Query(apply) => apply(state.as_ref()).map(Some),
            Effect::Reject(error) => Err(error),
            _ => panic!("unexpected effect"),
        }
    }

    fn wait_effect(defs: &[ToolDef], seconds: u64) -> AsyncQueryEffect {
        wait_effect_named(defs, "await_board", seconds)
    }

    fn wait_effect_named(defs: &[ToolDef], name: &str, seconds: u64) -> AsyncQueryEffect {
        let action = defs
            .iter()
            .find(|def| def.name == name)
            .unwrap_or_else(|| panic!("{name} action"));
        match (action.apply)(&json!({ "seconds": seconds })) {
            Effect::AsyncQuery(future) => future,
            _ => panic!("{name} should be an async query"),
        }
    }

    #[tokio::test]
    async fn an_agent_blocked_in_await_board_wakes_for_a_human_question() {
        let dir = temp_dir("await-human-question");
        let (state, _sse) = state(&dir);
        let defs = actions(&state);
        call(&state, &defs, "board_read", json!({})).expect("initial read");

        let waiting = tokio::spawn(wait_effect(&defs, 30));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "await_board must remain parked while the board is quiet"
        );

        call(
            &state,
            &defs,
            "board_compose",
            json!({
                "kind": "question",
                "text": "Which boundary should the architecture preserve?"
            }),
        )
        .expect("human question lands");

        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("the human question should wake the waiter within five seconds")
            .expect("the wait task did not panic")
            .expect("the wait returns the current board read-back");
        assert!(
            seen.contains("Which boundary should the architecture preserve?"),
            "{seen}"
        );
        assert!(seen.contains("by you"), "{seen}");
    }

    #[tokio::test]
    async fn unified_wait_wakes_for_a_board_question() {
        let dir = temp_dir("unified-await-human-question");
        let (state, _sse) = state(&dir);
        let defs = actions(&state);
        call(&state, &defs, "board_read", json!({})).expect("initial read");

        let waiting = tokio::spawn(wait_effect_named(&defs, "await_input", 30));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "await_input must remain parked while every lane is quiet"
        );

        call(
            &state,
            &defs,
            "board_compose",
            json!({
                "kind": "question",
                "text": "Which state transition needs proof?"
            }),
        )
        .expect("human question lands");

        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("the board question should wake the unified wait")
            .expect("the wait task did not panic")
            .expect("the wait returns the Board read-back");
        assert!(
            seen.contains("Which state transition needs proof?"),
            "{seen}"
        );
        assert!(seen.contains("by you"), "{seen}");
    }

    #[test]
    fn await_board_opts_into_the_runtime_chat_wake_path() {
        let dir = temp_dir("await-chat-capability");
        let (state, _sse) = state(&dir);
        let action = actions(&state)
            .into_iter()
            .find(|action| action.name == "await_board")
            .expect("await_board action");
        assert!(
            action.wake_on_chat,
            "await_board must let incoming human chat wake its parked query"
        );
        let unified = actions(&state)
            .into_iter()
            .find(|action| action.name == "await_input")
            .expect("await_input action");
        assert!(
            unified.wake_on_chat,
            "await_input must let incoming human chat wake its parked query"
        );
    }

    #[test]
    fn await_board_defaults_to_sixty_seconds_and_caps_at_six_hundred() {
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

    fn audience(defs: &[ToolDef], name: &str) -> ActionAudience {
        defs.iter()
            .find(|def| def.name == name)
            .expect("action")
            .audience
    }

    #[test]
    fn a_board_json_written_before_named_agents_still_loads() {
        let legacy: LegacyBoard =
            serde_json::from_str(OLD_BOARD_JSON).expect("fixture is valid for the old reader");
        assert_eq!(legacy.entries[0].author, LegacyAuthor::Agent);
        assert_eq!(legacy.entries[1].author, LegacyAuthor::You);

        let dir = temp_dir("old-author-format");
        std::fs::write(dir.join("board.json"), OLD_BOARD_JSON).expect("write old board fixture");
        let (state, _sse) = state(&dir);
        let board = state.board.lock();
        assert_eq!(board.entries()[0].author, Author::Agent);
        assert_eq!(board.entries()[1].author, Author::You);
        assert_eq!(board.subject(), Some("legacy board"));
    }

    #[test]
    fn the_named_author_format_is_additive_and_old_readers_fail_closed() {
        let dir = temp_dir("new-author-format");
        let (state, _sse) = state(&dir);
        state
            .write(|board| {
                board
                    .assert(
                        Assertion::Claim {
                            id: "named".to_string(),
                            text: "signed by a participant".to_string(),
                            status: Default::default(),
                        },
                        Author::Named("reviewer".to_string()),
                    )
                    .map(|_| ())
            })
            .expect("write named assertion");

        let saved =
            std::fs::read_to_string(dir.join("board.json")).expect("read persisted new board");
        let loaded: Board = serde_json::from_str(&saved).expect("new reader loads named author");
        assert_eq!(loaded.entries()[0].author.word(), "reviewer");

        // The old enum knows only the two string variants `agent` and `you`.
        // The additive `{"named":"reviewer"}` variant therefore makes an old
        // reader reject the new board instead of silently forging a generic
        // byline. Rolling back across a named write is fail-closed.
        let error = serde_json::from_str::<LegacyBoard>(&saved)
            .expect_err("old reader rejects new variant");
        assert!(
            error.to_string().contains("named"),
            "the refusal must identify the additive variant: {error}"
        );
    }

    #[test]
    fn board_assert_uses_the_attached_agents_room_label() {
        let dir = temp_dir("named-board-assert");
        let (state, _sse) = state(&dir);
        let extension = BoardExtension::new(state.clone());
        extension.note_caller(&Actor {
            caller: Caller::Agent,
            label: Some("reviewer".to_string()),
            participant_id: Some("agent-reviewer".to_string()),
            hue: None,
            responsible: None,
        });

        call(
            &state,
            extension.actions(),
            "board_assert",
            json!({
                "assertions": [{
                    "kind": "claim",
                    "id": "named",
                    "text": "this byline is host stamped"
                }]
            }),
        )
        .expect("named agent writes");

        let board = state.board.lock();
        assert_eq!(
            board.get("named").expect("named claim").author.word(),
            "reviewer"
        );
        assert!(
            board.peek().contains("[named, by reviewer]"),
            "agent read-back must carry the named byline"
        );
    }

    /// The property the whole port exists to preserve. `board_mark` is not
    /// "documented as human-only" — it is unreachable to a model, and the
    /// refusal is the runtime's, not this file's.
    #[test]
    fn the_agent_cannot_reach_the_humans_half_and_the_reverse() {
        let dir = temp_dir("audience");
        let (state, _sse) = state(&dir);
        let defs = actions(&state);

        for name in [
            "board_read",
            "await_board",
            "board_assert",
            "board_retract",
            "board_subject",
            "atlas_cement_propose",
        ] {
            assert_eq!(audience(&defs, name), ActionAudience::Agent, "{name}");
            assert!(!Caller::Human.may_call(audience(&defs, name)), "{name}");
        }
        for name in [
            "board_compose",
            "board_mark",
            "board_unmark",
            "board_seen",
            "board_clear",
            "atlas_cement",
        ] {
            assert_eq!(audience(&defs, name), ActionAudience::Human, "{name}");
            assert!(!Caller::Agent.may_call(audience(&defs, name)), "{name}");
            // A companion assistant is an agent for authorisation. It must not
            // be able to put a ✓ on the board either.
            assert!(!Caller::Companion.may_call(audience(&defs, name)), "{name}");
        }
        assert!(
            defs.iter().all(|def| def.audience != ActionAudience::Both),
            "an action open to both audiences would have no byline to sign"
        );
    }

    #[test]
    fn paper_mutations_are_only_adapters_over_human_actions() {
        let dir = temp_dir("paper-action-routes");
        let (state, _sse) = state(&dir);
        let extension = BoardExtension::new(state);

        assert!(
            extension
                .routes()
                .iter()
                .all(|route| route.method == HttpMethod::Get),
            "an ordinary paper route must not own a mutation"
        );
        let action_routes = extension.action_routes();
        let declared: std::collections::BTreeMap<_, _> = action_routes
            .iter()
            .map(|route| (route.path, route.action))
            .collect();
        assert_eq!(
            declared,
            std::collections::BTreeMap::from([
                ("/board/paper/compose", "board_compose"),
                ("/board/paper/mark", "board_mark"),
                ("/board/paper/seen", "board_seen"),
                ("/board/paper/unmark", "board_unmark"),
            ])
        );
        for route in action_routes {
            let action = extension
                .actions()
                .iter()
                .find(|action| action.name == route.action)
                .expect("every paper route must name its own typed action");
            assert_eq!(action.audience, ActionAudience::Human, "{}", route.path);
        }
    }

    /// One host. The real recipe, the real extensions, composed the way
    /// `main` composes them — so an action-name collision, a route collision,
    /// or two claimants on `/ws` fails here rather than at somebody's startup.
    #[test]
    fn the_board_and_the_map_compose_into_one_surface() {
        let dir = temp_dir("compose");
        let (board, _sse) = state(&dir);
        let (atlas_transport, _atlas_sse) = transport();
        let repo = Arc::new(
            crate::source::Repo::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("repo"),
        );
        let atlas = crate::atlas::AtlasState::open(atlas_transport, dir.join("atlas.json"), false)
            .expect("atlas state");

        let recipe = AppRecipe::from_file(concat!(env!("CARGO_MANIFEST_DIR"), "/agui.app.toml"))
            .expect("the shipped recipe parses");

        // Negative control for the declared `repository` service: the same
        // recipe and the same extensions, with nothing providing it. If this
        // composed, the recipe's `services = ["repository"]` line would be
        // documentation rather than a gate.
        {
            let (unbound_transport, _unbound_sse) = transport();
            let unbound_atlas = crate::atlas::AtlasState::open(
                unbound_transport,
                dir.join("unbound-atlas.json"),
                false,
            )
            .expect("atlas state");
            let (unbound_board, _unbound_board_sse) = state(&temp_dir("compose-unbound"));
            let error = CompositeSurface::from_recipe(
                &recipe,
                vec![
                    Box::new(crate::atlas::AtlasExtension::new(unbound_atlas)),
                    Box::new(BoardExtension::new(unbound_board)),
                ],
            )
            .err()
            .expect("composing without the repository service must fail");
            assert!(
                error.to_string().contains("repository"),
                "the failure must name the missing service: {error}"
            );
        }

        let mut services = ServiceRegistry::new();
        services
            .provide("repository", repo)
            .expect("provide the repository service");
        let surface = CompositeSurface::from_recipe_with_services(
            &recipe,
            vec![
                Box::new(crate::atlas::AtlasExtension::new(atlas)),
                Box::new(BoardExtension::new(board)),
            ],
            &services,
        )
        .expect("the shipped recipe composes the shipped extensions");

        assert_eq!(surface.extension_ids(), vec!["atlas", "board"]);
        let names: Vec<&str> = surface
            .tools()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        for expected in [
            "atlas_read",
            "await_atlas",
            "board_read",
            "await_board",
            "await_input",
            "board_mark",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} is missing from the one catalog"
            );
        }
        // The point of composing rather than running two servers: a terminal
        // agent attaching to this host sees both vocabularies in one list.
        assert!(
            names.iter().any(|name| name.starts_with("atlas_"))
                && names.iter().any(|name| name.starts_with("board_")),
            "both artifacts must be reachable through the same catalog"
        );
    }

    /// The exact live failure a real boot with `AGUI_AGENT_INK=1` hit: the
    /// browser module's `.no_actions()`/`.action(...)` declaration has to
    /// list exactly the human-visible actions `AtlasExtension::actions()`
    /// registers (`crates/ag-ui-surface/src/composition.rs`'s
    /// human-actions-must-match-declared-actions gate), and that set is
    /// different with the flag on (it now includes the human-only
    /// `atlas_mode_promote_to_alignment`) than with it off. Every other
    /// compose test in this file only ever built the surface with the flag
    /// off, so a mismatch that only exists when it is on passed a green
    /// suite and then failed the first real boot. This composes with the
    /// flag both ways and asserts both succeed, with the promotion tool
    /// present only when it is on.
    #[test]
    fn the_surface_composes_with_agent_ink_both_on_and_off() {
        let repo = || {
            Arc::new(
                crate::source::Repo::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                    .expect("repo"),
            )
        };
        let recipe = AppRecipe::from_file(concat!(env!("CARGO_MANIFEST_DIR"), "/agui.app.toml"))
            .expect("the shipped recipe parses");

        for agent_ink in [false, true] {
            let dir = temp_dir(if agent_ink {
                "compose-agent-ink-on"
            } else {
                "compose-agent-ink-off"
            });
            let (board, _sse) = state(&dir);
            let (atlas_transport, _atlas_sse) = transport();
            let atlas =
                crate::atlas::AtlasState::open(atlas_transport, dir.join("atlas.json"), agent_ink)
                    .expect("atlas state");

            let mut services = ServiceRegistry::new();
            services
                .provide("repository", repo())
                .expect("provide the repository service");
            let surface = CompositeSurface::from_recipe_with_services(
                &recipe,
                vec![
                    Box::new(crate::atlas::AtlasExtension::new(atlas)),
                    Box::new(BoardExtension::new(board)),
                ],
                &services,
            )
            .unwrap_or_else(|error| {
                panic!("composing with agent_ink={agent_ink} must succeed: {error}")
            });

            let names: std::collections::HashSet<&str> = surface
                .tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect();
            assert_eq!(
                names.contains("atlas_mode_promote_to_alignment"),
                agent_ink,
                "the promotion tool must be registered exactly when agent_ink={agent_ink}"
            );
        }
    }

    #[test]
    fn the_board_survives_a_restart_but_the_delta_does_not() {
        let dir = temp_dir("restart");
        {
            let (state, _sse) = state(&dir);
            let defs = actions(&state);
            call(
                &state,
                &defs,
                "board_assert",
                json!({"assertions": [{"kind": "claim", "id": "a", "text": "the port is done"}]}),
            )
            .expect("assert");
            call(&state, &defs, "board_read", json!({})).expect("read");
            let second = call(&state, &defs, "board_read", json!({}))
                .expect("read again")
                .unwrap_or_default();
            assert!(
                second.contains("nothing changed"),
                "the agent has now read it"
            );
        }

        let (state, _sse) = state(&dir);
        let defs = actions(&state);
        let read = call(&state, &defs, "board_read", json!({}))
            .expect("read")
            .unwrap_or_default();
        assert!(read.contains("the port is done"), "the claim survived");
        assert!(
            read.contains("written: a"),
            "a restart is a new session, and a new session has not read anything yet"
        );
    }

    /// A batch that half-lands must say so, and the half that landed must be
    /// on the page. Reporting a clean failure over three real writes is the
    /// exact shape of lie this surface exists to make impossible.
    #[test]
    fn a_batch_that_fails_midway_still_publishes_what_landed() {
        let dir = temp_dir("partial");
        let (state, mut sse) = state(&dir);
        let defs = actions(&state);

        let error = call(
            &state,
            &defs,
            "board_assert",
            json!({"assertions": [
                {"kind": "claim", "id": "a", "text": "this one is fine"},
                {"kind": "relation", "id": "r", "from": "a", "to": "ghost", "how": "supports"}
            ]}),
        )
        .expect_err("a relation to nothing is refused");
        assert!(
            error.contains("ghost"),
            "the error names what was missing: {error}"
        );
        assert!(
            error.contains("Already written: added a"),
            "and what already landed: {error}"
        );

        assert!(
            state.board.lock().get("a").is_some(),
            "the good write is real"
        );
        let published = sse
            .try_recv()
            .expect("the page was told about the good write");
        assert!(
            published.contains("this one is fine"),
            "the page must show what the file has: {published}"
        );
    }

    #[test]
    fn only_the_human_half_is_offered_to_the_browser() {
        let dir = temp_dir("module");
        let (state, _sse) = state(&dir);
        let module = BoardExtension::new(state)
            .client_module()
            .expect("the board has a browser half");
        let offered = module
            .actions
            .expect("composition requires explicit ownership");
        assert_eq!(
            offered
                .iter()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "atlas_cement",
                "board_clear",
                "board_compose",
                "board_mark",
                "board_seen",
                "board_unmark",
            ]),
            "the module may only own its human actions"
        );
        for agent_action in [
            "atlas_cement_propose",
            "board_assert",
            "board_read",
            "board_retract",
            "board_subject",
        ] {
            assert!(
                !offered.contains(&agent_action.to_string()),
                "{agent_action} must not be loadable from the page"
            );
        }
    }

    #[test]
    fn the_agent_can_preview_cement_but_only_the_human_action_writes_it() {
        let dir = temp_dir("cement-actions");
        let (state, _sse) = state(&dir);
        let defs = actions(&state);
        state
            .write(|board| {
                board.assert(
                    Assertion::Claim {
                        id: "settled".to_string(),
                        text: "the human signed this".to_string(),
                        status: Default::default(),
                    },
                    Author::Named("reviewer".to_string()),
                )?;
                board.mark("settled", Mark::Agree, None)?;
                Ok(())
            })
            .expect("settled board");

        let target = dir.join("cemented");
        let preview = call(&state, &defs, "atlas_cement_propose", json!({}))
            .expect("proposal")
            .expect("proposal text");
        assert!(preview.contains("\"obligations-v0.1.json\""), "{preview}");
        assert!(preview.contains("\"cement-receipt.json\""), "{preview}");
        assert!(!target.exists(), "the MCP proposal must write nothing");

        let result = call(
            &state,
            &defs,
            "atlas_cement",
            json!({ "directory": target }),
        )
        .expect("human cement")
        .expect("cement result");
        assert!(result.contains("cemented"), "{result}");
        assert!(target.join("obligations-v0.1.json").is_file());
        assert!(target.join("cement-receipt.json").is_file());
    }
}
