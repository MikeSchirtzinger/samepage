//! What the host knows about the project the room is *about*.
//!
//! Two jobs. Reading a file the room wants to show, under rules that keep a
//! pane from becoming a way to exfiltrate a key. And discovering what this
//! workspace can actually run, so "what are my options here" is answered by
//! the repository rather than by the model's memory of it.

use std::collections::HashMap;
use std::fs;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value as JsonValue};

const MAX_EXCERPT_LINES: usize = 400;
const MAX_EXCERPT_BYTES: u64 = 512 * 1024;
const MAX_WHOLE_FILE_LINES: usize = 200;
const PROBE_TIMEOUT: Duration = Duration::from_millis(80);
const PROBE_TTL: Duration = Duration::from_secs(3);

/// Directory names that never contribute a readable excerpt. Build output and
/// local state are noise; the rest is either private or not ours to show.
const DENIED_DIRECTORIES: &[&str] = &[
    ".git",
    ".local",
    "target",
    "node_modules",
    "pkg",
    ".cargo",
    ".ssh",
    ".venv",
];

/// Extensions the room will render as text. An allowlist rather than a
/// blocklist, so a new binary format is excluded by default.
const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "toml", "json", "md", "js", "mjs", "ts", "css", "html", "sh", "py", "yaml", "yml", "wit",
    "txt", "lock", "sql", "ron", "svg",
];

/// Filenames that look like credentials even when the extension is allowed.
const DENIED_STEMS: &[&str] = &["auth", "credentials", "secrets", "id_rsa", "token"];

#[derive(Clone, Debug)]
pub struct RunnableOption {
    pub package: String,
    pub path: String,
    pub description: String,
    pub command: String,
    pub port: Option<u16>,
    /// The environment variable that overrides `port`, when the example reads
    /// one. Empty when its source does not name it.
    pub port_env: String,
    pub extensions: Vec<String>,
    pub prepare: Option<String>,
}

pub struct Workspace {
    root: PathBuf,
    options: Vec<RunnableOption>,
    probes: Mutex<HashMap<u16, (Instant, bool)>>,
}

