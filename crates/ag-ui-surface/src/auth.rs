//! Pi-style per-provider credential store (`auth.json`).
//!
//! A faithful, scoped copy of how earendil-works/pi handles keys and
//! subscriptions:
//!
//! - Credentials live in a gitignored **`auth.json`** (mode `0600`), keyed by
//!   provider id, each tagged **`api_key`** or **`oauth`** — separate from
//!   `models.json` (which only defines providers/models, no secrets).
//! - Values resolve with Pi's rules: **`!cmd`** (shell out) · **`$VAR`/`${VAR}`**
//!   (env, with `${...}` and concatenation) · **`$$`/`$!`** escapes · literal.
//! - Read-back follows Pi's priority: **runtime override → stored → models.json
//!   key → env-var fallback** — a stored credential always wins.
//!
//! The teaching canvas is a web app, not a TUI, so the `/login` picker becomes a
//! per-provider settings panel (see `static/lesson.js`);
//! [`crate::auth::AuthStore::status`]
//! is what feeds its ready / needs-key badges.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use crate::providers::{AuthKind, Provider};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_COMMAND_OUTPUT: u64 = 64 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Credential-store failures. Corrupt or unwritable auth state is never
/// treated as an empty/successful store.
#[derive(Debug, thiserror::Error)]
pub enum AuthStoreError {
    /// The store could not be read or atomically replaced.
    #[error("auth store {operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Existing JSON is malformed.
    #[error("auth store contains invalid JSON at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// In-memory credentials could not be serialized.
    #[error("auth store serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// Fail-closed reasons the built-in Claude Code subscription credential cannot
/// be used. Every message names the broken authority boundary without exposing
/// any token material.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClaudeCodeOauthError {
    #[error(
        "Claude subscription unavailable: the USER account name is missing, so macOS Keychain item \"Claude Code-credentials\" cannot be read"
    )]
    MissingAccount,
    #[error("Claude subscription unavailable: failed to start /usr/bin/security: {0}")]
    StartKeychain(#[source] std::io::Error),
    #[error(
        "Claude subscription unavailable: macOS Keychain lookup for \"Claude Code-credentials\" exceeded the 5 second timeout"
    )]
    KeychainTimeout,
    #[error(
        "Claude subscription unavailable: macOS Keychain item \"Claude Code-credentials\" was not found for account {account}; run `claude` to sign in"
    )]
    KeychainItemMissing { account: String },
    #[error(
        "Claude subscription unavailable: macOS Keychain item \"Claude Code-credentials\" could not be read for account {account}: {detail}"
    )]
    KeychainRead { account: String, detail: String },
    #[error(
        "Claude subscription unavailable: macOS Keychain item \"Claude Code-credentials\" contains invalid JSON: {0}"
    )]
    InvalidJson(#[source] serde_json::Error),
    #[error(
        "Claude subscription unavailable: macOS Keychain item \"Claude Code-credentials\" is missing object `claudeAiOauth`"
    )]
    MissingOauthObject,
    #[error(
        "Claude subscription unavailable: macOS Keychain item \"Claude Code-credentials\" is missing string `claudeAiOauth.accessToken`"
    )]
    MissingAccessToken,
    #[error(
        "Claude subscription unavailable: `claudeAiOauth.accessToken` in macOS Keychain item \"Claude Code-credentials\" is empty"
    )]
    EmptyAccessToken,
    #[error(
        "Claude subscription unavailable: macOS Keychain item \"Claude Code-credentials\" is missing numeric `claudeAiOauth.expiresAt`"
    )]
    MissingExpiresAt,
    #[error(
        "Claude subscription unavailable: `claudeAiOauth.accessToken` expired at {expires_at_ms} milliseconds since Unix epoch; run `claude` to refresh the subscription login"
    )]
    Expired { expires_at_ms: u64 },
    #[error("Claude subscription unavailable: system time is before the Unix epoch")]
    InvalidSystemTime,
    #[cfg(not(target_os = "macos"))]
    #[error(
        "Claude subscription unavailable: Claude Code Keychain OAuth is only available on macOS"
    )]
    UnsupportedPlatform,
}

/// One stored credential — mirrors Pi's tagged union in `auth.json`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    /// A raw API key (itself a config-value: literal / `$VAR` / `!cmd`). The
    /// optional `env` map mirrors Pi's provider-scoped extra config.
    ApiKey {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
    },
    /// An OAuth credential retained for auth.json compatibility. Managed
    /// subscription agents own their credentials and do not use this record.
    Oauth {
        access: String,
        #[serde(default)]
        refresh: String,
        #[serde(default)]
        expires: i64,
    },
}

/// Secret portion of one atomic provider-settings transaction.
pub enum ApiKeyUpdate {
    Keep,
    Set(String),
    Remove,
}

/// A resolved key plus where it came from (drives status/logging).
pub struct Resolved {
    pub key: Option<String>,
    pub source: &'static str,
    pub label: Option<String>,
    /// A credential-resolution failure safe to expose to the operator. It
    /// never includes token material.
    pub error: Option<String>,
}

