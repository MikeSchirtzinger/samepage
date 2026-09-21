//! Chat that reaches an agent attached over `/mcp`.
//!
//! The conversation panel used to have exactly one destination: whatever
//! in-page provider the runtime had primed. On a terminal-first instance that
//! provider is a placeholder pointed at a dead endpoint, so the strongest
//! affordance on the page, the box that looks like talking to somebody, was
//! the one lane that could not work. A person typed a question, the socket
//! refused, and the failure was rendered in words that named a different
//! subsystem ("No agent is connected") while an agent was in fact attached and
//! working three columns away.
//!
//! This module makes an attached agent a first-class chat responder. A chat
//! message that has no live in-page provider is deposited here instead of being
//! thrown at a dead endpoint; `chat_read` hands it to any attached agent (and
//! blocks until one arrives, so an agent can park on chat the way it parks on
//! the document); `chat_reply` posts the answer back into the human transcript
//! under the answering agent's own byline.
//!
//! Deliberately in the crate rather than in an application: the conversation
//! panel is runtime chrome, so every Surface that serves people gets this the
//! moment it serves an agent too.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::Notify;

use crate::runtime_state::RuntimeState;
use crate::{
    ActionRouteDef, Actor, Caller, ClientModule, Effect, RouteDef, SemanticTarget,
    SemanticTargetRef, Surface, SurfaceState, SurfaceStore, ToolDef,
};

/// Tool an attached agent calls to collect chat nobody has answered.
pub const READ_ACTION: &str = "chat_read";
/// Tool an attached agent calls to answer, in the human's transcript.
pub const REPLY_ACTION: &str = "chat_reply";

/// Longest a single `chat_read` may park. Mirrors the atlas `await_atlas` cap
/// so an agent can hold one long block on either lane.
pub const MAX_WAIT_SECONDS: u64 = 600;
/// Default park when the caller does not say.
const DEFAULT_WAIT_SECONDS: u64 = 60;
/// Chat kept for an agent that has not looked yet. Old messages fall off the
/// back rather than growing without bound; a person who asked ten minutes and
/// a hundred messages ago is not waiting on this answer.
const MAX_PENDING: usize = 100;
const MAX_REPLY_CHARS: usize = 4000;

/// One chat message waiting for an answer.
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub id: u64,
    /// Display name of the person who typed it.
    pub from: String,
    /// Host-minted participant id when the sender is a joined person.
    pub from_id: Option<String>,
    /// Exactly what they sent, plus any resolved-referent preamble the runtime
    /// splices in when they were pointing at something.
    pub text: String,
    posted: Instant,
    /// Set the first time an agent read it, so a blocking read parks instead of
    /// spinning on the same backlog.
    delivered: bool,
}

impl ChatTurn {
    pub fn age(&self) -> Duration {
        self.posted.elapsed()
    }
}

/// The inbox itself, plus the wake signal anything else can park on.
pub struct ChatRelay {
    pending: Mutex<VecDeque<ChatTurn>>,
    next_id: AtomicU64,
    /// Notified whenever a chat message arrives. Public through
    /// [`ChatRelay::changed`] so another await tool (the board's) can select on
    /// chat as well and a blocked agent wakes for either.
    changed: Notify,
    /// The last agent seen dispatching a tool call, for the reply's byline.
    caller: Mutex<Option<Actor>>,
    /// Set once the runtime exists; the reply posts through it.
    runtime: Mutex<Option<Weak<RuntimeState>>>,
}

