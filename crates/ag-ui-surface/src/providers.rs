//! Provider registry — built-in agents plus a data-driven `models.json`,
//! modeled on earendil-works/pi.
//!
//! A provider is one selectable way to drive the tutor. Four supported shapes share one
//! registry and one supervisor:
//! - **ACP** ([`crate::providers::Backend::Acp`],
//!   [`crate::providers::AuthKind::Managed`]) — a spawned coding-agent
//!   subprocess (Claude, Gemini, opencode, Codex) that manages its own auth
//!   (subscription/OAuth/CLI login).
//! - **Pi RPC** ([`crate::providers::Backend::PiRpc`],
//!   [`crate::providers::AuthKind::Managed`]) — a spawned Pi coding agent
//!   driven over its newline-delimited RPC protocol. Surface actions are
//!   registered by an explicit extension that calls the runtime-owned,
//!   authenticated MCP endpoint; Pi's ordinary repository tools remain an
//!   explicit allowlist.
//! - **OpenAI + API key** ([`crate::providers::Backend::OpenAi`],
//!   [`crate::providers::AuthKind::ApiKey`]) — any
//!   OpenAI Chat Completions-compatible endpoint with a bring-your-own key: the
//!   built-in BYOK slot (`id: "openai"`, base/model from the ⚙ form) and any
//!   number of `models.json` presets (Groq, OpenRouter, a local Ollama, …).
//! - **Claude Code subscription OAuth** ([`crate::providers::Backend::OpenAi`],
//!   [`crate::providers::AuthKind::Oauth`]) — the built-in `claude-max`
//!   provider reads Claude Code's OAuth credential from the macOS Keychain and
//!   uses Anthropic's OpenAI-compatible endpoint without an API key or credits.
//!
//! Following Pi, secrets never live in `models.json`: each preset's `api_key`
//! field is a **config-value** (`$VAR` / `!cmd` / literal) resolved at use time
//! by [`crate::auth`]. Adding a provider is a JSON row, not Rust.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use tracing::info;

pub const CONNECTION_ID_PREFIX: &str = "connection-";
pub const CONNECTION_MARKER: &str = "agui_connection";
pub const CONNECTION_LABEL: &str = "label";
pub const CONNECTION_VISION: &str = "vision";
pub const CLAUDE_MAX_ID: &str = "claude-max";
pub const CLAUDE_MAX_MODEL: &str = "claude-sonnet-4-5-20250929";
/// Pi CLI versions whose RPC contract has been **checked against this
/// adapter**, newest last. An installed version outside this list is rejected
/// rather than assumed compatible: the adapter reads a wire protocol Pi does
/// not promise to keep, and a silent shape change would present as an agent
/// that connects, accepts a prompt, and streams nothing.
///
/// This is a list rather than one pin because "revalidated" is a fact about a
/// version, not a replacement for the previous fact. Adding an entry means
/// somebody actually drove the protocol surface in
/// [`crate::turn_loop::pi`] against that binary: the `get_state`, `prompt` and
/// `abort` requests, and the `agent_start` / `agent_settled` /
/// `message_update`→`assistantMessageEvent`→`text_delta` / `tool_execution_start`
/// / `tool_execution_end` events. 0.82.1 was checked on 2026-07-27 — it adds
/// `turn_start`/`turn_end`/`tool_execution_update`, which this adapter ignores,
/// and changes nothing it reads.
pub const PI_RPC_VERSIONS: &[&str] = &["0.81.1", "0.82.1"];

/// The newest revalidated version, for messages that name a single one.
pub fn pi_rpc_version() -> &'static str {
    PI_RPC_VERSIONS
        .last()
        .copied()
        .expect("PI_RPC_VERSIONS is never empty")
}

pub fn pi_rpc_supported(version: &str) -> bool {
    PI_RPC_VERSIONS.contains(&version)
}

#[derive(Debug, Error)]
pub enum ProviderConfigError {
    #[error("failed to read provider config {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid provider config {}: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
}

/// How the host drives a provider.
#[derive(Debug, Clone)]
pub enum Backend {
    /// A spawned ACP subprocess: `program` + `args` put it into agent mode.
    Acp { program: String, args: Vec<String> },
    /// A spawned Pi subprocess driven through `--mode rpc`. `builtin_tools`
    /// is passed to Pi's `--tools` allowlist alongside the exact registered
    /// Surface action names.
    PiRpc {
        program: String,
        args: Vec<String>,
        builtin_tools: Vec<String>,
    },
    /// An OpenAI Chat Completions-compatible HTTP endpoint.
    OpenAi,
}