impl Resolved {
    fn found(key: String, source: &'static str, label: Option<String>) -> Self {
        Self {
            key: Some(key),
            source,
            label,
            error: None,
        }
    }
    fn none() -> Self {
        Self {
            key: None,
            source: "needs_key",
            label: None,
            error: None,
        }
    }
    fn failed(source: &'static str, error: impl ToString) -> Self {
        Self {
            key: None,
            source,
            label: None,
            error: Some(error.to_string()),
        }
    }
}

/// Per-provider readiness for the UI badge (never carries the key itself).
pub struct AuthStatus {
    /// `managed` (ACP subprocess) · `api_key` · `oauth`.
    pub kind: &'static str,
    /// Whether the provider has enough configuration to attempt a request.
    /// This is not proof that a model request succeeds.
    pub ready: bool,
    /// `subscription` · `stored` · `runtime` · `environment` · `command` ·
    /// `models_json` · `managed` · `needs_key`.
    pub source: &'static str,
    /// A short human hint (environment variable, stored key, managed login).
    pub detail: Option<String>,
}

/// The credential store, backed by `auth.json`.
pub struct AuthStore {
    path: PathBuf,
    /// Persisted credentials (`auth.json`).
    creds: Mutex<HashMap<String, Credential>>,
    /// In-memory, non-persisted overrides (Pi's `setRuntimeApiKey`). Unused by
    /// the current UI, but part of the read priority for completeness.
    runtime: Mutex<HashMap<String, String>>,
}

impl AuthStore {
    /// Open (and load, if present) the store at `path`.
    pub fn open(path: PathBuf) -> Result<Self, AuthStoreError> {
        harden_existing_permissions(&path)?;
        let creds =
            match std::fs::read_to_string(&path) {
                Ok(text) => serde_json::from_str::<HashMap<String, Credential>>(&text).map_err(
                    |source| AuthStoreError::Parse {
                        path: path.clone(),
                        source,
                    },
                )?,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
                Err(source) => {
                    return Err(AuthStoreError::Io {
                        operation: "read",
                        path,
                        source,
                    });
                }
            };
        if !creds.is_empty() {
            info!(
                "auth.json: loaded {} stored credential(s) from {}",
                creds.len(),
                path.display()
            );
        }
        Ok(Self {
            path,
            creds: Mutex::new(creds),
            runtime: Mutex::new(HashMap::new()),
        })
    }

    /// Default host-state location beside, but never inside, the credential
    /// document. Keeping this derivation here avoids exposing the secret-store
    /// path as public application data while giving every [`crate::App`] a
    /// stable activity journal without requiring another builder call.
    pub(crate) fn default_activity_path(&self) -> PathBuf {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(
                || PathBuf::from("activity.json"),
                |parent| parent.join("activity.json"),
            )
    }