impl Default for ChatRelay {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatRelay {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            changed: Notify::new(),
            caller: Mutex::new(None),
            runtime: Mutex::new(None),
        }
    }

    /// Give the relay the runtime it posts replies through. `Weak` on purpose:
    /// the runtime holds the relay, and two `Arc`s pointing at each other is a
    /// process that never frees either.
    pub fn attach_runtime(&self, runtime: Weak<RuntimeState>) {
        *self.runtime.lock() = Some(runtime);
    }

    /// Wake signal. Park on `relay.changed().notified()` BEFORE checking
    /// [`ChatRelay::has_unread`], or a message that lands between the check and
    /// the await is lost.
    pub fn changed(&self) -> &Notify {
        &self.changed
    }

    /// Is there chat no agent has collected yet?
    pub fn has_unread(&self) -> bool {
        self.pending.lock().iter().any(|turn| !turn.delivered)
    }

    /// Everything still waiting for an answer, collected or not.
    pub fn waiting(&self) -> Vec<ChatTurn> {
        self.pending.lock().iter().cloned().collect()
    }

    /// Deposit one chat message for the attached agents. Returns its id.
    pub fn post(&self, from: &str, from_id: Option<&str>, text: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut pending = self.pending.lock();
            pending.push_back(ChatTurn {
                id,
                from: from.to_string(),
                from_id: from_id.map(str::to_string),
                text: text.to_string(),
                posted: Instant::now(),
                delivered: false,
            });
            while pending.len() > MAX_PENDING {
                pending.pop_front();
            }
        }
        self.changed.notify_waiters();
        id
    }

    /// Record who is calling, so a reply can be signed. Only agents are kept:
    /// a person's click must never end up as the byline on an agent's answer.
    pub fn note_caller(&self, actor: &Actor) {
        if actor.caller == Caller::Agent {
            *self.caller.lock() = Some(actor.clone());
        }
    }

    /// The name to sign the next reply with.
    fn byline(&self) -> String {
        self.caller
            .lock()
            .as_ref()
            .map(|actor| actor.byline().to_string())
            .unwrap_or_else(|| "agent".to_string())
    }

    /// Mark everything currently waiting as collected and render it for the
    /// agent. Returns the read-back text.
    fn collect(&self) -> String {
        let mut pending = self.pending.lock();
        if pending.is_empty() {
            return "No chat messages are waiting. Nobody has typed into the \
                    conversation panel since your last read."
                .to_string();
        }
        let total = pending.len();
        let mut lines = vec![format!(
            "{total} chat {} waiting for an answer. Reply with chat_reply and it \
             lands in the human transcript under your name.",
            if total == 1 {
                "message is"
            } else {
                "messages are"
            }
        )];
        for (index, turn) in pending.iter_mut().enumerate() {
            turn.delivered = true;
            lines.push(format!(
                "\n[{}/{}] {} asked {}s ago:\n{}",
                index + 1,
                total,
                turn.from,
                turn.age().as_secs(),
                turn.text
            ));
        }
        lines.join("\n")
    }

    /// Answer whatever is waiting. Posts into the human transcript with the
    /// calling agent's byline and clears the queue.
    fn reply(&self, text: &str) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("chat_reply needs a non-empty text".to_string());
        }
        if text.chars().count() > MAX_REPLY_CHARS {
            return Err(format!(
                "chat_reply text is longer than {MAX_REPLY_CHARS} characters"
            ));
        }
        let runtime = self
            .runtime
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(|| "the runtime is not accepting transcript writes".to_string())?;
        let by = self.byline();
        let answered: Vec<ChatTurn> = self.pending.lock().drain(..).collect();
        crate::narration::push_history_and_surface_event(
            &runtime,
            &by,
            text,
            "surface.narrate",
            json!({ "by": by, "text": text }),
        );
        // The panel put itself into "agent thinking" when the question was
        // relayed. Settle it, or the composer reads as busy forever.
        crate::narration::tutor_event(&runtime, "done", None);
        Ok(match answered.len() {
            0 => format!("Posted into the human transcript as {by}. Nothing was waiting."),
            1 => format!("Posted into the human transcript as {by}, answering 1 chat message."),
            count => format!(
                "Posted into the human transcript as {by}, answering {count} chat messages."
            ),
        })
    }
}

