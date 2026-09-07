//! OpenAI-compatible (BYOK) backend.
//!
//! Drives the SAME turn-loop contract as [`super::acp`] through any endpoint
//! that speaks the OpenAI **Chat Completions** API, with a key the user
//! brings at runtime. Unlike the ACP backend this is no subprocess: the
//! runtime IS the API client, streams the model's spoken text back as
//! narration, and calls `Surface::tools()` → [`super::dispatch_tool`]
//! directly. ACP reaches that same in-process dispatcher through the
//! runtime-owned HTTP MCP endpoint. Lifted from teaching-canvas's `openai.rs`
//! (M2 Phase C), generic over [`Surface`].
//!
//! Point it at `api.openai.com`, OpenRouter, Together, Groq, or a local
//! llama.cpp / Ollama server — anything OpenAI-shaped. Config is resolved into
//! memory; browser-supplied settings may be persisted in the credential store,
//! and secrets are never returned to the browser.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::narration::{
    emit_narrate, finish_turn, narrate_cancel, narrate_chunk, narrate_flush, reset_narration,
    tutor_event,
};
use crate::runtime_state::{RuntimeState, TurnRequest};
use crate::turn_loop::{build_context, dispatch_tool, Outcome, Prompts};
use crate::{Caller, Surface, ToolDef};

/// How the active OpenAI-compatible endpoint authenticates.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Bring-your-own API key, sent as `Authorization: Bearer <key>`.
    #[default]
    ApiKey,
    /// A Claude Code subscription token: bearer OAuth plus Anthropic's OAuth
    /// beta header. This path never substitutes an API key.
    Oauth,
}

/// The Claude-Code system prefix Anthropic's OAuth endpoint requires as the
/// first system content when authenticating with a subscription token.
pub const CLAUDE_CODE_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// The resolved, active OpenAI-compatible endpoint. Held in memory only; the
/// key/token is never returned to the browser. Rebuilt from the selected
/// provider + [`crate::auth`] on every switch or credential change.
#[derive(Clone, Default)]
pub struct OpenAiConfig {
    /// Stable registry identity used to keep provider-specific credential
    /// resolution fail closed.
    pub provider_id: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Whether `api_key` is a plain key or a subscription OAuth token.
    pub auth: AuthMode,
    /// For OAuth: the config-value to re-resolve per request so the token
    /// stays fresh as the OS keychain rotates it (`None` for plain API keys).
    pub oauth_source: Option<String>,
    /// A credential-resolution failure safe to show to the operator. It never
    /// contains token material.
    pub credential_error: Option<String>,
    /// The model can see images: an action marked
    /// [`ActionDef::with_state_snapshot`](crate::ActionDef::with_state_snapshot)
    /// attaches a rendered PNG so the agent has a real visual feedback loop.
    /// Off → text-only.
    pub vision: bool,
}

impl OpenAiConfig {
    fn endpoint_url(&self) -> Option<reqwest::Url> {
        canonical_base_url(&self.base_url)
            .ok()
            .and_then(|base_url| reqwest::Url::parse(&base_url).ok())
    }

    /// A local/loopback endpoint — a llama.cpp / Ollama server that needs no
    /// key. Cloud hosts (OpenAI, Groq, OpenRouter, Anthropic, …) always need
    /// one.
    pub(crate) fn is_local_host(&self) -> bool {
        let Some(url) = self.endpoint_url() else {
            return false;
        };
        let Some(host) = url.host_str() else {
            return false;
        };
        let normalized_host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        normalized_host.eq_ignore_ascii_case("localhost")
            || normalized_host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    }

    /// Enough to start a turn. A subscription endpoint needs its token
    /// resolved; an API-key endpoint needs a model plus either a key or a
    /// local host that may not need one. Cloud hosts without a key are not
    /// configured.
    pub fn configured(&self) -> bool {
        if self.model.is_empty() || self.endpoint_url().is_none() {
            return false;
        }
        match self.auth {
            AuthMode::Oauth => !self.api_key.is_empty(),
            AuthMode::ApiKey => !self.api_key.is_empty() || self.is_local_host(),
        }
    }
}