    /// Atomically persist a complete snapshot to disk with mode `0600`.
    fn persist_snapshot(&self, creds: &HashMap<String, Credential>) -> Result<(), AuthStoreError> {
        let text = serde_json::to_vec_pretty(creds).map_err(AuthStoreError::Serialize)?;
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| AuthStoreError::Io {
                operation: "create parent directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = self
            .path
            .with_extension(format!("tmp-{}-{sequence}", std::process::id()));
        let write_result = (|| -> Result<(), AuthStoreError> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp_path)
                .map_err(|source| AuthStoreError::Io {
                    operation: "create temporary file",
                    path: temp_path.clone(),
                    source,
                })?;
            file.write_all(&text).map_err(|source| AuthStoreError::Io {
                operation: "write temporary file",
                path: temp_path.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| AuthStoreError::Io {
                operation: "sync temporary file",
                path: temp_path.clone(),
                source,
            })?;
            std::fs::rename(&temp_path, &self.path).map_err(|source| AuthStoreError::Io {
                operation: "replace",
                path: self.path.clone(),
                source,
            })?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        write_result
    }

    /// Store (or replace) an API key for a provider and persist.
    pub fn set_api_key(&self, provider: &str, key: String) -> Result<(), AuthStoreError> {
        // Keep the read-modify-persist-assign transaction under one synchronous
        // lock. Persisting a cloned snapshot without retaining this guard lets
        // two concurrent writers both start from the same old map and silently
        // overwrite one another when they later re-lock to assign it.
        let mut creds = self.creds.lock();
        let mut candidate = creds.clone();
        let env = match candidate.get(provider) {
            Some(Credential::ApiKey { env, .. }) => env.clone(),
            _ => None,
        };
        candidate.insert(provider.to_string(), Credential::ApiKey { key, env });
        self.persist_snapshot(&candidate)?;
        *creds = candidate;
        info!("auth.json: saved API key for '{provider}'");
        Ok(())
    }

    /// Read a provider's persisted, non-secret settings. Pi's credential shape
    /// already has this provider-scoped `env` map; the generic OpenAI-compatible
    /// slot uses it for `base_url` + `model`, so clicking **Save** survives a
    /// process restart instead of silently falling back to OpenAI defaults.
    pub fn api_key_env(&self, provider: &str) -> HashMap<String, String> {
        match self.creds.lock().get(provider) {
            Some(Credential::ApiKey { env, .. }) => env.clone().unwrap_or_default(),
            _ => HashMap::new(),
        }
    }

    /// Snapshot persisted API-key records without exposing their secrets.
    /// Used to rebuild user-created connection metadata at startup.
    pub fn api_key_settings(&self) -> Vec<(String, HashMap<String, String>)> {
        self.creds
            .lock()
            .iter()
            .filter_map(|(id, credential)| match credential {
                Credential::ApiKey { env, .. } => {
                    Some((id.clone(), env.clone().unwrap_or_default()))
                }
                Credential::Oauth { .. } => None,
            })
            .collect()
    }

    /// Remove an entire persisted record, including its non-secret settings.
    /// This is intentionally distinct from [`Self::remove`], which logs out
    /// while preserving a connection definition.
    pub fn delete_entry(&self, provider: &str) -> Result<bool, AuthStoreError> {
        let mut creds = self.creds.lock();
        let mut candidate = creds.clone();
        let removed = candidate.remove(provider).is_some();
        if removed {
            self.persist_snapshot(&candidate)?;
            *creds = candidate;
            self.runtime.lock().remove(provider);
            info!("auth.json: deleted connection '{provider}'");
        }
        Ok(removed)
    }

    /// Persist a provider's non-secret settings without replacing its key.
    /// A config-only entry may have an empty key (useful for a local endpoint);
    /// [`Self::status`] deliberately does not count that as authenticated.
    pub fn set_api_key_env(
        &self,
        provider: &str,
        env: HashMap<String, String>,
    ) -> Result<(), AuthStoreError> {
        let mut creds = self.creds.lock();
        let mut candidate = creds.clone();
        let key = match candidate.get(provider) {
            Some(Credential::ApiKey { key, .. }) => key.clone(),
            _ => String::new(),
        };
        let env = (!env.is_empty()).then_some(env);
        candidate.insert(provider.to_string(), Credential::ApiKey { key, env });
        self.persist_snapshot(&candidate)?;
        *creds = candidate;
        info!("auth.json: saved provider settings for '{provider}'");
        Ok(())
    }

    /// Atomically update a provider's non-secret settings and secret in one
    /// snapshot replacement. No partial settings-only write is visible when a
    /// key set/removal would otherwise be the second persistence operation.
    pub fn update_api_key_entry(
        &self,
        provider: &str,
        env: HashMap<String, String>,
        secret: ApiKeyUpdate,
    ) -> Result<(), AuthStoreError> {
        let mut creds = self.creds.lock();
        // OAuth credentials have no `env` field. A settings-only/API-key
        // transaction must never silently replace an OAuth token with an
        // empty API-key record; callers that want to change credential kind
        // must do so explicitly with `Set` or remove it with `Remove`.
        if matches!(secret, ApiKeyUpdate::Keep)
            && matches!(creds.get(provider), Some(Credential::Oauth { .. }))
        {
            return Ok(());
        }
        let mut candidate = creds.clone();
        let existing_key = match candidate.get(provider) {
            Some(Credential::ApiKey { key, .. }) => key.clone(),
            _ => String::new(),
        };
        let key = match secret {
            ApiKeyUpdate::Keep => existing_key,
            ApiKeyUpdate::Set(key) => key,
            ApiKeyUpdate::Remove => String::new(),
        };
        let env = (!env.is_empty()).then_some(env);
        if key.is_empty() && env.is_none() {
            candidate.remove(provider);
        } else {
            candidate.insert(provider.to_string(), Credential::ApiKey { key, env });
        }
        if candidate == *creds {
            return Ok(());
        }
        self.persist_snapshot(&candidate)?;
        *creds = candidate;
        info!("auth.json: atomically updated credentials/settings for '{provider}'");
        Ok(())
    }

    /// Forget a provider's stored secret (Pi's `/logout`) while retaining its
    /// non-secret endpoint/model settings. A config-only API-key entry remains
    /// on disk with an empty key; entries without settings are removed.
    pub fn remove(&self, provider: &str) -> Result<bool, AuthStoreError> {
        let mut creds = self.creds.lock();
        let mut candidate = creds.clone();
        let removed = match candidate.get(provider).cloned() {
            Some(Credential::ApiKey { env: Some(env), .. }) if !env.is_empty() => {
                candidate.insert(
                    provider.to_string(),
                    Credential::ApiKey {
                        key: String::new(),
                        env: Some(env),
                    },
                );
                true
            }
            Some(_) => candidate.remove(provider).is_some(),
            None => false,
        };
        if removed {
            self.persist_snapshot(&candidate)?;
            *creds = candidate;
            info!("auth.json: removed stored secret for '{provider}'");
        }
        Ok(removed)
    }

    /// Resolve the effective key for a provider, following Pi's priority:
    /// runtime override → stored → models.json `key_source` → env-var fallback.
    /// May run a shell command (for `!cmd` key sources, e.g. the keychain read).
    pub fn resolve(&self, provider: &Provider) -> Resolved {
        // `claude-max` is a deliberately closed credential path. It never
        // accepts a runtime override, stored API key, models.json command, or
        // environment fallback: only Claude Code's typed Keychain OAuth record
        // can authorize this provider.
        if provider.id == crate::providers::CLAUDE_MAX_ID {
            return match resolve_claude_code_oauth_token() {
                Ok(token) => Resolved::found(
                    token,
                    "keychain",
                    Some("Claude Code-credentials.claudeAiOauth.accessToken".to_string()),
                ),
                Err(error) => Resolved::failed("subscription_error", error),
            };
        }

        // 1. runtime override (in-memory, never persisted)
        if let Some(k) = self.runtime.lock().get(&provider.id) {
            if !k.is_empty() {
                return Resolved::found(k.clone(), "runtime", None);
            }
        }
        // 2. stored credential (auth.json)
        let stored = self.creds.lock().get(&provider.id).cloned();
        if let Some(cred) = stored {
            match cred {
                Credential::ApiKey { key, .. } => {
                    if let Some(v) = resolve_config_value(&key) {
                        return Resolved::found(v, "stored", None);
                    }
                }
                Credential::Oauth { access, .. } => {
                    if !access.is_empty() {
                        return Resolved::found(access, "stored", None);
                    }
                }
            }
        }
        // 3. models.json key source (`$VAR` / `!cmd` / literal)
        if let Some(src) = provider.key_source.as_deref() {
            if let Some(v) = resolve_config_value(src) {
                let (source, label) = source_of(src);
                return Resolved::found(v, source, label);
            }
        }
        // 4. environment fallback
        for var in &provider.env_vars {
            if let Ok(v) = std::env::var(var) {
                let v = v.trim().to_string();
                if !v.is_empty() {
                    return Resolved::found(v, "environment", Some(var.clone()));
                }
            }
        }
        Resolved::none()
    }

    /// Configuration availability + source for the UI, without exposing the key. Availability for
    /// API-key providers is a *presence* check (Pi doesn't run `!cmd`s just to
    /// render a badge); OAuth providers resolve their credential so the badge
    /// reflects real credential state without claiming request health.
    pub fn status(&self, provider: &Provider) -> AuthStatus {
        let kind = match provider.auth {
            AuthKind::Managed => "managed",
            AuthKind::ApiKey => "api_key",
            AuthKind::Oauth => "oauth",
        };

        // Managed subprocess providers own their login. This says the adapter
        // is available, not that a model request has succeeded; only the
        // runtime's post-prime `ready` state proves that.
        if matches!(provider.auth, AuthKind::Managed) {
            return AuthStatus {
                kind,
                ready: true,
                source: "managed",
                detail: Some(provider.auth_note.clone()),
            };
        }

        // OAuth: resolve the credential, but describe only credential
        // availability. A present token is not subscription-health proof; only
        // a completed model request establishes that.
        if matches!(provider.auth, AuthKind::Oauth) {
            let resolved = self.resolve(provider);
            let ready = resolved.key.is_some();
            return AuthStatus {
                kind,
                ready,
                source: resolved.source,
                detail: resolved.error.or_else(|| {
                    ready.then(|| {
                        "Claude Code OAuth token present; request health is reported separately by runtime state"
                            .to_string()
                    })
                }),
            };
        }

        // API key: presence checks (no command execution).
        if self.runtime.lock().contains_key(&provider.id) {
            return AuthStatus {
                kind,
                ready: true,
                source: "runtime",
                detail: Some("session key".into()),
            };
        }
        if let Some(credential) = self.creds.lock().get(&provider.id) {
            let has_secret = match credential {
                Credential::ApiKey { key, .. } => !key.trim().is_empty(),
                Credential::Oauth { access, .. } => !access.trim().is_empty(),
            };
            if has_secret {
                return AuthStatus {
                    kind,
                    ready: true,
                    source: "stored",
                    detail: Some("saved key".into()),
                };
            }
        }
        if let Some(src) = provider.key_source.as_deref() {
            let s = src.trim();
            if s.starts_with('!') {
                if resolve_config_value(s).is_some() {
                    return AuthStatus {
                        kind,
                        ready: true,
                        source: "command",
                        detail: None,
                    };
                }
                return AuthStatus {
                    kind,
                    ready: false,
                    source: "needs_key",
                    detail: Some("configured credential command failed".to_string()),
                };
            }
            let vars = env_var_names(s);
            if vars.is_empty() {
                // literal key baked into models.json
                return AuthStatus {
                    kind,
                    ready: true,
                    source: "models_json",
                    detail: None,
                };
            }
            if vars.iter().all(|v| {
                std::env::var(v)
                    .map(|x| !x.trim().is_empty())
                    .unwrap_or(false)
            }) {
                return AuthStatus {
                    kind,
                    ready: true,
                    source: "environment",
                    detail: Some(vars.join(", ")),
                };
            }
        }
        for var in &provider.env_vars {
            if std::env::var(var)
                .map(|x| !x.trim().is_empty())
                .unwrap_or(false)
            {
                return AuthStatus {
                    kind,
                    ready: true,
                    source: "environment",
                    detail: Some(var.clone()),
                };
            }
        }
        // Nothing configured — name the env var it looks for, as a hint.
        let hint = provider.env_vars.first().cloned();
        AuthStatus {
            kind,
            ready: false,
            source: "needs_key",
            detail: hint,
        }
    }
}

#[cfg(unix)]
fn harden_existing_permissions(path: &std::path::Path) -> Result<(), AuthStoreError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AuthStoreError::Io {
                operation: "inspect permissions",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Err(AuthStoreError::Io {
            operation: "inspect permissions",
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "credential store is not a regular file",
            ),
        });
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(|source| AuthStoreError::Io {
            operation: "set permissions",
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_existing_permissions(_path: &std::path::Path) -> Result<(), AuthStoreError> {
    Ok(())
}

/// Resolve Claude Code's subscription token from the one authoritative JSON
/// location in the macOS Keychain. The blob may contain unrelated MCP OAuth
/// records with their own `accessToken`; only
/// `claudeAiOauth.accessToken` is accepted.
pub(crate) fn resolve_claude_code_oauth_token() -> Result<String, ClaudeCodeOauthError> {
    let raw = read_claude_code_keychain_json()?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ClaudeCodeOauthError::InvalidSystemTime)?
        .as_millis();
    let now_ms = u64::try_from(now_ms).map_err(|_| ClaudeCodeOauthError::InvalidSystemTime)?;
    parse_claude_code_oauth_token(&raw, now_ms)
}

fn parse_claude_code_oauth_token(raw: &str, now_ms: u64) -> Result<String, ClaudeCodeOauthError> {
    let root: Value = serde_json::from_str(raw).map_err(ClaudeCodeOauthError::InvalidJson)?;
    let oauth = root
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .ok_or(ClaudeCodeOauthError::MissingOauthObject)?;
    let token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .ok_or(ClaudeCodeOauthError::MissingAccessToken)?
        .trim();
    if token.is_empty() {
        return Err(ClaudeCodeOauthError::EmptyAccessToken);
    }
    let expires_at_ms = oauth
        .get("expiresAt")
        .and_then(Value::as_u64)
        .ok_or(ClaudeCodeOauthError::MissingExpiresAt)?;
    if expires_at_ms <= now_ms {
        return Err(ClaudeCodeOauthError::Expired { expires_at_ms });
    }
    Ok(token.to_string())
}

#[cfg(target_os = "macos")]
fn read_claude_code_keychain_json() -> Result<String, ClaudeCodeOauthError> {
    let account = std::env::var("USER")
        .ok()
        .filter(|account| !account.trim().is_empty())
        .ok_or(ClaudeCodeOauthError::MissingAccount)?;
    let mut process = Command::new("/usr/bin/security");
    process
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-a",
            &account,
            "-w",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = process
        .spawn()
        .map_err(ClaudeCodeOauthError::StartKeychain)?;
    let process_group = child.id();
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_command(&mut child, process_group);
                return Err(ClaudeCodeOauthError::KeychainTimeout);
            }
            Err(error) => {
                terminate_command(&mut child, process_group);
                return Err(ClaudeCodeOauthError::KeychainRead {
                    account,
                    detail: format!("failed while waiting for /usr/bin/security: {error}"),
                });
            }
        }
    };
    terminate_process_group(process_group);

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| ClaudeCodeOauthError::KeychainRead {
            account: account.clone(),
            detail: "security command exposed no stdout".to_string(),
        })?
        .take(MAX_COMMAND_OUTPUT + 1)
        .read_to_end(&mut stdout)
        .map_err(|error| ClaudeCodeOauthError::KeychainRead {
            account: account.clone(),
            detail: format!("failed to read security stdout: {error}"),
        })?;
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .ok_or_else(|| ClaudeCodeOauthError::KeychainRead {
            account: account.clone(),
            detail: "security command exposed no stderr".to_string(),
        })?
        .take(MAX_COMMAND_OUTPUT + 1)
        .read_to_end(&mut stderr)
        .map_err(|error| ClaudeCodeOauthError::KeychainRead {
            account: account.clone(),
            detail: format!("failed to read security stderr: {error}"),
        })?;
    if stdout.len() > MAX_COMMAND_OUTPUT as usize || stderr.len() > MAX_COMMAND_OUTPUT as usize {
        return Err(ClaudeCodeOauthError::KeychainRead {
            account,
            detail: "security command output exceeded 64 KiB".to_string(),
        });
    }
    let detail = String::from_utf8_lossy(&stderr).trim().to_string();
    if !status.success() {
        if status.code() == Some(44) || detail.to_ascii_lowercase().contains("could not be found") {
            return Err(ClaudeCodeOauthError::KeychainItemMissing { account });
        }
        return Err(ClaudeCodeOauthError::KeychainRead {
            account,
            detail: if detail.is_empty() {
                format!("/usr/bin/security exited with {status}")
            } else {
                detail
            },
        });
    }
    let raw = String::from_utf8(stdout).map_err(|error| ClaudeCodeOauthError::KeychainRead {
        account: account.clone(),
        detail: format!("security stdout was not UTF-8: {error}"),
    })?;
    if raw.trim().is_empty() {
        return Err(ClaudeCodeOauthError::KeychainRead {
            account,
            detail: "security returned an empty credential blob".to_string(),
        });
    }
    Ok(raw)
}

