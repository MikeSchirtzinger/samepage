//! Narration + barge-in chrome — lifted, behavior-preserving, from teaching-
//! canvas's `main.rs` (M2 Phase C; see `docs/ag-ui-surface-m2-handoff.md` §2's
//! `Narration`/`narrate_*`/`tutor_event`/`push_history`/`canvas_event`/
//! `interrupt_tx` rows, and §4 step 6 / §5 open risk #6).
//!
//! **Unification, not duplication.** Before this move, `acp.rs::run_turn` and
//! `openai.rs::run_turn` each called this exact sequence independently —
//! identical code, copy-pasted across both backends. This module is the ONE
//! implementation both `turn_loop::acp` and `turn_loop::openai` now call; the
//! duplication is gone, not dragged in twice.
//!
//! **Display and speech are separate contracts.** Display uses AG-UI's native
//! `TEXT_MESSAGE_START` / `TEXT_MESSAGE_CONTENT` / `TEXT_MESSAGE_END` lifecycle
//! so one model turn fills one browser message. Speech still buffers raw deltas
//! into whole sentences because TTS needs sentence-sized input. Note this module
//! does NOT touch application-specific `"canvas.chat"`/
//! `"canvas.widget"`/`"canvas.clear"` — those are teaching-canvas's own
//! `CanvasState::emit` calls (`examples/teaching-canvas/src/canvas_surface.rs`),
//! a Surface's own chrome, out of this module's scope entirely.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::warn;

use ag_ui_core::event::{
    BaseEvent, CustomEvent, Event as AgUiEvent, TextMessageContentEvent, TextMessageEndEvent,
    TextMessageStartEvent,
};
use ag_ui_core::types::MessageId;
use ag_ui_core::JsonValue;

use crate::runtime_state::{ActiveMessage, RuntimeState};
use crate::{ActivityOutcome, Caller};

/// A command to the single narration-audio worker.
pub enum Narration {
    /// Speak this line (queued behind whatever is already playing).
    Say(String),
    /// Drop everything still queued (a new question, or audio toggled off).
    Hush,
}

/// Max transcript lines retained for replay to a reconnecting client.
const HISTORY_CAP: usize = 80;

/// Append one raw model delta to the active AG-UI assistant message and, in a
/// completely separate buffer, queue whole sentences for speech. Suppressed
/// while the session is priming.
pub fn narrate_chunk(rt: &Arc<RuntimeState>, text: &str) {
    if text.is_empty() {
        return;
    }

    let _replay_guard = rt.transcript_replay_lock.lock();
    narrate_chunk_locked(rt, text);
}

/// Process-provider streamed delta gate. Session authorization is checked under
/// the same transcript boundary as the append, so cancellation cannot revoke
/// an old prompt between a preliminary check and the actual mutation.
pub(crate) fn narrate_provider_chunk(rt: &Arc<RuntimeState>, session_id: &str, text: &str) {
    narrate_provider_chunk_with_boundary_hook(rt, session_id, text, || {});
}

pub(crate) fn narrate_provider_chunk_with_boundary_hook(
    rt: &Arc<RuntimeState>,
    session_id: &str,
    text: &str,
    after_boundary: impl FnOnce(),
) {
    if text.is_empty() {
        return;
    }
    let _replay_guard = rt.transcript_replay_lock.lock();
    // The test hook makes the formerly-racy check/append interleaving
    // deterministic. Production passes a no-op; authorization remains checked
    // only after acquiring the transcript boundary.
    after_boundary();
    if rt.stream_active_session.lock().as_deref() != Some(session_id) {
        return;
    }
    narrate_chunk_locked(rt, text);
}