/// Validate and normalize an OpenAI-compatible endpoint root. The request
/// path is appended later, so URL components that would change or obscure
/// that path are rejected up front.
pub(crate) fn canonical_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("base_url cannot be empty".to_string());
    }
    let url = reqwest::Url::parse(trimmed).map_err(|error| format!("invalid URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("base_url must be an absolute HTTP(S) URL".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("base_url must not contain user information".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("base_url must not contain a query or fragment".to_string());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Bring-your-own base URL + model for the manual `openai` provider slot
/// (its key lives in [`crate::auth`]).
#[derive(Clone)]
pub struct ByokConfig {
    pub base_url: String,
    pub model: String,
}

impl Default for ByokConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ByokConfigError {
    #[error(transparent)]
    Environment(#[from] crate::config::EnvConfigError),
    #[error("{setting} is invalid: {message}")]
    InvalidBaseUrl {
        setting: &'static str,
        message: String,
    },
}

pub const BASE_URL_SETTING: &str = "base_url";
pub const MODEL_SETTING: &str = "model";

/// Minimum gap between request starts aimed at the same cloud/local endpoint
/// from this process. Tool-follow-up turns are normally sequential already;
/// this shared gate also protects future concurrent agents/extensions from
/// landing a same-millisecond burst on subscription-compatible gateways.
const REQUEST_START_STAGGER: Duration = Duration::from_millis(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_ERROR_BODY: usize = 4 * 1024;
const MAX_SSE_BUFFER: usize = 1024 * 1024;
const MAX_ASSISTANT_TEXT: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 64;
const MAX_TOOL_NAME: usize = 512;
const MAX_TOOL_ARGUMENTS: usize = 1024 * 1024;
static REQUEST_STARTS: LazyLock<tokio::sync::Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

async fn stagger_request_start(base_url: &str) {
    let endpoint = base_url.trim_end_matches('/').to_string();
    let mut starts = REQUEST_STARTS.lock().await;
    let now = Instant::now();
    starts.retain(|_, started| now.duration_since(*started) < Duration::from_secs(60));
    if let Some(previous) = starts.get(&endpoint) {
        let elapsed = previous.elapsed();
        if elapsed < REQUEST_START_STAGGER {
            tokio::time::sleep(REQUEST_START_STAGGER - elapsed).await;
        }
    }
    starts.insert(endpoint, Instant::now());
}

impl ByokConfig {
    /// Seed from `OPENAI_BASE_URL` / `OPENAI_MODEL` (key comes from `auth`).
    /// Kept for callers that do not have an [`AuthStore`](crate::auth::AuthStore).
    pub fn from_env() -> Result<Self, ByokConfigError> {
        Self::from_sources(None)
    }

    /// Environment overrides win; otherwise restore the values saved by the
    /// browser Settings panel beside the provider's credential in `auth.json`.
    pub fn from_env_and_auth(auth: &crate::auth::AuthStore) -> Result<Self, ByokConfigError> {
        Self::from_sources(Some(auth))
    }

    fn from_sources(auth: Option<&crate::auth::AuthStore>) -> Result<Self, ByokConfigError> {
        let saved = auth.map(|a| a.api_key_env("openai")).unwrap_or_default();
        let base_url = std::env::var_os("OPENAI_BASE_URL");
        let model = std::env::var_os("OPENAI_MODEL");
        Self::from_source_values(&saved, base_url.as_deref(), model.as_deref())
    }

    fn from_source_values(
        saved: &HashMap<String, String>,
        base_url_env: Option<&OsStr>,
        model_env: Option<&OsStr>,
    ) -> Result<Self, ByokConfigError> {
        let defaults = Self::default();
        let explicit_base_url =
            crate::config::normalize_optional_os("OPENAI_BASE_URL", base_url_env)?;
        let (candidate_base_url, setting) = match explicit_base_url {
            Some(base_url) => (base_url, "OPENAI_BASE_URL"),
            None => match saved.get(BASE_URL_SETTING) {
                Some(base_url) => (base_url.clone(), "stored OpenAI base URL"),
                None => (defaults.base_url, "default OpenAI base URL"),
            },
        };
        let base_url = canonical_base_url(&candidate_base_url)
            .map_err(|message| ByokConfigError::InvalidBaseUrl { setting, message })?;
        let model = crate::config::normalize_optional_os("OPENAI_MODEL", model_env)?
            .or_else(|| saved.get(MODEL_SETTING).cloned())
            .unwrap_or(defaults.model);
        Ok(Self { base_url, model })
    }
}

/// Resolve the active OpenAI-compatible endpoint for an OpenAI-backed
/// provider: base URL + model (from the provider, or the BYOK form for the
/// manual `openai` slot), plus the effective key/token from the credential
/// store. The result is what the OpenAI supervisor drives. Generic-runtime
/// logic (M2 Phase D) lifted verbatim from teaching-canvas's
/// `main.rs::resolve_openai_config` — every input (`Provider`, `AuthStore`,
/// `ByokConfig`) already lives in this crate, so nothing about it was ever
/// actually canvas-specific.
pub fn resolve_openai_config(
    auth: &crate::auth::AuthStore,
    byok: &ByokConfig,
    provider: &crate::providers::Provider,
    model_override: Option<&str>,
) -> OpenAiConfig {
    let (base_url, model) = if provider.id == "openai" {
        (
            canonical_base_url(&byok.base_url).unwrap_or_default(),
            byok.model.clone(),
        )
    } else {
        // A UI model override (⚙ Settings) wins over the preset's shipped model.
        let model = model_override
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| provider.model.clone().unwrap_or_default());
        (
            provider
                .base_url
                .as_deref()
                .and_then(|base_url| canonical_base_url(base_url).ok())
                .unwrap_or_default(),
            model,
        )
    };
    let auth_mode = if provider.auth == crate::providers::AuthKind::Oauth {
        AuthMode::Oauth
    } else {
        AuthMode::ApiKey
    };
    let resolved = auth.resolve(provider);
    tracing::debug!(
        "provider '{}' key source: {}{}",
        provider.id,
        resolved.source,
        resolved
            .label
            .as_deref()
            .map(|l| format!(" ({l})"))
            .unwrap_or_default()
    );
    OpenAiConfig {
        provider_id: provider.id.clone(),
        base_url,
        api_key: resolved.key.unwrap_or_default(),
        model,
        auth: auth_mode,
        oauth_source: if provider.auth == crate::providers::AuthKind::Oauth {
            provider.key_source.clone()
        } else {
            None
        },
        credential_error: resolved.error,
        vision: provider.vision,
    }
}

/// One assembled streamed tool call (deltas arrive split across SSE chunks).
#[derive(Default, Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
    /// Whether `TOOL_CALL_START` has already gone out for this slot. The call
    /// is announced the moment its id and name are both readable — typically
    /// the first delta — so the browser can show the action being composed
    /// instead of waiting for the arguments to finish decoding.
    announced: bool,
}

/// Run the OpenAI-compatible backend until a provider switch (or channel
/// close). Mirrors the ACP driver's contract: prime, then serve asks/missions,
/// watching for a switch. Re-entered fresh on each (re)prime, so config
/// changes and returning learners both rebuild the standing context from the
/// Surface's store.
pub(crate) async fn run_one_openai(
    rt: &Arc<RuntimeState>,
    surface: &Arc<dyn Surface>,
    prompts: &Arc<Prompts>,
    ask_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    mission_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    switch_rx: &mut mpsc::UnboundedReceiver<String>,
) -> Outcome {
    let provider_session = run_openai_session(rt, surface, prompts, ask_rx, mission_rx);
    tokio::pin!(provider_session);
    tokio::select! {
        biased;
        maybe_switch = switch_rx.recv() => {
            rt.ready.store(false, Ordering::Relaxed);
            rt.warming.store(true, Ordering::Relaxed);
            narrate_cancel(rt);
            rt.busy.store(false, Ordering::SeqCst);
            match maybe_switch {
                Some(id) => Outcome::Switch(id),
                None => Outcome::Exited,
            }
        }
        outcome = &mut provider_session => outcome,
    }
}

async fn run_openai_session(
    rt: &Arc<RuntimeState>,
    surface: &Arc<dyn Surface>,
    prompts: &Arc<Prompts>,
    ask_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
    mission_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
) -> Outcome {
    let cfg = rt.openai.lock().clone();

    rt.ready.store(false, Ordering::Relaxed);
    rt.warming.store(true, Ordering::Relaxed);
    tutor_event(rt, "warming", None);

    // Not configured yet: idle (the page shows the settings form) until the
    // user POSTs a config — which self-switches to "openai" to re-enter here
    // — or switches to a different provider. Asks are already blocked
    // client-side because we never go `ready`, so we only watch for a switch.
    if !cfg.configured() {
        info!("openai backend selected but not configured, awaiting credentials");
        rt.warming.store(false, Ordering::Relaxed);
        let msg = cfg.credential_error.clone().unwrap_or_else(|| {
            match cfg.auth {
                AuthMode::Oauth => "Claude subscription unavailable: the required Claude Code OAuth token is missing".to_string(),
                AuthMode::ApiKey => "Add a model and API key (or a local base URL) in Settings (the ⚙ by the agent picker) to begin.".to_string(),
            }
        });
        warn!("{msg}");
        *rt.provider_error.lock() = Some(msg.clone());
        emit_narrate(rt, &msg);
        tutor_event(rt, "failed", Some(&msg));
        std::future::pending::<()>().await;
        return Outcome::Exited;
    }

    info!(
        "openai backend ready (base_url={}, model={})",
        cfg.base_url, cfg.model
    );
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warn!(%error, "failed to construct OpenAI HTTP client");
            rt.warming.store(false, Ordering::Relaxed);
            return Outcome::Exited;
        }
    };
    // The UI reserves "request proven" for a provider that has completed a
    // real model request. A Keychain token being present is configuration,
    // not health, so the subscription provider proves its exact OAuth wire
    // before this session becomes ready. This probe is deliberately confined
    // to `claude-max`; API-key providers never stand in for subscription
    // health.
    if cfg.provider_id == crate::providers::CLAUDE_MAX_ID {
        if let Err(error) = verify_claude_subscription(&client, &cfg).await {
            warn!("{error}");
            rt.warming.store(false, Ordering::Relaxed);
            *rt.provider_error.lock() = Some(error.clone());
            emit_narrate(rt, &error);
            tutor_event(rt, "failed", Some(&error));
            std::future::pending::<()>().await;
            return Outcome::Exited;
        }
        info!(
            model = %cfg.model,
            "Claude subscription request verified"
        );
    }
    // The subscription (OAuth) endpoint requires the Claude-Code system
    // prefix as the first system content; the agent's persona follows it.
    let system = match cfg.auth {
        AuthMode::Oauth => format!(
            "{CLAUDE_CODE_PREFIX}\n\n{}",
            build_context(prompts, surface.as_ref())
        ),
        AuthMode::ApiKey => build_context(prompts, surface.as_ref()),
    };
    let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": system })];

    reset_narration(rt);
    rt.warming.store(false, Ordering::Relaxed);
    rt.ready.store(true, Ordering::Relaxed);
    tutor_event(rt, "ready", None);

    loop {
        tokio::select! {
            maybe_q = ask_rx.recv() => {
                let Some(mut q) = maybe_q else { return Outcome::Exited };
                run_turn(
                    &client,
                    rt,
                    surface,
                    &cfg,
                    &mut messages,
                    &q.text,
                    &mut q.cancel,
                ).await;
            }
            maybe_m = mission_rx.recv() => {
                let Some(mut m) = maybe_m else { return Outcome::Exited };
                let prompt = (prompts.mission)(&m.text);
                run_turn(
                    &client,
                    rt,
                    surface,
                    &cfg,
                    &mut messages,
                    &prompt,
                    &mut m.cancel,
                ).await;
            }
        }
    }
}