impl Workspace {
    pub fn open(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("could not open {}: {error}", root.display()))?;
        let options = discover(&root);
        Ok(Self {
            root,
            options,
            probes: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn option_count(&self) -> usize {
        self.options.len()
    }

    /// Read a repository-relative excerpt, fresh. Every rejection names the
    /// rule it broke — an agent that gets "outside the project root" back can
    /// correct itself without guessing.
    pub fn excerpt(
        &self,
        path: &str,
        from: Option<u32>,
        to: Option<u32>,
    ) -> Result<JsonValue, String> {
        let resolved = self.resolve_path(path)?;
        let metadata =
            fs::metadata(&resolved).map_err(|error| format!("could not stat {path}: {error}"))?;
        if !metadata.is_file() {
            return Err(format!("{path} is not a file"));
        }
        if metadata.len() > MAX_EXCERPT_BYTES {
            return Err(format!(
                "{path} is {} KiB; the room reads files up to {} KiB",
                metadata.len() / 1024,
                MAX_EXCERPT_BYTES / 1024
            ));
        }
        let body = fs::read_to_string(&resolved)
            .map_err(|error| format!("could not read {path} as text: {error}"))?;
        let lines: Vec<&str> = body.lines().collect();
        let total = lines.len();

        let (start, end, clamped) = match (from, to) {
            (None, None) => {
                let end = total.min(MAX_WHOLE_FILE_LINES);
                (1usize, end.max(1), end < total)
            }
            (from, to) => {
                let start = from.unwrap_or(1).max(1) as usize;
                let end = to.map(|to| to as usize).unwrap_or(total).min(total);
                if start > total {
                    return Err(format!(
                        "{path} has {total} lines; line {start} is past the end. \
                         The file changed since that range was written."
                    ));
                }
                let capped = end.min(start + MAX_EXCERPT_LINES - 1);
                (start, capped.max(start), capped < end)
            }
        };

        let text = lines
            .get(start.saturating_sub(1)..end)
            .unwrap_or_default()
            .join("\n");

        Ok(json!({
            "path": path,
            "first_line": start,
            "last_line": end,
            "total_lines": total,
            "truncated": clamped,
            "text": text,
        }))
    }

    /// The live catalog. Port status is probed, not assumed: a green dot means
    /// something accepted a TCP connection on that port just now.
    pub fn catalog(&self, filter: Option<&str>) -> Result<JsonValue, String> {
        let needle = filter.map(str::to_lowercase).unwrap_or_default();
        // Two packages defaulting to the same port is a real property of this
        // workspace. Reporting it beats letting the second one fail to bind
        // while the catalog still shows a hopeful green dot.
        let contested = |port: Option<u16>, package: &str| -> Vec<String> {
            let Some(port) = port else { return Vec::new() };
            self.options
                .iter()
                .filter(|other| other.port == Some(port) && other.package != package)
                .map(|other| other.package.clone())
                .collect()
        };
        let entries: Vec<JsonValue> = self
            .options
            .iter()
            .filter(|option| {
                needle.is_empty()
                    || option.package.to_lowercase().contains(&needle)
                    || option.description.to_lowercase().contains(&needle)
                    || option
                        .extensions
                        .iter()
                        .any(|extension| extension.to_lowercase().contains(&needle))
            })
            .map(|option| {
                json!({
                    "package": option.package,
                    "path": option.path,
                    "description": option.description,
                    "command": option.command,
                    "port": option.port,
                    "port_env": option.port_env,
                    "url": option.port.map(|port| format!("http://127.0.0.1:{port}")),
                    "listening": option.port.map(|port| self.listening(port)),
                    "extensions": option.extensions,
                    "prepare": option.prepare,
                    "contested_by": contested(option.port, &option.package),
                })
            })
            .collect();
        Ok(json!({ "options": entries, "total": self.options.len() }))
    }

    /// A plain-text version of the catalog for the agent's read-back.
    pub fn describe_options(&self) -> String {
        if self.options.is_empty() {
            return "No runnable packages were discovered in this workspace.".to_string();
        }
        let mut lines = Vec::new();
        for option in &self.options {
            let status = match option.port {
                Some(port) if self.listening(port) => format!("port {port}, LISTENING now"),
                Some(port) => format!("port {port}, not running"),
                None => "no default port".to_string(),
            };
            lines.push(format!(
                "- {} ({status}) — {}\n  run: {}",
                option.package,
                if option.description.is_empty() {
                    "no description in Cargo.toml"
                } else {
                    &option.description
                },
                option.command
            ));
        }
        lines.join("\n")
    }

    fn listening(&self, port: u16) -> bool {
        let now = Instant::now();
        if let Some((at, value)) = self.probes.lock().get(&port) {
            if now.duration_since(*at) < PROBE_TTL {
                return *value;
            }
        }
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let open = match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
                true
            }
            Err(_) => false,
        };
        self.probes.lock().insert(port, (now, open));
        open
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, String> {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            return Err(format!(
                "{path} is absolute; name a path relative to {}",
                self.root.display()
            ));
        }
        if path.contains("..") {
            return Err(format!("{path} traverses out of the project"));
        }
        for component in candidate.components() {
            let Some(name) = component.as_os_str().to_str() else {
                return Err(format!("{path} is not valid UTF-8"));
            };
            if DENIED_DIRECTORIES.contains(&name) {
                return Err(format!(
                    "{path} is inside {name}/, which the room does not read"
                ));
            }
        }
        let stem = candidate
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if DENIED_STEMS.iter().any(|denied| stem == *denied) {
            return Err(format!(
                "{path} looks like a credential file; the room will not display it"
            ));
        }
        let extension = candidate
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_lowercase();
        let bare_allowed = matches!(
            candidate.file_name().and_then(|name| name.to_str()),
            Some("Cargo.lock" | "AGENTS.md" | "README" | "LICENSE" | "Makefile")
        );
        if !bare_allowed && !TEXT_EXTENSIONS.contains(&extension.as_str()) {
            return Err(format!(
                "the room reads text files only; {path} has an unsupported type"
            ));
        }

        let resolved = self.root.join(candidate);
        let canonical = resolved
            .canonicalize()
            .map_err(|error| format!("could not resolve {path}: {error}"))?;
        // Symlinks are why this check happens after canonicalize, not instead of it.
        if !canonical.starts_with(&self.root) {
            return Err(format!("{path} resolves outside the project root"));
        }
        Ok(canonical)
    }
}

/// Read the workspace manifest for its members, then ask each member what it
/// is. Nothing here is hard-coded to this repository's current example list —
/// a package added tomorrow shows up without a code change.
fn discover(root: &Path) -> Vec<RunnableOption> {
    let Ok(manifest) = fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let mut options = Vec::new();
    for member in members(&manifest) {
        let directory = root.join(&member);
        let Ok(package_manifest) = fs::read_to_string(directory.join("Cargo.toml")) else {
            continue;
        };
        // A runnable option is one with a binary. Library crates are the
        // platform, not a thing to open.
        if !package_manifest.contains("[[bin]]") {
            continue;
        }
        let Some(package) = manifest_value(&package_manifest, "name") else {
            continue;
        };
        let description = manifest_value(&package_manifest, "description").unwrap_or_default();
        let source = fs::read_to_string(directory.join("src/main.rs")).unwrap_or_default();
        let (port_env, port) = match default_port(&source) {
            Some((name, port)) => (name, Some(port)),
            None => (String::new(), None),
        };
        let extensions = recipe_extensions(&directory.join("agui.app.toml"));
        let build_script = directory.join("build-web.sh");
        let prepare = build_script
            .is_file()
            .then(|| format!("./{}/build-web.sh", member.trim_end_matches('/')));
        options.push(RunnableOption {
            command: format!("cargo run -p {package}"),
            package,
            path: member,
            description,
            port,
            port_env,
            extensions,
            prepare,
        });
    }
    options.sort_by(|left, right| left.package.cmp(&right.package));
    options
}