fn narrate_chunk_locked(rt: &Arc<RuntimeState>, text: &str) {
    // Re-check while holding the same boundary used by cancel/flush. An ACP
    // update already queued on the reader task after barge-in must not reopen
    // a closed message or refill the speech buffer.
    if rt.warming.load(Ordering::Relaxed) || !rt.busy.load(Ordering::Relaxed) {
        return;
    }
    let (message_id, started) = {
        let mut active = rt.active_message.lock();
        let started = active.is_none();
        let message = active.get_or_insert_with(|| ActiveMessage {
            id: MessageId::random(),
            text: String::new(),
        });
        message.text.push_str(text);
        (message.id.clone(), started)
    };
    if started {
        let run_id = rt.current_activity_run.lock().clone();
        let message_id_text = message_id.to_string();
        rt.activity.record_message(
            Caller::Agent,
            "message.assistant_started",
            ActivityOutcome::Started,
            run_id.as_deref(),
            Some(&message_id_text),
            None,
        );
        emit_event(
            rt,
            AgUiEvent::<JsonValue>::TextMessageStart(TextMessageStartEvent::new(
                message_id.clone(),
            )),
        );
    }
    if let Ok(event) = TextMessageContentEvent::new(message_id, text.to_string()) {
        emit_event(rt, AgUiEvent::<JsonValue>::TextMessageContent(event));
    }
    // Speech keeps its sentence boundary buffer; it no longer controls how the
    // browser chunks messages.
    let mut sentences = Vec::new();
    {
        let mut buf = rt.narration_buf.lock();
        buf.push_str(text);
        while let Some(end) = sentence_break(&buf) {
            let s: String = buf.drain(..=end).collect();
            let t = s.trim();
            if !t.is_empty() {
                sentences.push(t.to_string());
            }
        }
    }
    for s in sentences {
        if rt.audio_on.load(Ordering::Relaxed) {
            speak_text(rt, &sanitize_for_speech(&s));
        }
    }
}

/// Finish a turn: speak whatever remains in the sentence buffer, persist the
/// full assistant message once, and close its AG-UI message lifecycle.
pub fn narrate_flush(rt: &Arc<RuntimeState>) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    let rem = {
        let mut buf = rt.narration_buf.lock();
        std::mem::take(&mut *buf)
    };
    let t = rem.trim();
    if !t.is_empty() && rt.audio_on.load(Ordering::Relaxed) {
        speak_text(rt, &sanitize_for_speech(t));
    }
    finish_active_message_locked(rt, ActivityOutcome::Succeeded);
}

/// Barge-in variant: close the partially streamed display message so clients
/// never retain a dangling stream, but discard unspoken sentence fragments.
pub fn narrate_cancel(rt: &Arc<RuntimeState>) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    let _ = rt.narr_tx.send(Narration::Hush);
    rt.narration_buf.lock().clear();
    finish_active_message_locked(rt, ActivityOutcome::Cancelled);
}

/// Stop current/queued audio at the same ordering boundary used by sentence
/// producers. A `Say` is therefore always wholly before or wholly after this
/// Hush; audio-disable sets `audio_on=false` before calling it, so no later Say
/// can be produced until audio is re-enabled.
pub fn hush(rt: &Arc<RuntimeState>) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    let _ = rt.narr_tx.send(Narration::Hush);
}

/// Clear any stale buffers while (re)priming a provider. No end event is sent:
/// this is only used at a session boundary where no turn should be active.
pub fn reset_narration(rt: &Arc<RuntimeState>) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    rt.narration_buf.lock().clear();
    *rt.active_message.lock() = None;
}

/// Byte index of the next sentence end: a newline, or `.`/`!`/`?` followed by
/// whitespace (so "0.5" and "Dr. " don't split mid-number/abbrev awkwardly —
/// a trailing sentence with no following space is flushed at turn end).
fn sentence_break(buf: &str) -> Option<usize> {
    let bytes = buf.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            return Some(i);
        }
        if matches!(b, b'.' | b'!' | b'?') {
            if let Some(next) = bytes.get(i + 1) {
                if next.is_ascii_whitespace() {
                    return Some(i);
                }
            }
        }
    }
    None
}