#[cfg(not(target_os = "macos"))]
fn read_claude_code_keychain_json() -> Result<String, ClaudeCodeOauthError> {
    Err(ClaudeCodeOauthError::UnsupportedPlatform)
}

/// Resolve a config-value the way Pi does: `!cmd` (shell stdout) · `$VAR`/
/// `${VAR}` (env interpolation) · `$$`/`$!` escapes · literal. Returns `None`
/// when a referenced env var is unset or a command fails/prints nothing.
pub fn resolve_config_value(spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some(cmd) = spec.strip_prefix('!') {
        return run_config_command(cmd);
    }
    interpolate_env(spec)
}

fn run_config_command(command: &str) -> Option<String> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = process.spawn().ok()?;
    let process_group = child.id();
    let stdout = child.stdout.take()?;
    let (reader_tx, reader_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAX_COMMAND_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = reader_tx.send(result);
    });

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_command(&mut child, process_group);
                return None;
            }
            Err(_) => {
                terminate_command(&mut child, process_group);
                return None;
            }
        }
    };
    // The shell may exit after spawning a background descendant that inherited
    // stdout. Terminate any process still in this invocation's isolated group
    // so pipe collection cannot outlive the configured timeout.
    terminate_process_group(process_group);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let bytes = reader_rx.recv_timeout(remaining).ok()?.ok()?;
    if !status.success() || bytes.len() > MAX_COMMAND_OUTPUT as usize {
        return None;
    }
    let value = String::from_utf8(bytes).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn terminate_command(child: &mut Child, process_group: u32) {
    terminate_process_group(process_group);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_process_group(process_group: u32) {
    let Ok(process_group) = i32::try_from(process_group) else {
        return;
    };
    // SAFETY: `process_group` is the positive pid returned by `Child::id` for
    // a command created with `process_group(0)`. Negating it addresses only
    // that command's isolated process group; SIGKILL requires no Rust-owned
    // pointer or memory invariants.
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_process_group: u32) {}

/// Interpolate `$VAR` / `${VAR}` (with `$$`→`$`, `$!`→`!`). Any unset referenced
/// variable makes the whole value unresolved (`None`), matching Pi.
fn interpolate_env(spec: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                chars.next();
                out.push('$');
            }
            Some('!') => {
                chars.next();
                out.push('!');
            }
            Some('{') => {
                chars.next();
                let mut name = String::new();
                for n in chars.by_ref() {
                    if n == '}' {
                        break;
                    }
                    name.push(n);
                }
                out.push_str(std::env::var(&name).ok()?.trim());
            }
            Some(n) if n == '_' || n.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n == '_' || n.is_ascii_alphanumeric() {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(std::env::var(&name).ok()?.trim());
            }
            _ => out.push('$'),
        }
    }
    Some(out)
}