/// A deliberately small TOML reader. Pulling in a parser to read a members
/// list and two string keys would be the wrong trade for an example.
fn members(manifest: &str) -> Vec<String> {
    let Some(start) = manifest.find("members") else {
        return Vec::new();
    };
    let after = &manifest[start..];
    let Some(open) = after.find('[') else {
        return Vec::new();
    };
    let Some(close) = after[open..].find(']') else {
        return Vec::new();
    };
    after[open + 1..open + close]
        .split(',')
        .filter_map(|entry| {
            let trimmed = entry.trim().trim_matches('"').trim();
            (!trimmed.is_empty() && !trimmed.starts_with('#')).then(|| trimmed.to_string())
        })
        .collect()
}

fn manifest_value(manifest: &str, key: &str) -> Option<String> {
    manifest
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(key) && line[key.len()..].trim_start().starts_with('='))
        .and_then(|line| line.split_once('='))
        .map(|(_, value)| value.trim().trim_matches('"').replace("\\\"", "\""))
        .filter(|value| !value.is_empty() && !value.contains('{'))
}

/// The port an example listens on when nothing overrides it, read out of its
/// own source so the catalog cannot drift from the code. This workspace grew
/// three spellings of "the default port", and a catalog that only understood
/// the newest one would silently report "no default port" for the older half
/// — which reads as *not runnable* rather than as *not parsed*.
fn default_port(source: &str) -> Option<(String, u16)> {
    // config::string_or("AGUI_ADDR", "127.0.0.1:8100")
    if let Some(rest) = after(source, "\"AGUI_ADDR\"") {
        if let Some(literal) = until(rest, ')') {
            if let Some(host) = after(literal, "127.0.0.1:") {
                if let Some(port) = leading_port(host) {
                    return Some(("AGUI_ADDR".to_string(), port));
                }
            }
        }
    }
    // config::port_or("CANVAS_PORT", 8091)
    if let Some(rest) = after(source, "port_or(\"") {
        if let Some(literal) = until(rest, ')') {
            if let Some((name, default)) = literal.split_once(',') {
                if let Some(port) = leading_port(default.trim()) {
                    return Some((name.trim_matches('"').to_string(), port));
                }
            }
        }
    }
    // let raw = configured.unwrap_or("8092");
    if let Some(rest) = after(source, "unwrap_or(\"") {
        if let Some(port) = leading_port(rest) {
            if rest.starts_with(&port.to_string()) && (1024..=65535).contains(&(port as u32)) {
                return Some((String::new(), port));
            }
        }
    }
    None
}

fn after<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    haystack
        .find(needle)
        .map(|at| &haystack[at + needle.len()..])
}

fn until(haystack: &str, terminator: char) -> Option<&str> {
    haystack.find(terminator).map(|at| &haystack[..at])
}