/// Emit a complete one-shot assistant message (configuration errors, provider
/// failures). Normal model output should use [`narrate_chunk`] +
/// [`narrate_flush`] so it streams incrementally.
pub fn emit_narrate(rt: &Arc<RuntimeState>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let _replay_guard = rt.transcript_replay_lock.lock();
    finish_active_message_locked(rt, ActivityOutcome::Cancelled);
    let id = MessageId::random();
    push_history_locked(rt, "tutor", text);
    emit_event(
        rt,
        AgUiEvent::<JsonValue>::TextMessageStart(TextMessageStartEvent::new(id.clone())),
    );
    if let Ok(event) = TextMessageContentEvent::new(id.clone(), text.to_string()) {
        emit_event(rt, AgUiEvent::<JsonValue>::TextMessageContent(event));
    }
    emit_event(
        rt,
        AgUiEvent::<JsonValue>::TextMessageEnd(TextMessageEndEvent {
            base: BaseEvent::default(),
            message_id: id.clone(),
        }),
    );
    let run_id = rt.current_activity_run.lock().clone();
    let message_id = id.to_string();
    rt.activity.record_host_message(
        "message.host_notice",
        ActivityOutcome::Succeeded,
        run_id.as_deref(),
        Some(&message_id),
        Some(&format!("{} characters", text.chars().count())),
    );
}

fn finish_active_message_locked(rt: &Arc<RuntimeState>, outcome: ActivityOutcome) {
    let Some(message) = rt.active_message.lock().take() else {
        return;
    };
    if !message.text.trim().is_empty() {
        push_history_locked(rt, "tutor", &message.text);
    }
    emit_event(
        rt,
        AgUiEvent::<JsonValue>::TextMessageEnd(TextMessageEndEvent {
            base: BaseEvent::default(),
            message_id: message.id.clone(),
        }),
    );
    let run_id = rt.current_activity_run.lock().clone();
    let message_id = message.id.to_string();
    rt.activity.record_message(
        Caller::Agent,
        "message.assistant_finished",
        outcome,
        run_id.as_deref(),
        Some(&message_id),
        Some(&format!("{} characters", message.text.chars().count())),
    );
}

pub(crate) fn emit_event(rt: &RuntimeState, event: AgUiEvent<JsonValue>) {
    if let Ok(json) = serde_json::to_string(&event) {
        let _ = rt.sse_tx.send(json);
    }
}

/// Reconstruct an in-flight assistant message for a newly connected SSE
/// client. Called after replaying completed history, so the partial bubble is
/// appended rather than wiped by `surface.history`.
pub fn active_message_events(rt: &RuntimeState) -> Vec<String> {
    let Some(message) = rt.active_message.lock().clone() else {
        return Vec::new();
    };
    let events = [
        AgUiEvent::<JsonValue>::TextMessageStart(TextMessageStartEvent::new(message.id.clone())),
        AgUiEvent::<JsonValue>::TextMessageContent(TextMessageContentEvent {
            base: BaseEvent::default(),
            message_id: message.id,
            delta: message.text,
        }),
    ];
    events
        .iter()
        .filter_map(|event| serde_json::to_string(event).ok())
        .collect()
}

/// Append one line to the replayable transcript ring buffer (agent or human).
pub fn push_history(rt: &Arc<RuntimeState>, by: &str, text: &str) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    push_history_locked(rt, by, text);
}

/// Append a transcript entry and broadcast its live custom-event projection
/// at one reconnect boundary. A client sees the entry either in history or in
/// the queued live event, never both because it connected between the two.
pub fn push_history_and_surface_event(
    rt: &Arc<RuntimeState>,
    by: &str,
    text: &str,
    event_name: &str,
    event_value: JsonValue,
) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    push_history_locked(rt, by, text);
    surface_event(rt, event_name, event_value);
}