/// One turn: push the user message, then drive the model in a tool loop —
/// stream its speech as narration, apply each function call via
/// [`super::dispatch_tool`], and loop until it stops calling tools (or a
/// safety cap). Errors surface as a spoken line rather than killing the
/// session.
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    client: &reqwest::Client,
    rt: &Arc<RuntimeState>,
    surface: &Arc<dyn Surface>,
    cfg: &OpenAiConfig,
    messages: &mut Vec<Value>,
    user_text: &str,
    cancel: &mut tokio::sync::broadcast::Receiver<()>,
) {
    messages.push(json!({ "role": "user", "content": user_text }));
    let turn_start = Instant::now();

    // Barge-in: a tick on this cancels the turn mid-stream. Dropping the
    // in-flight `stream_completion` future aborts the underlying reqwest stream.
    let tools = render_openai_tools(surface.tools());
    let mut steps = 0;
    let mut interrupted = false;
    let result: Result<(), String> = 'turn: loop {
        steps += 1;
        if steps > 16 {
            break Err("the model kept calling tools without finishing".to_string());
        }
        let step_start = Instant::now();
        let step = tokio::select! {
            biased;
            _ = cancel.recv() => { interrupted = true; None }
            res = stream_completion(client, cfg, messages, &tools, rt) => Some(res),
        };
        let Some(step) = step else { break Ok(()) }; // interrupted mid-stream
        match step {
            Ok((text, tool_calls)) => {
                info!(
                    "openai turn step {steps}: completion in {:?} ({} tool call(s), {} chars spoken)",
                    step_start.elapsed(),
                    tool_calls.len(),
                    text.len()
                );
                // Record the assistant turn (content + any tool calls) so the
                // next request has the full thread.
                let mut msg = serde_json::Map::new();
                msg.insert("role".into(), json!("assistant"));
                msg.insert(
                    "content".into(),
                    if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    },
                );
                if !tool_calls.is_empty() {
                    let tcs: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": { "name": tc.name, "arguments": tc.arguments }
                            })
                        })
                        .collect();
                    msg.insert("tool_calls".into(), json!(tcs));
                }
                messages.push(Value::Object(msg));

                if tool_calls.is_empty() {
                    break Ok(());
                }
                // Dispatch each call via `Surface::tools()` and feed a result
                // back per call. A `Query`-effect tool (teaching-canvas:
                // read_canvas) returns the data the model asked for; a
                // mutating tool just confirms it applied.
                let mut wants_state_snapshot = false;
                for (index, tc) in tool_calls.iter().enumerate() {
                    let action = surface
                        .tools()
                        .iter()
                        .find(|action| action.name == tc.name && action.audience.agent());
                    let dispatch = async {
                        match action {
                            Some(_) => match parse_tool_arguments(&tc.name, &tc.arguments) {
                                Ok(args) => {
                                    dispatch_tool(
                                        rt,
                                        surface.as_ref(),
                                        Caller::Agent,
                                        &tc.name,
                                        &args,
                                    )
                                    .await
                                }
                                Err(error) => (error, false, false),
                            },
                            None => (
                                format!("action {:?} is not visible to this agent", tc.name),
                                false,
                                false,
                            ),
                        }
                    };
                    tokio::pin!(dispatch);
                    let dispatched = tokio::select! {
                        biased;
                        _ = cancel.recv() => {
                            interrupted = true;
                            None
                        }
                        result = &mut dispatch => Some(result),
                    };
                    let Some((content, _was_query, ok)) = dispatched else {
                        // The assistant message already declared every tool
                        // call. Keep the conversation valid by supplying one
                        // terminal tool result for this and every undispatched
                        // call before recording the interruption — and tell
                        // the browser the same thing, so an interrupted turn
                        // does not leave calls pending on the surface forever.
                        for remaining in tool_calls.iter().skip(index) {
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": remaining.id,
                                "content": "interrupted by learner",
                            }));
                            super::emit_tool_call_result(
                                rt,
                                &remaining.id,
                                "interrupted by learner",
                            );
                        }
                        break 'turn Ok(());
                    };
                    if ok && action.is_some_and(|action| action.include_state_snapshot) {
                        wants_state_snapshot = true;
                    }
                    // The human reads exactly the text the model reads.
                    super::emit_tool_call_result(rt, &tc.id, &content);
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tc.id,
                        "content": content,
                    }));
                }
                // Visual feedback loop: after an explicitly opted-in action
                // on a vision model, append a render of the state as a user turn
                // so the agent literally SEES what it drew — overlaps,
                // off-screen points, cramped layout — not just the
                // coordinates in the text read-back. This is what lets it
                // clean the diagram up.
                if wants_state_snapshot && cfg.vision {
                    let r_start = Instant::now();
                    let snapshot = surface.state().snapshot_png();
                    tokio::pin!(snapshot);
                    let rendered = tokio::select! {
                        biased;
                        _ = cancel.recv() => {
                            interrupted = true;
                            None
                        }
                        png = &mut snapshot => Some(png),
                    };
                    let Some(rendered) = rendered else {
                        break 'turn Ok(());
                    };
                    let png = match rendered {
                        Ok(Some(png)) => Some(png),
                        Ok(None) => None,
                        Err(error) => {
                            break 'turn Err(format!("visual state snapshot failed: {error}"));
                        }
                    };
                    if let Some(png) = png {
                        // Keep only the LATEST render in the thread — strip
                        // earlier image turns so base64 PNGs don't pile up
                        // and inflate every later completion (they otherwise
                        // compound all session).
                        messages.retain(|m| !is_surface_image_msg(m));
                        info!(
                            "visual feedback: rendered {} bytes in {:?}",
                            png.len(),
                            r_start.elapsed()
                        );
                        let uri = png_to_data_uri(&png);
                        messages.push(json!({
                            "role": "user",
                            "content": [
                                { "type": "text", "text": "This is what the learner sees right now: your canvas, rendered. Glance at it once: is everything on-screen and readable, nothing overlapping? If ONE thing is genuinely off, nudge just that; otherwise leave it and keep teaching. Do not clear and redraw the whole picture." },
                                { "type": "image_url", "image_url": { "url": uri } }
                            ]
                        }));
                    }
                }
                // Loop: let the model keep narrating / drawing or wrap up.
            }
            Err(e) => break Err(e),
        }
    };

    if interrupted {
        // Barge-in: hush, keep the thread well-formed for the next turn, free UI.
        narrate_cancel(rt);
        messages.push(
            json!({ "role": "assistant", "content": "(The learner interrupted; stopping here.)" }),
        );
        finish_turn(rt);
        info!("barge-in: cancelled current OpenAI turn");
        return;
    }

    narrate_flush(rt);
    info!(
        "openai turn complete in {:?} ({} step(s))",
        turn_start.elapsed(),
        steps
    );
    if let Err(e) = result {
        warn!("openai turn error: {e}");
        let friendly = friendly_error(&e);
        emit_narrate(rt, &friendly);
        crate::narration::fail_turn(rt, &friendly);
        return;
    }
    finish_turn(rt);
}