/// The env-var names a config-value references (for status labels + fallback).
pub(crate) fn env_var_names(spec: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut chars = spec.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            continue;
        }
        match chars.peek().copied() {
            Some('$') | Some('!') => {
                chars.next();
            }
            Some('{') => {
                chars.next();
                let mut name = String::new();
                for n in chars.by_ref() {
                    if n == '}' {
                        break;
                    }
                    name.push(n);
                }
                if !name.is_empty() {
                    names.push(name);
                }
            }
            Some(n) if n == '_' || n.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n == '_' || n.is_ascii_alphanumeric() {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                names.push(name);
            }
            _ => {}
        }
    }
    names
}

/// Classify a resolved key source for status/logging.
fn source_of(spec: &str) -> (&'static str, Option<String>) {
    let s = spec.trim();
    if s.starts_with('!') {
        return ("command", None);
    }
    let vars = env_var_names(s);
    if vars.is_empty() {
        ("models_json", None)
    } else {
        ("environment", Some(vars.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_auth_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ag-ui-surface-{name}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn provider_settings_and_key_survive_restart_without_clobbering_each_other() {
        let path = temp_auth_path("provider-settings");
        let store = AuthStore::open(path.clone()).expect("temporary auth store should open");

        let mut env = HashMap::new();
        env.insert("base_url".into(), "https://api.deepseek.com/v1".into());
        env.insert("model".into(), "deepseek-v4-pro".into());
        store
            .set_api_key_env("openai", env.clone())
            .expect("provider settings should persist");
        store
            .set_api_key("openai", "test-key".into())
            .expect("API key should persist");
        drop(store);

        let reopened = AuthStore::open(path.clone()).expect("persisted auth store should reopen");
        assert_eq!(reopened.api_key_env("openai"), env);
        match reopened.creds.lock().get("openai") {
            Some(Credential::ApiKey { key, .. }) => assert_eq!(key, "test-key"),
            _ => panic!("openai API key was not persisted"),
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn combined_provider_update_persists_settings_and_secret_in_one_snapshot() {
        let path = temp_auth_path("combined-provider-update");
        let store = AuthStore::open(path.clone()).expect("temporary auth store should open");
        let env = HashMap::from([
            (
                "base_url".to_string(),
                "https://example.test/v1".to_string(),
            ),
            ("model".to_string(), "model-a".to_string()),
        ]);
        store
            .update_api_key_entry(
                "openai",
                env.clone(),
                ApiKeyUpdate::Set("combined-secret".to_string()),
            )
            .expect("combined update should persist");
        drop(store);

        let reopened = AuthStore::open(path.clone()).expect("combined store should reopen");
        assert_eq!(reopened.api_key_env("openai"), env);
        match reopened.creds.lock().get("openai") {
            Some(Credential::ApiKey { key, .. }) => assert_eq!(key, "combined-secret"),
            _ => panic!("combined credential was not persisted"),
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn api_key_keep_never_replaces_an_oauth_credential() {
        let path = temp_auth_path("oauth-keep");
        let store = AuthStore::open(path.clone()).expect("temporary auth store should open");
        let oauth = Credential::Oauth {
            access: "oauth-access".to_string(),
            refresh: "oauth-refresh".to_string(),
            expires: 42,
        };
        store
            .persist_snapshot(&HashMap::from([(
                "subscription".to_string(),
                oauth.clone(),
            )]))
            .expect("OAuth fixture should persist");
        store
            .creds
            .lock()
            .insert("subscription".to_string(), oauth.clone());

        store
            .update_api_key_entry("subscription", HashMap::new(), ApiKeyUpdate::Keep)
            .expect("empty settings update should be a no-op");
        store
            .update_api_key_entry(
                "subscription",
                HashMap::from([("model".to_string(), "must-not-convert".to_string())]),
                ApiKeyUpdate::Keep,
            )
            .expect("settings update should preserve OAuth");
        assert!(store.creds.lock().get("subscription") == Some(&oauth));

        drop(store);
        let reopened = AuthStore::open(path.clone()).expect("OAuth store should reopen");
        assert!(reopened.creds.lock().get("subscription") == Some(&oauth));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn api_key_remove_explicitly_removes_an_oauth_credential() {
        let path = temp_auth_path("oauth-remove");
        let store = AuthStore::open(path.clone()).expect("temporary auth store should open");
        let oauth = Credential::Oauth {
            access: "oauth-access".to_string(),
            refresh: "oauth-refresh".to_string(),
            expires: 42,
        };
        store
            .persist_snapshot(&HashMap::from([(
                "subscription".to_string(),
                oauth.clone(),
            )]))
            .expect("OAuth fixture should persist");
        store.creds.lock().insert("subscription".to_string(), oauth);

        store
            .update_api_key_entry("subscription", HashMap::new(), ApiKeyUpdate::Remove)
            .expect("explicit removal should persist");
        assert!(!store.creds.lock().contains_key("subscription"));
        drop(store);
        let reopened = AuthStore::open(path.clone()).expect("credential store should reopen");
        assert!(!reopened.creds.lock().contains_key("subscription"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn removing_key_preserves_provider_settings_across_restart() {
        let path = temp_auth_path("remove-key-preserve-settings");
        let store = AuthStore::open(path.clone()).expect("temporary auth store should open");
        let env = HashMap::from([
            (
                "base_url".to_string(),
                "http://localhost:11434/v1".to_string(),
            ),
            ("model".to_string(), "local-model".to_string()),
        ]);
        store
            .set_api_key_env("openai", env.clone())
            .expect("provider settings should persist");
        store
            .set_api_key("openai", "secret".to_string())
            .expect("API key should persist");

        assert!(store.remove("openai").expect("key removal should persist"));
        drop(store);

        let reopened = AuthStore::open(path.clone()).expect("auth store should reopen");
        assert_eq!(reopened.api_key_env("openai"), env);
        match reopened.creds.lock().get("openai") {
            Some(Credential::ApiKey { key, .. }) => assert!(key.is_empty()),
            _ => panic!("provider settings entry was removed with its key"),
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn claude_oauth_parser_selects_only_the_claude_ai_token() {
        let raw = r#"{
            "mcpOAuth": {
                "unrelated": {
                    "accessToken": ""
                }
            },
            "claudeAiOauth": {
                "accessToken": "claude-subscription-token",
                "expiresAt": 2000
            }
        }"#;
        let token =
            parse_claude_code_oauth_token(raw, 1000).expect("Claude OAuth fixture should parse");
        assert_eq!(token, "claude-subscription-token");
    }

    #[test]
    fn claude_oauth_parser_fails_loudly_for_missing_or_expired_token() {
        let missing = r#"{
            "mcpOAuth": {
                "unrelated": {
                    "accessToken": "must-not-be-used"
                }
            },
            "claudeAiOauth": {
                "expiresAt": 2000
            }
        }"#;
        let missing_error = parse_claude_code_oauth_token(missing, 1000)
            .expect_err("missing claudeAiOauth.accessToken must fail");
        assert!(matches!(
            missing_error,
            ClaudeCodeOauthError::MissingAccessToken
        ));
        assert!(missing_error
            .to_string()
            .contains("claudeAiOauth.accessToken"));

        let expired = r#"{"claudeAiOauth":{"accessToken":"expired-token","expiresAt":999}}"#;
        let expired_error = parse_claude_code_oauth_token(expired, 1000)
            .expect_err("expired Claude Code token must fail");
        assert!(matches!(
            expired_error,
            ClaudeCodeOauthError::Expired { expires_at_ms: 999 }
        ));
        assert!(expired_error.to_string().contains("expired"));
    }

    #[test]
    fn config_command_kills_background_stdout_holders() {
        let started = Instant::now();
        let value = resolve_config_value("!printf bounded; sleep 30 &");
        assert_eq!(value.as_deref(), Some("bounded"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "background descendants must not hold config resolution open"
        );
    }

    #[test]
    fn corrupt_auth_json_is_an_error() {
        let path = temp_auth_path("corrupt");
        std::fs::write(&path, b"{not-json").expect("temporary corrupt fixture should write");
        let result = AuthStore::open(path.clone());
        assert!(matches!(result, Err(AuthStoreError::Parse { .. })));
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn opening_an_existing_store_repairs_secret_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_auth_path("repair-permissions");
        std::fs::write(&path, b"{}").expect("temporary auth fixture should write");
        let mut permissions = std::fs::metadata(&path)
            .expect("auth fixture metadata")
            .permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&path, permissions).expect("make fixture overly permissive");

        AuthStore::open(path.clone()).expect("existing auth store should open");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("hardened auth metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn concurrent_provider_updates_do_not_lose_committed_entries() {
        const WRITERS: usize = 16;

        let path = temp_auth_path("concurrent-updates");
        let store = Arc::new(AuthStore::open(path.clone()).expect("auth store should open"));
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut writers = Vec::with_capacity(WRITERS);

        for index in 0..WRITERS {
            let store = store.clone();
            let barrier = barrier.clone();
            writers.push(std::thread::spawn(move || {
                barrier.wait();
                store
                    .set_api_key(&format!("provider-{index}"), format!("key-{index}"))
                    .expect("concurrent API key should persist");
            }));
        }
        for writer in writers {
            writer.join().expect("credential writer should not panic");
        }

        let reopened = AuthStore::open(path.clone()).expect("auth store should reopen");
        let creds = reopened.creds.lock();
        assert_eq!(creds.len(), WRITERS);
        for index in 0..WRITERS {
            match creds.get(&format!("provider-{index}")) {
                Some(Credential::ApiKey { key, .. }) => assert_eq!(key, &format!("key-{index}")),
                _ => panic!("provider-{index} update was lost"),
            }
        }

        drop(creds);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_persistence_does_not_change_in_memory_credentials() {
        let path = temp_auth_path("failed-persistence");
        let parent = path.with_extension("parent");
        std::fs::create_dir_all(&parent).expect("temporary parent should be created");
        let store_path = parent.join("auth.json");
        let store = AuthStore::open(store_path).expect("auth store should open");

        std::fs::remove_dir(&parent).expect("empty temporary parent should be removable");
        std::fs::write(&parent, b"not-a-directory")
            .expect("blocking parent fixture should be created");

        assert!(store
            .set_api_key("openai", "must-not-stick".into())
            .is_err());
        assert!(!store.creds.lock().contains_key("openai"));

        let _ = std::fs::remove_file(parent);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_combined_update_changes_neither_settings_nor_secret_in_memory() {
        let parent = temp_auth_path("failed-combined-parent");
        std::fs::create_dir_all(&parent).expect("temporary parent should be created");
        let store_path = parent.join("auth.json");
        let store = AuthStore::open(store_path.clone()).expect("auth store should open");
        let initial_env = HashMap::from([("model".to_string(), "model-before".to_string())]);
        store
            .update_api_key_entry(
                "openai",
                initial_env.clone(),
                ApiKeyUpdate::Set("key-before".to_string()),
            )
            .expect("initial combined update should persist");

        std::fs::remove_file(&store_path).expect("remove persisted fixture");
        std::fs::remove_dir(&parent).expect("remove temporary parent");
        std::fs::write(&parent, b"not-a-directory").expect("block persistence parent");
        let changed_env = HashMap::from([("model".to_string(), "model-after".to_string())]);
        assert!(store
            .update_api_key_entry(
                "openai",
                changed_env,
                ApiKeyUpdate::Set("key-after".to_string()),
            )
            .is_err());
        assert_eq!(store.api_key_env("openai"), initial_env);
        match store.creds.lock().get("openai") {
            Some(Credential::ApiKey { key, .. }) => assert_eq!(key, "key-before"),
            _ => panic!("failed combined update changed credential shape"),
        }
        let _ = std::fs::remove_file(parent);
    }
}