fn wait_seconds(args: &serde_json::Value) -> Result<u64, String> {
    match args.get("seconds") {
        None | Some(serde_json::Value::Null) => Ok(DEFAULT_WAIT_SECONDS),
        Some(value) => {
            let seconds = value
                .as_u64()
                .ok_or_else(|| "`seconds` must be a whole number".to_string())?;
            if seconds == 0 || seconds > MAX_WAIT_SECONDS {
                return Err(format!(
                    "`seconds` must be between 1 and {MAX_WAIT_SECONDS}"
                ));
            }
            Ok(seconds)
        }
    }
}

fn relay_actions(relay: &Arc<ChatRelay>) -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            READ_ACTION,
            "Read chat the people on this page typed into the conversation \
             panel and nobody has answered. This is the same box a person \
             reaches for first, so treat what arrives here as directly \
             addressed to you. Blocks up to `seconds` (default 60, max 600) \
             when nothing new is waiting and returns as soon as somebody \
             sends, so you can park on chat the way you park on the document. \
             Answer with chat_reply.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_WAIT_SECONDS,
                        "description": "How long to wait before returning with nothing new."
                    }
                }
            }),
            {
                let relay = relay.clone();
                move |args: &serde_json::Value| {
                    let seconds = match wait_seconds(args) {
                        Ok(value) => value,
                        Err(error) => return Effect::Reject(error),
                    };
                    let relay = relay.clone();
                    Effect::AsyncQuery(Box::pin(async move {
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
                        loop {
                            // Register before checking so a message that lands
                            // between the check and the await is not lost.
                            let woken = relay.changed().notified();
                            if relay.has_unread() {
                                return Ok(relay.collect());
                            }
                            tokio::select! {
                                _ = woken => {}
                                _ = tokio::time::sleep_until(deadline) => {
                                    return Ok(relay.collect());
                                }
                            }
                        }
                    }))
                }
            },
        )
        .agent_only(),
        ToolDef::new(
            REPLY_ACTION,
            "Answer the people in the conversation panel. The text appears in \
             the human transcript signed with your name, and clears the chat \
             you were holding. Use it for anything you would say out loud; use \
             the surface's own authoring tools for anything you would draw.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_REPLY_CHARS,
                        "description": "What to say, in the human's transcript."
                    }
                }
            }),
            {
                let relay = relay.clone();
                move |args: &serde_json::Value| {
                    let text = args
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let relay = relay.clone();
                    Effect::Mutate(Box::new(move |_state| relay.reply(&text).map(Some)))
                }
            },
        )
        .agent_only(),
    ]
}

struct RelaySurface {
    inner: Arc<dyn Surface>,
    relay: Arc<ChatRelay>,
    tools: Vec<ToolDef>,
}

fn waiting_read_back(relay: &ChatRelay) -> String {
    let waiting = relay.waiting();
    let mut lines = vec![
        "Incoming human chat woke this wait. Call chat_read to collect it, then answer with chat_reply."
            .to_string(),
    ];
    for turn in waiting {
        lines.push(format!(
            "- {} ({} ms ago): {}",
            turn.from,
            turn.age().as_millis(),
            turn.text
        ));
    }
    lines.join("\n")
}

fn install_chat_wake(mut action: ToolDef, relay: &Arc<ChatRelay>) -> ToolDef {
    if !action.wake_on_chat {
        return action;
    }
    let original = action.apply.clone();
    let relay = relay.clone();
    let name = action.name.clone();
    action.apply = Arc::new(move |args| {
        let Effect::AsyncQuery(surface_wait) = original(args) else {
            return Effect::Reject(format!(
                "action {name:?} enables chat wake but did not return an asynchronous query"
            ));
        };
        let relay = relay.clone();
        Effect::AsyncQuery(Box::pin(async move {
            let mut surface_wait = surface_wait;
            loop {
                // Register before checking. A message landing between the
                // predicate and the select must still wake this call.
                let chat_woken = relay.changed().notified();
                if relay.has_unread() {
                    return Ok(waiting_read_back(&relay));
                }
                tokio::select! {
                    result = &mut surface_wait => return result,
                    _ = chat_woken => {}
                }
            }
        }))
    });
    action
}