/// Begin a human-authored turn as one reconnect transaction: stop old audio,
/// append the transcript line, optionally project it to live clients, and mark
/// the tutor thinking. A reconnect therefore receives the human line either
/// from replay or from the queued live event; it cannot observe only the later
/// tutor-state transition.
pub fn begin_human_turn(
    rt: &Arc<RuntimeState>,
    by: &str,
    text: &str,
    live_event: Option<(&str, JsonValue)>,
    question: Option<&str>,
) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    let run_id = format!("run-{}", uuid::Uuid::new_v4().simple());
    *rt.current_activity_run.lock() = Some(run_id.clone());
    rt.activity.record_message(
        Caller::Human,
        "message.human_submitted",
        ActivityOutcome::Started,
        Some(&run_id),
        None,
        Some(&format!("{} characters", text.chars().count())),
    );
    rt.busy.store(true, Ordering::SeqCst);
    let _ = rt.narr_tx.send(Narration::Hush);
    push_history_locked(rt, by, text);
    if let Some((name, value)) = live_event {
        surface_event(rt, name, value);
    }
    tutor_event_locked(rt, "thinking", question);
}

fn push_history_locked(rt: &RuntimeState, by: &str, text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    let mut h = rt.history.lock();
    h.push_back(serde_json::json!({ "by": by, "text": t }));
    while h.len() > HISTORY_CAP {
        h.pop_front();
    }
}

/// Broadcast the turn-loop's lifecycle state (`warming`/`ready`/`thinking`/
/// `done`/`awaiting`/`failed`) as a `surface.tutor` SSE custom event, keeping the
/// replayable "your turn" flag ([`RuntimeState::awaiting`]) in sync: a recall
/// question sets it; a new turn (`thinking`) or a fresh prime (`warming`)
/// clears it. `done`/`ready` leave it alone — a turn that ended on a question
/// stays awaiting.
pub fn tutor_event(rt: &Arc<RuntimeState>, tutor_state: &str, question: Option<&str>) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    tutor_event_locked(rt, tutor_state, question);
}

/// Publish the terminal turn event and make idle observable as one reconnect
/// transition. A subscriber receives `done` live or snapshots the idle state
/// after this boundary; it cannot miss `done` while replaying `thinking`.
pub fn finish_turn(rt: &Arc<RuntimeState>) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    rt.busy.store(false, Ordering::SeqCst);
    tutor_event_locked(rt, "done", None);
    *rt.current_activity_run.lock() = None;
}

/// Settle a turn that was accepted and projected as `thinking` but could not
/// be handed to a provider. This shares the replay boundary with normal
/// completion so reconnects never remain stuck on a phantom active turn.
pub fn fail_turn(rt: &Arc<RuntimeState>, error: &str) {
    let _replay_guard = rt.transcript_replay_lock.lock();
    let _ = rt.narr_tx.send(Narration::Hush);
    rt.narration_buf.lock().clear();
    finish_active_message_locked(rt, ActivityOutcome::Failed);
    rt.ready.store(false, Ordering::Relaxed);
    rt.warming.store(false, Ordering::Relaxed);
    *rt.provider_error.lock() = Some(error.to_string());
    rt.busy.store(false, Ordering::SeqCst);
    tutor_event_locked(rt, "failed", Some(error));
    *rt.current_activity_run.lock() = None;
}

fn tutor_event_locked(rt: &Arc<RuntimeState>, tutor_state: &str, question: Option<&str>) {
    match tutor_state {
        "awaiting" => rt.awaiting.store(true, Ordering::Relaxed),
        "thinking" | "warming" | "failed" => rt.awaiting.store(false, Ordering::Relaxed),
        _ => {}
    }
    surface_event(
        rt,
        "surface.tutor",
        serde_json::json!({ "state": tutor_state, "question": question }),
    );
    let run_id = rt.current_activity_run.lock().clone();
    let outcome = match tutor_state {
        "warming" | "thinking" | "awaiting" => ActivityOutcome::Started,
        "ready" | "done" => ActivityOutcome::Succeeded,
        "failed" => ActivityOutcome::Failed,
        _ => ActivityOutcome::Info,
    };
    rt.activity.record_lifecycle(
        &format!("lifecycle.{tutor_state}"),
        outcome,
        run_id.as_deref(),
        question,
    );
    if tutor_state == "ready" {
        let provider = rt.provider.lock().clone();
        rt.activity.record_provider(
            "provider.ready",
            ActivityOutcome::Succeeded,
            run_id.as_deref(),
            &provider,
        );
    } else if tutor_state == "failed" {
        rt.activity.record_failure(
            "lifecycle.failed",
            None,
            run_id.as_deref(),
            question.unwrap_or("runtime lifecycle failed"),
        );
    }
}