fn parse_tool_arguments(name: &str, raw: &str) -> Result<Value, String> {
    serde_json::from_str(raw)
        .map_err(|error| format!("invalid arguments for action {name:?}: malformed JSON: {error}"))
}

/// Render a Surface's tool vocabulary as OpenAI Chat Completions function
/// tools — the generic, Surface-agnostic replacement for teaching-canvas's
/// `tools::openai_tools()` (which rendered `tools::specs()`, a canvas-local
/// list). Reads `ToolDef.name`/`description`/`parameters` directly, so ANY
/// Surface's vocabulary renders correctly with no per-Surface renderer.
fn render_openai_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .filter(|action| action.audience.agent())
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        })
        .collect()
}

/// A user turn carrying a state render (an `image_url` content part). We
/// keep only the most recent one in the thread so rendered PNGs don't
/// accumulate and inflate the prompt on every subsequent completion.
fn is_surface_image_msg(m: &Value) -> bool {
    m.get("role").and_then(Value::as_str) == Some("user")
        && m.get("content")
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                arr.iter()
                    .any(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
            })
}

/// Wrap PNG bytes as a `data:image/png;base64,...` URI (the OpenAI vision
/// content-part shape). Runtime-owned formatting over
/// `SurfaceState::snapshot_png()` — not a Surface concern, just wire-format
/// glue this backend needs (mirrors teaching-canvas's original `render.rs`
/// helper, ported here rather than depended on since `render.rs` stays in
/// `examples/teaching-canvas`).
fn png_to_data_uri(png: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

/// Stream one chat completion: forward streamed prose to narration as it
/// arrives, and assemble any function (tool) calls. Returns the full
/// assistant text plus the assembled tool calls.
async fn stream_completion(
    client: &reqwest::Client,
    cfg: &OpenAiConfig,
    messages: &[Value],
    tools: &[Value],
    rt: &Arc<RuntimeState>,
) -> Result<(String, Vec<ToolCall>), String> {
    let base_url = canonical_base_url(&cfg.base_url)
        .map_err(|error| format!("invalid OpenAI base URL: {error}"))?;
    let url = reqwest::Url::parse(&format!("{base_url}/chat/completions"))
        .map_err(|error| format!("failed to construct completion URL: {error}"))?;
    let body = completion_request_body(&cfg.model, messages, tools);

    let mut req = client.post(url).json(&body);
    match cfg.auth {
        AuthMode::Oauth => {
            // Re-resolve the subscription token per request so it stays fresh
            // as the OS keychain rotates it. `claude-max` never falls back to
            // a stored API key, environment variable, or its primed token.
            let token = if cfg.provider_id == crate::providers::CLAUDE_MAX_ID {
                crate::auth::resolve_claude_code_oauth_token().map_err(|error| error.to_string())?
            } else {
                cfg.oauth_source
                    .as_deref()
                    .and_then(crate::auth::resolve_config_value)
                    .unwrap_or_else(|| cfg.api_key.clone())
            };
            req = req
                .bearer_auth(token)
                .header("anthropic-beta", "oauth-2025-04-20");
        }
        AuthMode::ApiKey => {
            if !cfg.api_key.is_empty() {
                req = req.bearer_auth(&cfg.api_key);
            }
        }
    }
    stagger_request_start(&cfg.base_url).await;
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let detail = bounded_response_body(resp, MAX_ERROR_BODY).await;
        if cfg.auth == AuthMode::Oauth {
            return Err(format!(
                "Claude subscription request failed with {status}: {detail}"
            ));
        }
        return Err(format!("{status} {detail}"));
    }

    let stream = resp.bytes_stream();
    futures_util::pin_mut!(stream);

    let mut buf = Vec::new();
    let mut text = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut saw_json_event = false;
    let mut saw_completion = false;

    'stream: while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_SSE_BUFFER {
            return Err("OpenAI-compatible endpoint emitted an oversized SSE line".to_string());
        }

        // Process complete SSE lines; leave any partial tail in `buf`.
        while let Some(nl) = buf.iter().position(|byte| *byte == b'\n') {
            let line = buf.drain(..=nl).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .map_err(|error| format!("endpoint emitted non-UTF-8 SSE: {error}"))?
                .trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                saw_completion = true;
                break 'stream;
            }
            let v = serde_json::from_str::<Value>(data)
                .map_err(|error| format!("endpoint emitted malformed SSE JSON: {error}"))?;
            let choice = v
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .ok_or_else(|| "endpoint SSE event had no completion choice".to_string())?;
            let delta = choice
                .get("delta")
                .ok_or_else(|| "endpoint SSE choice had no delta".to_string())?;
            saw_json_event = true;
            let terminal = choice
                .get("finish_reason")
                .is_some_and(|reason| !reason.is_null());
            if terminal {
                saw_completion = true;
            }

            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    append_limited(&mut text, content, MAX_ASSISTANT_TEXT, "assistant text")?;
                    narrate_chunk(rt, content);
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    let idx = tool_call_index(tc)?;
                    if idx >= MAX_TOOL_CALLS {
                        return Err(format!(
                            "endpoint emitted tool-call index {idx}, above the {MAX_TOOL_CALLS} limit"
                        ));
                    }
                    while calls.len() <= idx {
                        calls.push(ToolCall::default());
                    }
                    let slot = calls
                        .get_mut(idx)
                        .ok_or_else(|| "internal tool-call slot was not allocated".to_string())?;
                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            slot.id = id.to_string();
                        }
                    }
                    let mut argument_delta: Option<&str> = None;
                    if let Some(f) = tc.get("function") {
                        if let Some(name) = f.get("name").and_then(Value::as_str) {
                            append_limited(&mut slot.name, name, MAX_TOOL_NAME, "tool-call name")?;
                        }
                        if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                            append_limited(
                                &mut slot.arguments,
                                a,
                                MAX_TOOL_ARGUMENTS,
                                "tool-call arguments",
                            )?;
                            argument_delta = Some(a);
                        }
                    }
                    // Announce as soon as the call has an identity, then
                    // forward every argument chunk as it arrives. The first
                    // announcement flushes whatever already accumulated,
                    // because a provider may deliver arguments in the same
                    // delta that first names the tool.
                    if !slot.announced && !slot.id.is_empty() && !slot.name.is_empty() {
                        slot.announced = true;
                        super::emit_tool_call_start(rt, &slot.id, &slot.name);
                        super::emit_tool_call_args(rt, &slot.id, &slot.arguments);
                    } else if slot.announced {
                        if let Some(delta) = argument_delta {
                            super::emit_tool_call_args(rt, &slot.id, delta);
                        }
                    }
                }
            }
            if terminal {
                break 'stream;
            }
        }
    }

    // Close every announced argument stream before any of the completion
    // checks below can return early, so a protocol failure never leaves the
    // browser rendering a tool call that is still "composing".
    for call in calls.iter().filter(|call| call.announced) {
        super::emit_tool_call_end(rt, &call.id);
    }

    if !saw_completion && !buf.iter().all(u8::is_ascii_whitespace) {
        return Err("endpoint closed with an incomplete SSE line".to_string());
    }
    if !saw_json_event {
        return Err("endpoint returned success but no completion events".to_string());
    }
    if !saw_completion {
        return Err("endpoint closed before a completion marker".to_string());
    }

    validate_completed_tool_calls(&calls)?;
    if text.is_empty() && calls.is_empty() {
        return Err("endpoint returned an empty completion".to_string());
    }
    Ok((text, calls))
}