impl Backend {
    /// Stable transport/adapter name exposed to setup and provider status UIs.
    pub fn adapter(&self) -> &'static str {
        match self {
            Self::Acp { .. } => "acp",
            Self::PiRpc { .. } => "pi-rpc",
            Self::OpenAi => "openai-compatible",
        }
    }

    /// Managed agents own their model login and execute as subprocesses.
    pub fn is_managed_agent(&self) -> bool {
        matches!(self, Self::Acp { .. } | Self::PiRpc { .. })
    }

    /// The provider executable, when this backend is process-backed.
    pub fn program(&self) -> Option<&str> {
        match self {
            Self::Acp { program, .. } | Self::PiRpc { program, .. } => Some(program),
            Self::OpenAi => None,
        }
    }
}

/// How a provider authenticates — the seam that lets the UI treat keys and
/// subscriptions uniformly (Pi's `ProviderAuth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// The ACP subprocess handles its own credentials.
    Managed,
    /// A bring-your-own API key, sent as a bearer token.
    ApiKey,
    /// A subscription OAuth token sent as bearer auth with the provider's
    /// required OAuth beta header. This is never an API-key fallback.
    Oauth,
}

/// One selectable provider the switcher exposes.
#[derive(Clone)]
pub struct Provider {
    /// Stable id used in the URL/config (`claude`, `claude-max`, `groq`, …).
    pub id: String,
    /// Human label for the UI.
    pub label: String,
    /// One-line note on how it authenticates (shown in the UI / logs).
    pub auth_note: String,
    /// How the host drives it.
    pub backend: Backend,
    /// How it authenticates.
    pub auth: AuthKind,
    /// For OpenAI-backed providers: the endpoint base URL (`None` for the BYOK
    /// slot, whose base/model come from the ⚙ form).
    pub base_url: Option<String>,
    /// For OpenAI-backed providers: the model id.
    pub model: Option<String>,
    /// The `models.json` `api_key` config-value (`$VAR` / `!cmd` / literal),
    /// resolved by [`crate::auth`]. `None` for built-ins that rely purely on a
    /// stored key or an env fallback.
    pub key_source: Option<String>,
    /// Environment variables to fall back to (and to name in "needs key" hints).
    pub env_vars: Vec<String>,
    /// The model can SEE images. When true, `read_canvas` on the OpenAI path
    /// attaches a rendered PNG of the scene so the tutor gets a real visual
    /// feedback loop. Off by default — a text-only model (Groq Llama, a local
    /// Ollama) would reject or waste an image, so it stays text-only there.
    pub vision: bool,
}

impl Provider {
    fn acp(id: &str, label: &str, program: &str, args: &[&str], auth_note: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            auth_note: auth_note.into(),
            backend: Backend::Acp {
                program: program.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
            },
            auth: AuthKind::Managed,
            base_url: None,
            model: None,
            key_source: None,
            env_vars: Vec::new(),
            vision: false, // ACP agents get the image via an MCP image block, not this flag
        }
    }

    fn claude_max() -> Self {
        Self {
            id: CLAUDE_MAX_ID.into(),
            label: "Claude (subscription · fast)".into(),
            auth_note:
                "Claude Code subscription OAuth from macOS Keychain; no API key or API credits"
                    .into(),
            backend: Backend::OpenAi,
            auth: AuthKind::Oauth,
            base_url: Some("https://api.anthropic.com/v1".into()),
            model: Some(CLAUDE_MAX_MODEL.into()),
            // This built-in is deliberately resolved by the typed Keychain JSON
            // path in `auth`; it must never fall through to a generic command,
            // stored API key, or environment variable.
            key_source: None,
            env_vars: Vec::new(),
            vision: true,
        }
    }

    fn pi_rpc() -> Self {
        Self {
            id: "pi".into(),
            label: "Pi (local RPC)".into(),
            auth_note: format!(
                "Pi {} selected model and local credentials; AG-UI actions use the authenticated MCP bridge",
                pi_rpc_version()
            ),
            backend: Backend::PiRpc {
                program: "pi".into(),
                args: Vec::new(),
                // Pi's ordinary coding tools are explicit rather than inherited
                // from whatever its global configuration happens to enable.
                builtin_tools: ["read", "bash", "edit", "write", "grep", "find", "ls"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            },
            auth: AuthKind::Managed,
            base_url: None,
            model: None,
            key_source: None,
            env_vars: Vec::new(),
            vision: false,
        }
    }

    /// A user-created, persisted OpenAI Chat Completions-compatible endpoint.
    pub fn connection(
        id: String,
        label: String,
        base_url: String,
        model: String,
        vision: bool,
    ) -> Result<Self, String> {
        if !valid_connection_id(&id) {
            return Err("invalid connection id".to_string());
        }
        let label = label.trim();
        if label.is_empty() || label.len() > 80 {
            return Err("connection name must be 1-80 characters".to_string());
        }
        let model = model.trim();
        if model.is_empty() || model.len() > 200 {
            return Err("model must be 1-200 characters".to_string());
        }
        let base_url = crate::turn_loop::openai::canonical_base_url(&base_url)?;
        Ok(Self {
            id,
            label: label.to_string(),
            auth_note: format!("saved API connection · {base_url}"),
            backend: Backend::OpenAi,
            auth: AuthKind::ApiKey,
            base_url: Some(base_url),
            model: Some(model.to_string()),
            key_source: None,
            env_vars: Vec::new(),
            vision,
        })
    }
}