impl Surface for RelaySurface {
    fn state(&self) -> &dyn SurfaceState {
        self.inner.state()
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

    fn note_caller(&self, actor: &Actor) {
        self.relay.note_caller(actor);
        self.inner.note_caller(actor);
    }

    fn resolve_focus(&self, event: &str, id: &str) -> Result<Option<String>, String> {
        self.inner.resolve_focus(event, id)
    }

    fn bind_semantic_targets(&self, service: &Arc<crate::semantic_targets::SemanticTargetService>) {
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

/// Give a surface the two chat-relay actions. Installed innermost, before the
/// attention host and the activity journal wrap it, so every later wrapper
/// carries the actions and forwards `note_caller` down to the relay.
pub(crate) fn install(
    surface: Arc<dyn Surface>,
) -> Result<(Arc<dyn Surface>, Arc<ChatRelay>), String> {
    if let Some(action) = surface
        .tools()
        .iter()
        .find(|action| action.name == READ_ACTION || action.name == REPLY_ACTION)
    {
        return Err(format!(
            "surface action {:?} collides with a chat-relay action",
            action.name
        ));
    }
    let relay = Arc::new(ChatRelay::new());
    let mut tools = surface
        .tools()
        .iter()
        .cloned()
        .map(|action| install_chat_wake(action, &relay))
        .collect::<Vec<_>>();
    tools.extend(relay_actions(&relay));
    let wrapped: Arc<dyn Surface> = Arc::new(RelaySurface {
        inner: surface,
        relay: relay.clone(),
        tools,
    });
    Ok((wrapped, relay))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posted_message_is_unread_until_an_agent_collects_it() {
        let relay = ChatRelay::new();
        assert!(!relay.has_unread());
        relay.post("Mike", Some("human-1"), "Are you there?");
        assert!(relay.has_unread());
        let read = relay.collect();
        assert!(read.contains("Are you there?"), "{read}");
        assert!(read.contains("Mike"), "{read}");
        assert!(
            !relay.has_unread(),
            "a collected message must not wake the next blocking read"
        );
        assert_eq!(relay.waiting().len(), 1, "it is collected, not answered");
    }

    #[test]
    fn an_empty_inbox_says_so_without_claiming_a_failure() {
        let read = ChatRelay::new().collect();
        assert!(read.contains("No chat messages are waiting"), "{read}");
        assert!(!read.contains("No agent is connected"), "{read}");
    }

    #[test]
    fn the_backlog_is_bounded() {
        let relay = ChatRelay::new();
        for index in 0..(MAX_PENDING + 10) {
            relay.post("Mike", None, &format!("message {index}"));
        }
        assert_eq!(relay.waiting().len(), MAX_PENDING);
        let oldest = relay.waiting().remove(0);
        assert_eq!(oldest.text, "message 10");
    }

    #[test]
    fn a_reply_without_a_runtime_fails_instead_of_dropping_the_answer() {
        let relay = ChatRelay::new();
        relay.post("Mike", None, "hello");
        let error = relay.reply("hi").expect_err("no runtime is attached");
        assert!(error.contains("not accepting transcript writes"), "{error}");
        assert_eq!(
            relay.waiting().len(),
            1,
            "a failed reply must not clear the question"
        );
    }

    #[test]
    fn a_human_caller_never_becomes_an_agent_byline() {
        let relay = ChatRelay::new();
        relay.note_caller(&Actor::anonymous(Caller::Agent));
        relay.note_caller(&Actor::anonymous(Caller::Human));
        assert_eq!(relay.byline(), "agent");
    }

    #[test]
    fn install_adds_both_actions_and_refuses_a_name_collision() {
        struct Bare {
            tools: Vec<ToolDef>,
        }
        struct BareState;
        impl SurfaceState for BareState {
            fn backing(&self) -> crate::StateBacking {
                crate::StateBacking::Ephemeral
            }
            fn describe(&self) -> Result<String, String> {
                Ok(String::new())
            }
            fn snapshot(&self) -> Result<crate::StateSnapshot, String> {
                Ok(crate::StateSnapshot {
                    backing: crate::StateBacking::Ephemeral,
                    body: json!({}),
                    chrome: None,
                })
            }
        }
        impl Surface for Bare {
            fn state(&self) -> &dyn SurfaceState {
                &BareState
            }
            fn tools(&self) -> &[ToolDef] {
                &self.tools
            }
            fn client_modules(&self) -> Vec<ClientModule> {
                Vec::new()
            }
        }

        let (wrapped, _relay) = install(Arc::new(Bare { tools: Vec::new() }))
            .expect("a bare surface accepts the relay");
        let names: Vec<&str> = wrapped.tools().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec![READ_ACTION, REPLY_ACTION]);
        assert!(
            wrapped
                .tools()
                .iter()
                .all(|tool| tool.audience == crate::ActionAudience::Agent),
            "chat relay actions are agent-only"
        );

        let collides = ToolDef::new(
            READ_ACTION,
            "shadowing the host action",
            json!({ "type": "object", "properties": {} }),
            |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
        );
        let error = match install(Arc::new(Bare {
            tools: vec![collides],
        })) {
            Ok(_) => panic!("a surface may not shadow the relay"),
            Err(error) => error,
        };
        assert!(error.contains("collides"), "{error}");
    }

    #[tokio::test]
    async fn a_blocked_read_wakes_within_a_second_of_a_chat_send() {
        let relay = Arc::new(ChatRelay::new());
        let waker = relay.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            waker.post("Mike", None, "Give me an architecture review diagram.");
        });
        let read = tokio::time::timeout(Duration::from_secs(5), async move {
            loop {
                let woken = relay.changed().notified();
                if relay.has_unread() {
                    return relay.collect();
                }
                woken.await;
            }
        })
        .await
        .expect("a blocked read must wake for chat");
        assert!(read.contains("architecture review diagram"), "{read}");
    }