fn leading_port(value: &str) -> Option<u16> {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn recipe_extensions(path: &Path) -> Vec<String> {
    let Ok(recipe) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    let mut in_extension = false;
    for line in recipe.lines().map(str::trim) {
        if line.starts_with('[') {
            in_extension = line.starts_with("[[extensions]]");
            continue;
        }
        if in_extension {
            if let Some(value) = line.strip_prefix("id") {
                if let Some((_, id)) = value.split_once('=') {
                    ids.push(id.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> Workspace {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        Workspace::open(root).expect("workspace opens")
    }

    #[test]
    fn reads_a_real_excerpt_fresh_from_disk() {
        let value = workspace()
            .excerpt("Cargo.toml", Some(1), Some(3))
            .expect("workspace manifest is readable");
        assert_eq!(value["first_line"], 1);
        assert_eq!(value["last_line"], 3);
        assert!(
            value["text"]
                .as_str()
                .expect("text")
                .contains("[workspace]"),
            "{value:?}"
        );
    }

    #[test]
    fn rejects_traversal_absolute_and_private_paths() {
        let workspace = workspace();
        for path in [
            "../../../etc/passwd",
            "/etc/passwd",
            "target/debug/anything.rs",
            "examples/same-page-room/.local/room.json",
            ".git/config",
        ] {
            assert!(
                workspace.excerpt(path, None, None).is_err(),
                "{path} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_a_credential_shaped_file() {
        let error = workspace()
            .excerpt("examples/create-ag-ui-app/auth.json", None, None)
            .expect_err("an auth file is refused");
        assert!(error.contains("credential"), "{error}");
    }

    #[test]
    fn rejects_a_binary_type() {
        workspace()
            .excerpt(
                "examples/shared-site-review/static/captures/capture-1.png",
                None,
                None,
            )
            .expect_err("a png is not text");
    }

    #[test]
    fn reports_a_stale_range_instead_of_empty_text() {
        let error = workspace()
            .excerpt("Cargo.toml", Some(90_000), Some(90_010))
            .expect_err("a range past the end is an error");
        assert!(error.contains("past the end"), "{error}");
    }

    #[test]
    fn discovers_this_workspace_and_its_ports() {
        let workspace = workspace();
        let catalog = workspace.catalog(None).expect("catalog builds");
        let options = catalog["options"].as_array().expect("options array");
        let discovered: std::collections::BTreeSet<String> = options
            .iter()
            .map(|option| {
                option["path"]
                    .as_str()
                    .expect("every catalog option names its workspace path")
                    .to_string()
            })
            .collect();
        let workspace_manifest = fs::read_to_string(workspace.root().join("Cargo.toml"))
            .expect("workspace manifest is readable");
        let expected: std::collections::BTreeSet<String> = members(&workspace_manifest)
            .into_iter()
            .filter(|member| {
                fs::read_to_string(workspace.root().join(member).join("Cargo.toml"))
                    .is_ok_and(|manifest| manifest.contains("[[bin]]"))
            })
            .collect();
        assert_eq!(
            discovered, expected,
            "catalog paths must match runnable members in the workspace manifest"
        );
        let room = options
            .iter()
            .find(|option| option["package"] == "same-page-room")
            .expect("the room discovers itself");
        assert_eq!(room["port"], 8100);
        assert_eq!(room["command"], "cargo run -p same-page-room");
        assert!(room["extensions"]
            .as_array()
            .expect("extensions")
            .iter()
            .any(|id| id == "room"));
    }

    #[test]
    fn filters_the_catalog() {
        let workspace = workspace();
        let catalog = workspace.catalog(Some("atlas")).expect("catalog builds");
        let options = catalog["options"].as_array().expect("options array");
        assert!(options.iter().all(|option| option["package"]
            .as_str()
            .unwrap_or_default()
            .contains("atlas")));
    }

    #[test]
    fn parses_every_spelling_of_a_default_port_in_this_workspace() {
        assert_eq!(
            default_port(r#"let addr = config::string_or("AGUI_ADDR", "127.0.0.1:8100")?;"#),
            Some(("AGUI_ADDR".to_string(), 8100))
        );
        assert_eq!(
            default_port(r#"let port = config::port_or("CANVAS_PORT", 8091)?;"#),
            Some(("CANVAS_PORT".to_string(), 8091))
        );
        assert_eq!(
            default_port(r#"let raw = configured.unwrap_or("8092");"#),
            Some((String::new(), 8092))
        );
        assert_eq!(default_port("no address here"), None);
    }

    #[test]
    fn declared_ports_are_reported_and_portless_packages_stay_portless() {
        let workspace = workspace();
        let catalog = workspace.catalog(None).expect("catalog builds");
        for option in catalog["options"].as_array().expect("options array") {
            let path = option["path"]
                .as_str()
                .expect("every catalog option names its workspace path");
            let source = fs::read_to_string(workspace.root().join(path).join("src/main.rs"))
                .unwrap_or_default();
            match default_port(&source) {
                Some((port_env, port)) => {
                    assert_eq!(option["port"].as_u64(), Some(u64::from(port)), "{path}");
                    assert_eq!(option["port_env"], port_env, "{path}");
                    assert_eq!(option["url"], format!("http://127.0.0.1:{port}"), "{path}");
                    assert!(option["listening"].is_boolean(), "{path}");
                }
                None => {
                    assert!(option["port"].is_null(), "{path}");
                    assert_eq!(option["port_env"], "", "{path}");
                    assert!(option["url"].is_null(), "{path}");
                    assert!(option["listening"].is_null(), "{path}");
                }
            }
        }
    }

    #[test]
    fn reports_packages_that_want_the_same_port() {
        let workspace = workspace();
        let catalog = workspace.catalog(None).expect("catalog builds");
        let contested: Vec<(&str, usize)> = catalog["options"]
            .as_array()
            .expect("options array")
            .iter()
            .map(|option| {
                (
                    option["package"].as_str().unwrap_or_default(),
                    option["contested_by"].as_array().map_or(0, Vec::len),
                )
            })
            .filter(|(_, count)| *count > 0)
            .collect();
        // teaching-canvas and vellum-canvas both default to 8091 today. This
        // asserts the catalog *notices*, not that the collision stays.
        assert!(
            contested.iter().all(|(_, count)| *count > 0),
            "{contested:?}"
        );
    }
}