pub fn valid_connection_id(id: &str) -> bool {
    id.strip_prefix(CONNECTION_ID_PREFIX).is_some_and(|suffix| {
        !suffix.is_empty()
            && id.len() <= 80
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

pub fn is_user_connection(provider: &Provider) -> bool {
    valid_connection_id(&provider.id)
}

/// Restore user-created connections from the same 0600 auth store that holds
/// their secrets. Malformed records fail closed instead of becoming selectable.
pub fn saved_connections(auth: &crate::auth::AuthStore) -> Result<Vec<Provider>, String> {
    let mut connections = Vec::new();
    for (id, env) in auth.api_key_settings() {
        if env.get(CONNECTION_MARKER).map(String::as_str) != Some("true") {
            continue;
        }
        let label = env.get(CONNECTION_LABEL).cloned().unwrap_or_default();
        let base_url = env
            .get(crate::turn_loop::openai::BASE_URL_SETTING)
            .cloned()
            .unwrap_or_default();
        let model = env
            .get(crate::turn_loop::openai::MODEL_SETTING)
            .cloned()
            .unwrap_or_default();
        let vision = env.get(CONNECTION_VISION).map(String::as_str) == Some("true");
        connections.push(Provider::connection(id, label, base_url, model, vision)?);
    }
    connections.sort_by_key(|connection| connection.label.to_lowercase());
    Ok(connections)
}

/// The built-in providers whose required executables are available, plus the
/// in-process OpenAI-compatible slot.
fn builtin() -> Vec<Provider> {
    let providers = vec![
        Provider::claude_max(),
        Provider::acp(
            "claude",
            "Claude (subscription · ACP)",
            "npx",
            &["-y", "@agentclientprotocol/claude-agent-acp@0.58.1"],
            "Claude Code subscription / OAuth (no API key)",
        ),
        Provider::acp(
            "gemini",
            "Gemini",
            "gemini",
            &["--experimental-acp"],
            "GEMINI_API_KEY or `gemini` OAuth login",
        ),
        Provider::acp(
            "opencode",
            "opencode",
            "opencode",
            &["acp"],
            "opencode's own provider config",
        ),
        Provider::acp(
            "codex",
            "Codex (OpenAI)",
            "npx",
            &["-y", "@agentclientprotocol/codex-acp@1.1.2"],
            "ChatGPT subscription or OPENAI_API_KEY",
        ),
        Provider::pi_rpc(),
        Provider {
            id: "openai".into(),
            label: "Existing API connection".into(),
            auth_note: "Legacy single-slot connection; add named API connections for new endpoints"
                .into(),
            backend: Backend::OpenAi,
            auth: AuthKind::ApiKey,
            base_url: None, // base/model come from the ⚙ form
            model: None,
            key_source: None,
            env_vars: vec!["OPENAI_API_KEY".into()],
            // BYOK model is unknown; stay text-only so a non-vision endpoint
            // (a local Ollama, a text model) isn't sent an image it rejects.
            vision: false,
        },
    ];
    providers
        .into_iter()
        .filter(|provider| match &provider.backend {
            Backend::Acp { program, .. } | Backend::PiRpc { program, .. }
                if !crate::config::command_available(program) =>
            {
                info!(
                    provider = %provider.id,
                    executable = %program,
                    "provider omitted because its executable is unavailable"
                );
                false
            }
            Backend::Acp { .. } | Backend::PiRpc { .. } => true,
            Backend::OpenAi => true,
        })
        .collect()
}

/// Build the full registry: available built-ins plus any `models.json`
/// presets. A missing optional file yields built-ins; a present unreadable or
/// malformed file is a startup error rather than silently changing the
/// advertised provider set.
pub fn build(models_path: &Path) -> Result<Vec<Provider>, ProviderConfigError> {
    build_with_requirement(models_path, false)
}

/// Build the full registry while requiring the configured `models.json` to
/// exist. Applications use this for an explicit environment override so a
/// typo cannot silently fall back to built-in providers.
pub fn build_required(models_path: &Path) -> Result<Vec<Provider>, ProviderConfigError> {
    build_with_requirement(models_path, true)
}

fn build_with_requirement(
    models_path: &Path,
    required: bool,
) -> Result<Vec<Provider>, ProviderConfigError> {
    let mut providers = builtin();
    match std::fs::read_to_string(models_path) {
        Ok(text) => {
            let before = providers.len();
            load_into(&mut providers, &text, models_path)?;
            info!(
                "provider presets: loaded {} from {}",
                providers.len() - before,
                models_path.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => {
            info!(
                "provider presets: none ({} not found)",
                models_path.display()
            );
        }
        Err(source) => {
            return Err(ProviderConfigError::Read {
                path: models_path.to_path_buf(),
                source,
            });
        }
    }
    Ok(providers)
}

/// Parse `models.json` and append its provider presets (OpenAI-compatible only).
fn load_into(
    providers: &mut Vec<Provider>,
    text: &str,
    path: &Path,
) -> Result<(), ProviderConfigError> {
    let invalid = |message: String| ProviderConfigError::Invalid {
        path: path.to_path_buf(),
        message,
    };
    let root: Value =
        serde_json::from_str(text).map_err(|error| invalid(format!("malformed JSON: {error}")))?;
    let list = root
        .get("providers")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("expected a top-level `providers` array".to_string()))?;
    for (index, entry) in list.iter().enumerate() {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid(format!(
                "provider row {index} has an invalid id {id:?}"
            )));
        }
        if providers.iter().any(|p| p.id == id) {
            return Err(invalid(format!(
                "provider id {id:?} is duplicated or collides with a built-in"
            )));
        }
        // Only the OpenAI Chat Completions wire is supported today.
        let api = entry
            .get("api")
            .and_then(Value::as_str)
            .unwrap_or("openai-completions");
        if api != "openai-completions" {
            return Err(invalid(format!(
                "provider {id:?} uses unsupported api {api:?}"
            )));
        }
        let base_url = entry
            .get("base_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .trim_end_matches('/')
            .to_string();
        let model = entry
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if base_url.is_empty() || model.is_empty() {
            return Err(invalid(format!(
                "provider {id:?} needs non-empty base_url and model"
            )));
        }
        let base_url = crate::turn_loop::openai::canonical_base_url(&base_url)
            .map_err(|error| invalid(format!("provider {id:?} has invalid base_url: {error}")))?;
        // Managed subscription credentials belong to their ACP agent. API
        // presets accept API keys only; reusing a CLI OAuth token here is not
        // a supported subscription route.
        let auth = match entry.get("auth").and_then(Value::as_str) {
            Some("oauth") => {
                return Err(invalid(format!(
                    "provider {id:?} cannot use managed subscription OAuth; use its ACP agent"
                )));
            }
            Some("api_key") | None => AuthKind::ApiKey,
            Some(other) => {
                return Err(invalid(format!(
                    "provider {id:?} uses unsupported auth {other:?}"
                )));
            }
        };
        let key_source = entry
            .get("api_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        // Env-var fallbacks + the "needs key" hint come from any `$VAR`s the
        // key source references.
        let env_vars = key_source
            .as_deref()
            .map(crate::auth::env_var_names)
            .unwrap_or_default();
        let label = entry
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string();
        let auth_note = match auth {
            AuthKind::Oauth => format!("OAuth token · {base_url}"),
            _ => format!("openai-compatible · {base_url}"),
        };
        // Opt a preset into the visual feedback loop with `"vision": true` (its
        // model must accept OpenAI `image_url` content — Claude, GPT-4o-class,
        // Claude-via-OpenRouter). Defaults off.
        let vision = match entry.get("vision") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| invalid(format!("provider {id:?} `vision` must be a boolean")))?,
            None => false,
        };
        providers.push(Provider {
            id,
            label,
            auth_note,
            backend: Backend::OpenAi,
            auth,
            base_url: Some(base_url),
            model: Some(model),
            key_source,
            env_vars,
            vision,
        });
    }
    Ok(())
}

/// Look up a provider by id within a registry.
pub fn find<'a>(list: &'a [Provider], id: &str) -> Option<&'a Provider> {
    list.iter().find(|p| p.id == id)
}