    #[tokio::test]
    async fn an_await_board_action_returns_the_waiting_human_message() {
        let waiting = ToolDef::new(
            "await_board",
            "Wait for a surface change or incoming chat.",
            json!({ "type": "object", "properties": {} }),
            |_args| Effect::AsyncQuery(Box::pin(std::future::pending())),
        )
        .wake_on_chat()
        .agent_only();
        let (surface, relay) = install(Arc::new(BareForChatWake {
            tools: vec![waiting],
        }))
        .expect("install chat wake path");
        let action = surface
            .tools()
            .iter()
            .find(|action| action.name == "await_board")
            .expect("marked wait action");
        let Effect::AsyncQuery(waiting) = (action.apply)(&json!({})) else {
            panic!("marked wait action should remain an async query");
        };

        relay.post("Mike", Some("human-mike"), "Can you see this chat turn?");
        let read = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("incoming chat should wake the marked wait action")
            .expect("chat wake should return a readable result");
        assert!(read.contains("Can you see this chat turn?"), "{read}");
        assert!(read.contains("chat_read"), "{read}");
    }

    struct BareForChatWake {
        tools: Vec<ToolDef>,
    }

    impl Surface for BareForChatWake {
        fn state(&self) -> &dyn SurfaceState {
            &BareStateForChatWake
        }

        fn tools(&self) -> &[ToolDef] {
            &self.tools
        }

        fn client_modules(&self) -> Vec<ClientModule> {
            Vec::new()
        }
    }

    struct BareStateForChatWake;

    impl SurfaceState for BareStateForChatWake {
        fn backing(&self) -> crate::StateBacking {
            crate::StateBacking::Ephemeral
        }

        fn describe(&self) -> Result<String, String> {
            Ok(String::new())
        }

        fn snapshot(&self) -> Result<crate::StateSnapshot, String> {
            Ok(crate::StateSnapshot {
                backing: crate::StateBacking::Ephemeral,
                body: json!({}),
                chrome: None,
            })
        }
    }
}