/// Prove the built-in subscription provider with a real, minimal model
/// completion before the runtime advertises it as ready. Credential presence
/// alone is not provider health, and this function cannot consult an API key.
async fn verify_claude_subscription(
    client: &reqwest::Client,
    cfg: &OpenAiConfig,
) -> Result<(), String> {
    let base_url = canonical_base_url(&cfg.base_url)
        .map_err(|error| format!("invalid Claude subscription base URL: {error}"))?;
    let url = reqwest::Url::parse(&format!("{base_url}/chat/completions"))
        .map_err(|error| format!("failed to construct Claude subscription probe URL: {error}"))?;
    let token =
        crate::auth::resolve_claude_code_oauth_token().map_err(|error| error.to_string())?;
    let response = client
        .post(url)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .json(&json!({
            "model": cfg.model,
            "stream": false,
            "messages": [
                { "role": "system", "content": CLAUDE_CODE_PREFIX },
                { "role": "user", "content": "Reply with exactly READY." }
            ]
        }))
        .send()
        .await
        .map_err(|error| format!("Claude subscription verification request failed: {error}"))?;
    let status = response.status();
    let detail = bounded_response_body(response, MAX_ERROR_BODY).await;
    if !status.is_success() {
        return Err(format!(
            "Claude subscription verification failed with {status}: {detail}"
        ));
    }
    let payload: Value = serde_json::from_str(&detail).map_err(|error| {
        format!("Claude subscription verification returned malformed JSON: {error}")
    })?;
    let _text = payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            "Claude subscription verification returned no assistant response".to_string()
        })?;
    Ok(())
}