/// Choose the provider to start with. Environment policy wins
/// (`AGUI_PROVIDER`, then legacy `CANVAS_PROVIDER`), followed by the
/// application's preferred id. The compatibility fallbacks keep richer
/// existing apps on their previous Claude selection while allowing an
/// OpenAI-only starter to fall through to its runnable `openai` slot.
pub fn default_id_with(
    list: &[Provider],
    preferred: Option<&str>,
) -> Result<Option<String>, String> {
    for env_name in ["AGUI_PROVIDER", "CANVAS_PROVIDER"] {
        match std::env::var(env_name) {
            Ok(id) => return select_default_id(list, preferred, Some((env_name, &id))),
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(format!("{env_name} is not valid Unicode"));
            }
        }
    }
    select_default_id(list, preferred, None)
}

fn select_default_id(
    list: &[Provider],
    preferred: Option<&str>,
    environment: Option<(&str, &str)>,
) -> Result<Option<String>, String> {
    if let Some((name, raw_id)) = environment {
        let id = raw_id.trim();
        if id.is_empty() {
            return Err(format!("{name} must not be empty"));
        }
        if !list.iter().any(|provider| provider.id == id) {
            return Err(format!("{name} selects unknown provider `{id}`"));
        }
        return Ok(Some(id.to_string()));
    }
    if let Some(raw_id) = preferred {
        let id = raw_id.trim();
        if id.is_empty() {
            return Err("the configured default provider must not be empty".into());
        }
        if !list.iter().any(|provider| provider.id == id) {
            return Err(format!("configured default provider `{id}` is not enabled"));
        }
        return Ok(Some(id.to_string()));
    }
    for id in [CLAUDE_MAX_ID, "claude", "openai"] {
        if list.iter().any(|p| p.id == id) {
            return Ok(Some(id.to_string()));
        }
    }
    Ok(list.first().map(|provider| provider.id.clone()))
}