/// Broadcast a `surface.*` custom event over SSE.
pub fn surface_event(rt: &Arc<RuntimeState>, name: &str, value: JsonValue) {
    let event = AgUiEvent::<JsonValue>::Custom(CustomEvent {
        base: BaseEvent::default(),
        name: name.to_string(),
        value,
    });
    if let Ok(json) = serde_json::to_string(&event) {
        let _ = rt.sse_tx.send(json);
    }
}

fn speak_text(rt: &Arc<RuntimeState>, text: &str) {
    let _ = rt.narr_tx.send(Narration::Say(text.to_string()));
}

/// Play spoken narration **serially** — each line to completion before the
/// next. `speak` POSTs to Kokoro, writes a single shared temp mp3, then plays
/// it via a *detached* `afplay` and only exits once that playback finishes.
/// Spawning one `speak` per sentence without waiting therefore (a) overlaps
/// voices and (b) races on the shared temp file — exactly the "talking over
/// each other" bug. Awaiting each `speak` process serializes playback and
/// removes the race, since only one `speak`/`afplay` is ever live at a time.
pub async fn narration_worker(mut rx: mpsc::UnboundedReceiver<Narration>) {
    narration_worker_with_command(&mut rx, std::path::Path::new("speak"), &[]).await;
}

async fn narration_worker_with_command(
    rx: &mut mpsc::UnboundedReceiver<Narration>,
    program: &std::path::Path,
    prefix_args: &[String],
) {
    let mut queue: VecDeque<String> = VecDeque::new();
    loop {
        // Block for the next line when idle.
        if queue.is_empty() {
            match rx.recv().await {
                None => return,
                Some(Narration::Say(text)) => queue.push_back(text),
                Some(Narration::Hush) => continue,
            }
        }
        // Honor any Hush that already arrived before committing to speak.
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Narration::Say(text) => queue.push_back(text),
                Narration::Hush => queue.clear(),
            }
        }
        let Some(text) = queue.pop_front() else {
            continue;
        };
        let mut command = tokio::process::Command::new(program);
        command
            .args(prefix_args)
            .arg(&text)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                warn!(
                    program = %program.display(),
                    %error,
                    "narration audio: failed to spawn speaker"
                );
                continue;
            }
        };
        let process_group = child.id().and_then(|pid| i32::try_from(pid).ok());
        let mut speaker = SpeakerProcess {
            child,
            process_group,
        };

        // Keep consuming commands while playback is active. Hush must stop the
        // current speaker and all descendants (the local `speak` command owns
        // an `afplay` child), not merely clear lines queued behind it.
        loop {
            tokio::select! {
                result = speaker.child.wait() => {
                    if let Err(error) = result {
                        warn!(%error, "narration audio: failed to reap speaker");
                    }
                    break;
                }
                command = rx.recv() => match command {
                    Some(Narration::Say(text)) => queue.push_back(text),
                    Some(Narration::Hush) => {
                        queue.clear();
                        terminate_speaker(&mut speaker).await;
                        break;
                    }
                    None => {
                        terminate_speaker(&mut speaker).await;
                        return;
                    }
                }
            }
        }
    }
}

struct SpeakerProcess {
    child: tokio::process::Child,
    process_group: Option<i32>,
}