fn completion_request_body(model: &str, messages: &[Value], tools: &[Value]) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert("messages".to_string(), json!(messages));
    body.insert("stream".to_string(), json!(true));
    if !tools.is_empty() {
        body.insert("tools".to_string(), json!(tools));
        body.insert("tool_choice".to_string(), json!("auto"));
    }
    Value::Object(body)
}

fn tool_call_index(tool_call: &Value) -> Result<usize, String> {
    let raw = tool_call
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "endpoint emitted a tool-call delta without a non-negative integer index".to_string()
        })?;
    usize::try_from(raw).map_err(|_| "tool-call index does not fit usize".to_string())
}

fn validate_completed_tool_calls(calls: &[ToolCall]) -> Result<(), String> {
    for (index, call) in calls.iter().enumerate() {
        if call.id.is_empty() {
            return Err(format!(
                "endpoint completed with tool call {index} missing its id"
            ));
        }
        if call.name.is_empty() {
            return Err(format!(
                "endpoint completed with tool call {index} missing its function name"
            ));
        }
    }
    Ok(())
}

fn append_limited(
    target: &mut String,
    value: &str,
    limit: usize,
    label: &str,
) -> Result<(), String> {
    let next = target
        .len()
        .checked_add(value.len())
        .ok_or_else(|| format!("{label} length overflow"))?;
    if next > limit {
        return Err(format!("endpoint emitted oversized {label}"));
    }
    target.push_str(value);
    Ok(())
}

async fn bounded_response_body(response: reqwest::Response, limit: usize) -> String {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = limit.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend(chunk.iter().take(remaining).copied());
    }
    String::from_utf8_lossy(&body).trim().to_string()
}