/// Backward-compatible default selection with no application preference.
pub fn default_id(list: &[Provider]) -> Result<Option<String>, String> {
    default_id_with(list, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Vec<Provider>, ProviderConfigError> {
        let mut providers = Vec::new();
        load_into(&mut providers, text, Path::new("models.json"))?;
        Ok(providers)
    }

    #[test]
    fn config_is_fail_closed_and_normalizes_safe_base_urls() {
        let providers = parse(
            r#"{"providers":[{"id":"local","base_url":"https://example.test/v1/","model":"m"}]}"#,
        )
        .unwrap();
        assert_eq!(
            providers[0].base_url.as_deref(),
            Some("https://example.test/v1")
        );

        for base_url in [
            "https://user@example.test/v1",
            "https://example.test/v1?q=x",
            "https://example.test/v1#x",
        ] {
            let text = format!(
                r#"{{"providers":[{{"id":"unsafe","base_url":"{base_url}","model":"m"}}]}}"#
            );
            assert!(matches!(
                parse(&text),
                Err(ProviderConfigError::Invalid { .. })
            ));
        }
        assert!(matches!(
            parse("{not-json"),
            Err(ProviderConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn managed_subscription_oauth_is_rejected_from_api_presets() {
        let error = match parse(
            r#"{"providers":[{"id":"subscription","base_url":"https://example.test/v1","model":"m","auth":"oauth"}]}"#,
        ) {
            Ok(_) => panic!("managed subscription OAuth must not be accepted as an API preset"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("use its ACP agent"));
    }

    #[test]
    fn claude_max_is_a_first_class_keychain_oauth_provider() {
        let providers = builtin();
        let provider = find(&providers, CLAUDE_MAX_ID).expect("claude-max built-in");
        assert!(matches!(provider.backend, Backend::OpenAi));
        assert_eq!(provider.auth, AuthKind::Oauth);
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.anthropic.com/v1")
        );
        assert_eq!(provider.model.as_deref(), Some(CLAUDE_MAX_MODEL));
        assert!(provider.key_source.is_none());
        assert!(provider.env_vars.is_empty());
    }

    #[test]
    fn saved_connections_restore_without_exposing_or_copying_the_key() {
        let path = std::env::temp_dir().join(format!(
            "ag-ui-saved-connection-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&path);
        let auth = crate::auth::AuthStore::open(path.clone()).unwrap();
        let mut env = std::collections::HashMap::new();
        env.insert(CONNECTION_MARKER.to_string(), "true".to_string());
        env.insert(CONNECTION_LABEL.to_string(), "Team Router".to_string());
        env.insert(
            crate::turn_loop::openai::BASE_URL_SETTING.to_string(),
            "https://openrouter.ai/api/v1".to_string(),
        );
        env.insert(
            crate::turn_loop::openai::MODEL_SETTING.to_string(),
            "openai/gpt-4o-mini".to_string(),
        );
        auth.update_api_key_entry(
            "connection-team-router",
            env,
            crate::auth::ApiKeyUpdate::Set("secret-not-in-provider".to_string()),
        )
        .unwrap();

        let restored = saved_connections(&auth).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].label, "Team Router");
        assert_eq!(restored[0].model.as_deref(), Some("openai/gpt-4o-mini"));
        assert!(restored[0].key_source.is_none());
        assert_eq!(
            auth.resolve(&restored[0]).key.as_deref(),
            Some("secret-not-in-provider")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn explicit_missing_config_is_an_error_but_default_missing_config_is_optional() {
        let path = std::env::temp_dir().join(format!(
            "ag-ui-provider-config-missing-{}-{}.json",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&path);

        assert!(build(&path).is_ok());
        assert!(matches!(
            build_required(&path),
            Err(ProviderConfigError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn explicit_provider_selection_never_falls_through() {
        let providers = parse(
            r#"{"providers":[{"id":"openai","base_url":"https://example.test/v1","model":"m"}]}"#,
        )
        .expect("provider fixture");

        assert_eq!(
            select_default_id(&providers, None, Some(("AGUI_PROVIDER", " openai "))),
            Ok(Some("openai".into()))
        );
        assert!(
            select_default_id(&providers, None, Some(("AGUI_PROVIDER", "")))
                .is_err_and(|error| error.contains("must not be empty"))
        );
        assert!(
            select_default_id(&providers, None, Some(("AGUI_PROVIDER", "missing")))
                .is_err_and(|error| error.contains("unknown provider"))
        );
        assert!(select_default_id(&providers, Some("missing"), None)
            .is_err_and(|error| error.contains("is not enabled")));
        assert!(select_default_id(&providers, Some("  "), None)
            .is_err_and(|error| error.contains("must not be empty")));
    }

    #[test]
    fn adapter_package_versions_are_pinned() {
        let providers = builtin();
        for id in ["claude", "codex"] {
            if let Some(provider) = find(&providers, id) {
                let Backend::Acp { args, .. } = &provider.backend else {
                    panic!("{id} must use ACP");
                };
                assert!(args.iter().any(|arg| arg.rsplit_once('@').is_some_and(
                    |(package, version)| package.starts_with("@agentclientprotocol/")
                        && !version.is_empty()
                        && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
                )));
            }
        }
        let pi = Provider::pi_rpc();
        assert!(matches!(pi.backend, Backend::PiRpc { .. }));
        // The gate must stay a gate: an unchecked version is refused, and a
        // checked one is accepted. Both directions, so a future "just make it
        // work" edit that turns this into a floor fails here.
        assert!(
            pi_rpc_supported("0.81.1"),
            "0.81.1 was the original contract"
        );
        assert!(
            pi_rpc_supported("0.82.1"),
            "0.82.1 was probed on 2026-07-27"
        );
        assert!(
            !pi_rpc_supported("0.83.0"),
            "an unprobed version must be refused"
        );
        assert!(!pi_rpc_supported(""), "no version is not a version");
        assert_eq!(
            pi_rpc_version(),
            "0.82.1",
            "messages name the newest checked build"
        );
    }
}