impl Drop for SpeakerProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            if process_group_exists(process_group) {
                signal_speaker_group(process_group, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}

async fn terminate_speaker(speaker: &mut SpeakerProcess) {
    #[cfg(unix)]
    if let Some(process_group) = speaker.process_group {
        signal_speaker_group(process_group, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = speaker.child.start_kill();

    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), speaker.child.wait()).await;

    #[cfg(unix)]
    if let Some(process_group) = speaker.process_group {
        if !wait_for_process_group_exit(process_group, std::time::Duration::from_millis(500)).await
        {
            signal_speaker_group(process_group, libc::SIGKILL);
            let _ =
                wait_for_process_group_exit(process_group, std::time::Duration::from_secs(1)).await;
        }
    }
    #[cfg(not(unix))]
    let _ = speaker.child.start_kill();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), speaker.child.wait()).await;
}

#[cfg(unix)]
fn signal_speaker_group(process_group: i32, signal: libc::c_int) {
    if process_group <= 0 {
        return;
    }
    // SAFETY: `process_group` came from the positive PID of a child launched
    // with `process_group(0)`; negating it addresses that isolated group only.
    let result = unsafe { libc::kill(-process_group, signal) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            warn!(process_group, %error, "failed to signal narration process group");
        }
    }
}

#[cfg(unix)]
fn process_group_exists(process_group: i32) -> bool {
    if process_group <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs an existence/permission check only. The
    // negative id addresses the isolated child process group.
    let result = unsafe { libc::kill(-process_group, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
async fn wait_for_process_group_exit(process_group: i32, timeout: std::time::Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while process_group_exists(process_group) {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    true
}

fn sanitize_for_speech(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '#' | '>' | '|' | '~'))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    #[test]
    fn streamed_deltas_form_one_agui_message_and_one_history_entry() {
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, mut sse_rx) = tokio::sync::broadcast::channel(8);
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let auth = Arc::new(
            crate::auth::AuthStore::open(
                std::env::temp_dir().join("ag-ui-narration-test-missing-auth.json"),
            )
            .expect("temporary auth store should open"),
        );
        let byok = crate::turn_loop::openai::ByokConfig {
            base_url: "http://localhost".into(),
            model: "test".into(),
        };
        let (rt, _channels) = crate::runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history.clone(),
            Vec::new(),
            auth,
            "test".into(),
            byok,
            false,
            0,
        );
        rt.warming.store(false, Ordering::Relaxed);
        rt.busy.store(true, Ordering::Relaxed);

        narrate_chunk(&rt, "Hello ");
        narrate_chunk(&rt, "world.");
        narrate_flush(&rt);

        let events: Vec<serde_json::Value> = (0..4)
            .map(|_| serde_json::from_str(&sse_rx.try_recv().unwrap()).unwrap())
            .collect();
        assert_eq!(events[0]["type"], "TEXT_MESSAGE_START");
        assert_eq!(events[1]["type"], "TEXT_MESSAGE_CONTENT");
        assert_eq!(events[1]["delta"], "Hello ");
        assert_eq!(events[2]["delta"], "world.");
        assert_eq!(events[3]["type"], "TEXT_MESSAGE_END");
        let id = events[0]["messageId"].clone();
        assert!(events.iter().all(|event| event["messageId"] == id));

        let history = history.lock();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["text"], "Hello world.");
    }

    #[test]
    fn reconnect_boundary_replays_or_queues_each_delta_exactly_once() {
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _) = tokio::sync::broadcast::channel(8);
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-narration-boundary-test-{}.json",
                std::process::id()
            )))
            .expect("temporary auth store should open"),
        );
        let byok = crate::turn_loop::openai::ByokConfig {
            base_url: "http://localhost".into(),
            model: "test".into(),
        };
        let (rt, _channels) = crate::runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history.clone(),
            Vec::new(),
            auth,
            "test".into(),
            byok,
            false,
            0,
        );
        rt.warming.store(false, Ordering::Relaxed);
        rt.busy.store(true, Ordering::Relaxed);
        narrate_chunk(&rt, "A");

        // Freeze the exact lock used by the SSE boundary. The second writer
        // announces that it has started, then must remain blocked until after
        // the receiver and active-message replay have been captured.
        let replay_guard = rt.transcript_replay_lock.lock();
        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let writer_rt = rt.clone();
        let writer = std::thread::spawn(move || {
            attempted_tx.send(()).expect("announce writer");
            narrate_chunk(&writer_rt, "B");
        });
        attempted_rx.recv().expect("writer reached boundary");
        let mut rx = rt.sse_tx.subscribe();
        let replay = active_message_events(rt.as_ref());
        drop(replay_guard);
        writer.join().expect("delta writer should finish");

        let replay_delta = replay
            .iter()
            .filter_map(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .find(|event| event["type"] == "TEXT_MESSAGE_CONTENT")
            .and_then(|event| event["delta"].as_str().map(str::to_string))
            .expect("active replay contains delta A");
        let queued: serde_json::Value =
            serde_json::from_str(&rx.try_recv().expect("delta B is queued")).unwrap();
        assert_eq!(queued["type"], "TEXT_MESSAGE_CONTENT");
        let queued_delta = queued["delta"].as_str().unwrap();
        assert_eq!(format!("{replay_delta}{queued_delta}"), "AB");

        narrate_flush(&rt);
        assert!(active_message_events(rt.as_ref()).is_empty());
        assert_eq!(history.lock()[0]["text"], "AB");
    }

    #[test]
    fn late_update_after_cancel_cannot_reopen_display_or_speech_state() {
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, mut sse_rx) = tokio::sync::broadcast::channel(8);
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-narration-cancel-test-{}.json",
                std::process::id()
            )))
            .expect("temporary auth store should open"),
        );
        let (rt, _channels) = crate::runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history.clone(),
            Vec::new(),
            auth,
            "test".into(),
            crate::turn_loop::openai::ByokConfig {
                base_url: "http://localhost".into(),
                model: "test".into(),
            },
            false,
            0,
        );
        rt.warming.store(false, Ordering::Relaxed);
        rt.busy.store(true, Ordering::Relaxed);
        narrate_chunk(&rt, "partial");
        rt.busy.store(false, Ordering::Relaxed);
        narrate_cancel(&rt);

        while sse_rx.try_recv().is_ok() {}
        narrate_chunk(&rt, "late update");
        assert!(rt.active_message.lock().is_none());
        assert!(rt.narration_buf.lock().is_empty());
        assert!(sse_rx.try_recv().is_err());
        assert_eq!(history.lock().len(), 1);
        assert_eq!(history.lock()[0]["text"], "partial");
    }

    #[test]
    fn sentence_then_cancel_orders_say_before_hush_and_suppresses_late_output() {
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _) = tokio::sync::broadcast::channel(8);
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-narration-order-test-{}.json",
                std::process::id()
            )))
            .expect("temporary auth store should open"),
        );
        let (rt, mut channels) = crate::runtime_state::RuntimeState::new(
            ws_tx,
            sse_tx,
            history,
            Vec::new(),
            auth,
            "test".into(),
            crate::turn_loop::openai::ByokConfig {
                base_url: "http://localhost".into(),
                model: "test".into(),
            },
            true,
            0,
        );
        rt.warming.store(false, Ordering::Relaxed);
        rt.busy.store(true, Ordering::Relaxed);

        narrate_chunk(&rt, "first sentence. ");
        narrate_cancel(&rt);
        finish_turn(&rt);
        narrate_chunk(&rt, "late sentence. ");

        assert!(matches!(
            channels.narr_rx.try_recv(),
            Ok(Narration::Say(text)) if text == "first sentence."
        ));
        assert!(matches!(channels.narr_rx.try_recv(), Ok(Narration::Hush)));
        assert!(channels.narr_rx.try_recv().is_err());
    }

    #[cfg(unix)]
    async fn wait_for_recorded_pid(path: &std::path::Path) -> libc::pid_t {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(value) = std::fs::read_to_string(path) {
                    if let Ok(pid) = value.trim().parse() {
                        return pid;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake speaker should record its pid")
    }

    #[cfg(unix)]
    async fn wait_for_pid_exit(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            // SAFETY: signal 0 only checks whether the recorded test process
            // still exists; it cannot alter that process.
            let result = unsafe { libc::kill(pid, 0) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    fn fake_speaker_prefix() -> Vec<String> {
        vec![
            "-c".to_string(),
            concat!(
                "printf '%s' \"$$\" > \"${1}.parent\"; ",
                "(trap '' TERM; sleep 30) & ",
                "child=$!; printf '%s' \"$child\" > \"${1}.child\"; ",
                "trap 'exit 0' TERM; wait \"$child\""
            )
            .to_string(),
            "ag-ui-fake-speaker".to_string(),
        ]
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hush_kills_term_ignoring_speaker_group_clears_queue_and_worker_recovers() {
        let root = std::env::temp_dir().join(format!(
            "ag-ui-narration-worker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create worker fixture");
        let first = root.join("first");
        let queued = root.join("queued");
        let later = root.join("later");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prefix = fake_speaker_prefix();
        let worker = tokio::spawn(async move {
            narration_worker_with_command(&mut rx, std::path::Path::new("/bin/sh"), &prefix).await;
        });

        tx.send(Narration::Say(first.to_string_lossy().into_owned()))
            .expect("start fake speaker");
        let parent = wait_for_recorded_pid(&first.with_extension("parent")).await;
        let child = wait_for_recorded_pid(&first.with_extension("child")).await;
        tx.send(Narration::Say(queued.to_string_lossy().into_owned()))
            .expect("queue second line");
        tx.send(Narration::Hush).expect("interrupt speaker");

        assert!(
            wait_for_pid_exit(parent).await,
            "speaker wrapper survived Hush"
        );
        assert!(
            wait_for_pid_exit(child).await,
            "TERM-ignoring speaker descendant survived KILL fallback"
        );
        assert!(!process_group_exists(parent));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !queued.with_extension("parent").exists(),
            "Hush did not clear queued speech"
        );

        tx.send(Narration::Say(later.to_string_lossy().into_owned()))
            .expect("worker accepts later speech");
        let later_parent = wait_for_recorded_pid(&later.with_extension("parent")).await;
        let later_child = wait_for_recorded_pid(&later.with_extension("child")).await;
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(3), worker)
            .await
            .expect("worker should stop when channel closes")
            .expect("worker task should not panic");
        assert!(wait_for_pid_exit(later_parent).await);
        assert!(wait_for_pid_exit(later_child).await);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worker_task_cancellation_kills_owned_speaker_process_group() {
        let root = std::env::temp_dir().join(format!(
            "ag-ui-narration-cancel-worker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create cancel fixture");
        let active = root.join("active");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let prefix = fake_speaker_prefix();
        let worker = tokio::spawn(async move {
            narration_worker_with_command(&mut rx, std::path::Path::new("/bin/sh"), &prefix).await;
        });
        tx.send(Narration::Say(active.to_string_lossy().into_owned()))
            .expect("start cancellable fake speaker");
        let parent = wait_for_recorded_pid(&active.with_extension("parent")).await;
        let child = wait_for_recorded_pid(&active.with_extension("child")).await;

        worker.abort();
        let _ = worker.await;
        assert!(
            wait_for_pid_exit(parent).await,
            "wrapper survived task abort"
        );
        assert!(
            wait_for_pid_exit(child).await,
            "descendant survived task abort"
        );
        assert!(!process_group_exists(parent));
        drop(tx);
        let _ = std::fs::remove_dir_all(root);
    }
}