/// Map a raw API error to one short spoken line for the learner and name the
/// configuration action that can resolve it.
fn friendly_error(e: &str) -> String {
    if e.starts_with("Claude subscription unavailable:")
        || e.starts_with("Claude subscription request failed")
    {
        return e.to_string();
    }
    let l = e.to_lowercase();
    if l.contains("credit balance")
        || l.contains("insufficient")
        || l.contains("quota")
        || l.contains("billing")
    {
        "This API connection reported insufficient credits. It is not using your managed subscription. Switch agents or update the key in Agents & connections.".to_string()
    } else if l.contains("429") || l.contains("rate limit") || l.contains("rate_limit") {
        "Rate limited for a moment. Give it a few seconds and ask again.".to_string()
    } else if l.contains("401")
        || l.contains("403")
        || l.contains("unauthorized")
        || l.contains("invalid api key")
        || l.contains("permission")
    {
        "The API key was rejected. Update it in Agents & connections, or switch to another configured provider.".to_string()
    } else if l.contains("404") || (l.contains("model") && l.contains("not")) {
        "That model wasn't found at this endpoint. Check the model name in Agents & connections."
            .to_string()
    } else if l.contains("connect")
        || l.contains("dns")
        || l.contains("timed out")
        || l.contains("error sending request")
    {
        "Couldn't reach the endpoint. Check the base URL in Agents & connections.".to_string()
    } else {
        format!(
            "The model call failed: {}",
            e.chars().take(180).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionAudience, Effect};

    #[test]
    fn openai_schema_projection_excludes_human_only_actions() {
        let action = |name, audience| {
            ToolDef::new(
                name,
                format!("{name} description"),
                json!({ "type": "object", "properties": {} }),
                |_args| Effect::Query(Box::new(|_state| Ok("ok".to_string()))),
            )
            .audience(audience)
        };
        let rendered = render_openai_tools(&[
            action("human", ActionAudience::Human),
            action("agent", ActionAudience::Agent),
            action("both", ActionAudience::Both),
        ]);
        assert_eq!(
            rendered
                .iter()
                .map(|tool| tool["function"]["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["agent", "both"]
        );
    }

    #[test]
    fn completion_body_omits_tool_fields_when_no_agent_actions_exist() {
        let body = completion_request_body("model", &[json!({ "role": "user" })], &[]);
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());

        let body = completion_request_body(
            "model",
            &[json!({ "role": "user" })],
            &[json!({ "type": "function" })],
        );
        assert!(body.get("tools").is_some());
        assert_eq!(body["tool_choice"], "auto");
    }

    #[tokio::test]
    async fn done_marker_completes_without_waiting_for_http_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind held-open SSE fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept completion request");
            let mut request = vec![0_u8; 8192];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .expect("write SSE headers");
            let events = b"data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
            socket
                .write_all(format!("{:X}\r\n", events.len()).as_bytes())
                .await
                .expect("write chunk length");
            socket.write_all(events).await.expect("write SSE events");
            socket.write_all(b"\r\n").await.expect("finish data chunk");
            // Deliberately never send the terminating zero chunk. The client
            // must return on [DONE], not HTTP EOF.
            std::future::pending::<()>().await;
        });

        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let (sse_tx, _) = tokio::sync::broadcast::channel(8);
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-openai-done-test-{}.json",
                std::process::id()
            )))
            .expect("open test auth store"),
        );
        let (rt, _) = RuntimeState::new(
            ws_tx,
            sse_tx,
            Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::new())),
            Vec::new(),
            auth,
            "test".to_string(),
            ByokConfig::default(),
            false,
            0,
        );
        rt.busy.store(true, Ordering::Relaxed);
        let cfg = OpenAiConfig {
            base_url: format!("http://{address}/v1"),
            model: "test".to_string(),
            ..OpenAiConfig::default()
        };
        let client = reqwest::Client::new();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            stream_completion(
                &client,
                &cfg,
                &[json!({ "role": "user", "content": "hi" })],
                &[],
                &rt,
            ),
        )
        .await
        .expect("[DONE] must end the stream without EOF")
        .expect("completion should succeed");
        assert_eq!(result.0, "done");
        server.abort();
        let _ = server.await;
    }

    /// Serve one fixed SSE body, then collect the AG-UI events the runtime
    /// emitted while consuming it.
    async fn events_emitted_for_sse(body: &'static str) -> (Vec<Value>, Result<(), String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind SSE fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept completion request");
            let mut request = vec![0_u8; 8192];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .await
                .expect("write SSE headers");
            socket
                .write_all(body.as_bytes())
                .await
                .expect("write SSE events");
        });

        let (ws_tx, _) = tokio::sync::broadcast::channel(64);
        let (sse_tx, _) = tokio::sync::broadcast::channel(64);
        let auth = Arc::new(
            crate::auth::AuthStore::open(std::env::temp_dir().join(format!(
                "ag-ui-openai-stream-test-{}-{:p}.json",
                std::process::id(),
                body
            )))
            .expect("open test auth store"),
        );
        let (rt, _) = RuntimeState::new(
            ws_tx,
            sse_tx,
            Arc::new(parking_lot::Mutex::new(std::collections::VecDeque::new())),
            Vec::new(),
            auth,
            "test".to_string(),
            ByokConfig::default(),
            false,
            0,
        );
        rt.busy.store(true, Ordering::Relaxed);
        let mut sse = rt.sse_tx.subscribe();
        let cfg = OpenAiConfig {
            base_url: format!("http://{address}/v1"),
            model: "test".to_string(),
            ..OpenAiConfig::default()
        };
        let client = reqwest::Client::new();
        let outcome = stream_completion(
            &client,
            &cfg,
            &[json!({ "role": "user", "content": "hi" })],
            &[],
            &rt,
        )
        .await
        .map(|_| ());
        server.abort();
        let _ = server.await;

        let mut events = Vec::new();
        while let Ok(raw) = sse.try_recv() {
            events.push(serde_json::from_str::<Value>(&raw).expect("emitted valid JSON"));
        }
        (events, outcome)
    }

    fn tool_events(events: &[Value]) -> Vec<(&str, String)> {
        events
            .iter()
            .filter_map(|event| {
                let kind = event.get("type").and_then(Value::as_str)?;
                if !kind.starts_with("TOOL_CALL_") {
                    return None;
                }
                let detail = event
                    .get("delta")
                    .or_else(|| event.get("toolCallName"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Some((kind, detail))
            })
            .collect()
    }

    #[tokio::test]
    async fn tool_arguments_reach_the_browser_as_deltas_while_the_model_composes_them() {
        // One tool call whose arguments are split across four SSE deltas —
        // the shape a model produces while composing a large payload.
        let (events, outcome) = events_emitted_for_sse(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"publish\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"a\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1,\\\"b\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"2}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ))
        .await;
        outcome.expect("stream should complete");

        let emitted = tool_events(&events);
        assert_eq!(
            emitted,
            vec![
                // Announced on the FIRST delta — before any argument text
                // exists. This is the whole point: the surface shows the
                // action being composed rather than appearing after.
                ("TOOL_CALL_START", "publish".to_string()),
                ("TOOL_CALL_ARGS", "{\"a\":".to_string()),
                ("TOOL_CALL_ARGS", "1,\"b\":".to_string()),
                ("TOOL_CALL_ARGS", "2}".to_string()),
                ("TOOL_CALL_END", String::new()),
            ]
        );
        // Deltas are appends, never the accumulated buffer: concatenating them
        // reconstructs the arguments exactly once.
        let rebuilt: String = emitted
            .iter()
            .filter(|(kind, _)| *kind == "TOOL_CALL_ARGS")
            .map(|(_, delta)| delta.as_str())
            .collect();
        assert_eq!(rebuilt, r#"{"a":1,"b":2}"#);
    }

    #[tokio::test]
    async fn an_announced_call_is_closed_even_when_the_stream_fails_validation() {
        // The model names a call, streams arguments, then the endpoint ends the
        // stream without ever supplying an id. The completion must fail — and
        // the browser must not be left rendering a call that never closes.
        let (events, outcome) = events_emitted_for_sse(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"publish\",\"arguments\":\"{\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        ))
        .await;
        assert!(
            outcome.is_err(),
            "a tool call with no id must fail the completion"
        );

        let kinds: Vec<&str> = tool_events(&events)
            .into_iter()
            .map(|(kind, _)| kind)
            .collect();
        assert_eq!(
            kinds,
            vec!["TOOL_CALL_START", "TOOL_CALL_ARGS", "TOOL_CALL_END"],
            "the announced call must be closed before the error path returns"
        );
    }

    #[test]
    fn malformed_tool_arguments_are_not_coerced_to_an_empty_object() {
        let error = parse_tool_arguments("clear", "{not-json").unwrap_err();
        assert!(error.contains("invalid arguments for action \"clear\""));
        assert!(error.contains("malformed JSON"));
        assert_eq!(parse_tool_arguments("clear", "{}").unwrap(), json!({}));
    }

    #[test]
    fn completed_tool_calls_must_have_ids_and_function_names() {
        let missing_id = ToolCall {
            id: String::new(),
            name: "mutate".to_string(),
            arguments: "{}".to_string(),
            announced: false,
        };
        assert!(validate_completed_tool_calls(&[missing_id])
            .unwrap_err()
            .contains("missing its id"));

        let missing_name = ToolCall {
            id: "call-1".to_string(),
            name: String::new(),
            arguments: "{}".to_string(),
            announced: false,
        };
        assert!(validate_completed_tool_calls(&[missing_name])
            .unwrap_err()
            .contains("missing its function name"));
    }

    #[test]
    fn tool_call_deltas_require_an_explicit_non_negative_integer_index() {
        assert_eq!(tool_call_index(&json!({ "index": 2 })).unwrap(), 2);
        for malformed in [
            json!({}),
            json!({ "index": null }),
            json!({ "index": -1 }),
            json!({ "index": 1.5 }),
            json!({ "index": "0" }),
        ] {
            assert!(tool_call_index(&malformed)
                .unwrap_err()
                .contains("non-negative integer index"));
        }
    }

    #[test]
    fn keyless_endpoint_requires_an_exact_loopback_host() {
        let config = |base_url: &str, api_key: &str| OpenAiConfig {
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            model: "test-model".to_string(),
            ..OpenAiConfig::default()
        };

        assert!(config("http://localhost:11434/v1", "").configured());
        assert!(config("http://127.9.8.7:8080/v1", "").configured());
        assert!(config("http://[::1]:8080/v1", "").configured());
        assert!(!config("https://localhost.evil.example/v1", "").configured());
        assert!(!config("https://example.test/v1/127.0.0.1", "").configured());
        assert!(!config("not a URL containing localhost", "").configured());
        assert!(!config("ftp://localhost/v1", "").configured());
        assert!(!config("http://0.0.0.0:8080/v1", "").configured());
        assert!(config("https://api.example.test/v1", "secret").configured());
        assert!(!config("https://user@example.test/v1", "secret").configured());
        assert!(!config("https://example.test/v1?q=x", "secret").configured());
        assert!(!config("https://example.test/v1#x", "secret").configured());
    }

    #[test]
    fn base_url_rejects_components_that_break_appended_request_paths() {
        assert_eq!(
            canonical_base_url(" https://example.test/v1/// ").unwrap(),
            "https://example.test/v1"
        );
        assert!(canonical_base_url("https://user@example.test/v1")
            .unwrap_err()
            .contains("user information"));
        assert!(canonical_base_url("https://example.test/v1?q=x")
            .unwrap_err()
            .contains("query or fragment"));
        assert!(canonical_base_url("https://example.test/v1#x")
            .unwrap_err()
            .contains("query or fragment"));
    }

    #[test]
    fn byok_explicit_blank_values_do_not_fall_through_to_saved_settings() {
        let saved = HashMap::from([
            (
                BASE_URL_SETTING.to_string(),
                "https://saved.example.test/v1".to_string(),
            ),
            (MODEL_SETTING.to_string(), "saved-model".to_string()),
        ]);

        assert!(matches!(
            ByokConfig::from_source_values(&saved, Some(OsStr::new("   ")), None),
            Err(ByokConfigError::Environment(
                crate::config::EnvConfigError::Empty {
                    name: "OPENAI_BASE_URL"
                }
            ))
        ));
        assert!(matches!(
            ByokConfig::from_source_values(&saved, None, Some(OsStr::new(""))),
            Err(ByokConfigError::Environment(
                crate::config::EnvConfigError::Empty {
                    name: "OPENAI_MODEL"
                }
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn byok_explicit_non_unicode_values_are_rejected() {
        use std::os::unix::ffi::OsStringExt;

        let invalid = std::ffi::OsString::from_vec(vec![0xff]);
        let saved = HashMap::new();
        assert!(matches!(
            ByokConfig::from_source_values(&saved, Some(&invalid), None),
            Err(ByokConfigError::Environment(
                crate::config::EnvConfigError::NotUnicode {
                    name: "OPENAI_BASE_URL"
                }
            ))
        ));
        assert!(matches!(
            ByokConfig::from_source_values(&saved, None, Some(&invalid)),
            Err(ByokConfigError::Environment(
                crate::config::EnvConfigError::NotUnicode {
                    name: "OPENAI_MODEL"
                }
            ))
        ));
    }

    #[test]
    fn byok_invalid_explicit_or_persisted_base_url_fails_closed() {
        let saved = HashMap::new();
        assert!(matches!(
            ByokConfig::from_source_values(&saved, Some(OsStr::new("not-an-absolute-url")), None,),
            Err(ByokConfigError::InvalidBaseUrl {
                setting: "OPENAI_BASE_URL",
                ..
            })
        ));

        let saved = HashMap::from([(
            BASE_URL_SETTING.to_string(),
            "https://example.test/v1?unsafe=true".to_string(),
        )]);
        assert!(matches!(
            ByokConfig::from_source_values(&saved, None, None),
            Err(ByokConfigError::InvalidBaseUrl {
                setting: "stored OpenAI base URL",
                ..
            })
        ));
    }

    #[test]
    fn byok_absent_env_uses_saved_settings_or_defaults() {
        let saved = HashMap::from([
            (
                BASE_URL_SETTING.to_string(),
                "https://saved.example.test/v1/".to_string(),
            ),
            (MODEL_SETTING.to_string(), "saved-model".to_string()),
        ]);
        let restored = ByokConfig::from_source_values(&saved, None, None)
            .expect("saved BYOK settings should be valid");
        assert_eq!(restored.base_url, "https://saved.example.test/v1");
        assert_eq!(restored.model, "saved-model");

        let defaults = ByokConfig::from_source_values(&HashMap::new(), None, None)
            .expect("absent settings should use defaults");
        assert_eq!(defaults.base_url, "https://api.openai.com/v1");
        assert_eq!(defaults.model, "gpt-4o");
    }

    #[tokio::test]
    async fn request_starts_for_one_endpoint_are_staggered() {
        let endpoint = format!("test://stagger/{}", std::process::id());
        stagger_request_start(&endpoint).await;
        let started = Instant::now();
        stagger_request_start(&endpoint).await;
        assert!(
            started.elapsed() >= Duration::from_millis(9),
            "same-endpoint request starts must be separated by about 10ms"
        );
    }
}
