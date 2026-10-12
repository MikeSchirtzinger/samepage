//! Native repository facts and durable binding baselines. Never linked into WASM.
use samepage_capability::{CapError, Change, View};
use samepage_doc::host::Verifier;
use samepage_sources::{
    binding_key, effective_bindings, string, Binding, BindingFact, Excerpt, Facts,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    sync::Mutex,
};
pub struct Repository {
    pub root: PathBuf,
}
impl Repository {
    pub fn open(root: &Path) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("cannot open project root: {e}"))?;
        if !root.is_dir() {
            return Err("project root must be a directory".into());
        }
        Ok(Self { root })
    }
    pub fn check(&self, relative: &str, lines: &str) -> Result<(), CapError> {
        self.read(relative, lines).map(|_| ())
    }
    /// How many lines a readable project file has. The path passes the same
    /// privacy and containment policy as [`Repository::read`].
    pub fn line_count(&self, relative: &str) -> Result<usize, CapError> {
        self.read(relative, "1")?;
        let resolved = self.root.join(relative);
        let file = fs::File::open(&resolved)
            .map_err(|_| CapError::new("source_invalid", format!("{relative}: cannot read file")))?;
        let mut count = 0;
        let mut reader = BufReader::new(file.take(4 * 1024 * 1024));
        let mut buffer = Vec::new();
        while reader
            .read_until(b'\n', &mut buffer)
            .map_err(|_| CapError::new("source_invalid", format!("{relative}: cannot read file")))?
            > 0
        {
            count += 1;
            buffer.clear();
        }
        Ok(count)
    }
    pub fn read(&self, relative: &str, lines: &str) -> Result<String, CapError> {
        let fail = |message: &str| {
            CapError::new("source_invalid", format!("{relative}:{lines}: {message}"))
        };
        let denied = [
            ".git",
            ".local",
            ".claude",
            ".ssh",
            "target",
            "node_modules",
            "pkg",
        ];
        let path = Path::new(relative);
        if relative.is_empty()
            || path.is_absolute()
            || relative.starts_with('~')
            || relative.contains('\\')
            || relative.chars().any(char::is_control)
            || relative.split('/').any(|p| {
                p.is_empty()
                    || p == "."
                    || p == ".."
                    || denied.contains(&p.to_ascii_lowercase().as_str())
            })
        {
            return Err(fail("must be a readable project-relative path"));
        }
        let lower = relative.to_ascii_lowercase();
        if ["secret", "credential", "password", "token", "auth.json"]
            .iter()
            .any(|s| lower.contains(s))
            || path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".env"))
        {
            return Err(fail("credential-shaped paths are not readable"));
        }
        let resolved = self
            .root
            .join(path)
            .canonicalize()
            .map_err(|_| fail("file does not exist"))?;
        if !resolved.starts_with(&self.root) || !resolved.is_file() {
            return Err(fail("file must stay inside project"));
        }
        // Apply private-name policy to the canonical destination as well.
        let destination = resolved
            .strip_prefix(&self.root)
            .map_err(|_| fail("file escaped project"))?;
        if destination.components().any(|c| {
            let c = c.as_os_str().to_string_lossy().to_ascii_lowercase();
            denied.contains(&c.as_str())
                || c.starts_with(".env")
                || ["secret", "credential", "password", "token", "auth.json"]
                    .iter()
                    .any(|s| c.contains(s))
        }) {
            return Err(fail("resolved path is private"));
        }
        if resolved.extension().is_some_and(|e| {
            [
                "pem", "key", "p12", "pfx", "crt", "der", "wasm", "png", "jpg", "jpeg", "gif",
                "webp", "ico", "pdf", "zip", "gz", "tar", "bin", "so", "dylib", "dll", "mp4",
                "mov", "woff", "woff2", "ttf",
            ]
            .contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
        }) {
            return Err(fail("binary or credential file is not readable"));
        }
        let (start, end) = lines.split_once('-').unwrap_or((lines, lines));
        let start = start
            .parse::<usize>()
            .map_err(|_| fail("lines must be N or N-M"))?;
        let end = end
            .parse::<usize>()
            .map_err(|_| fail("lines must be N or N-M"))?;
        if start == 0 || end < start || end - start >= 400 {
            return Err(fail("line range is reversed or exceeds 400 lines"));
        }
        let file = fs::File::open(&resolved).map_err(|_| fail("cannot read file"))?;
        let mut reader = BufReader::new(file);
        let mut buffer = Vec::new();
        let mut chars = 0;
        let mut excerpt = String::new();
        // Scan only through the requested end. Large modules are supported;
        // memory is bounded independently of the whole file's size.
        for line in 1..=end {
            buffer.clear();
            let read = reader
                .by_ref()
                .take(96_001)
                .read_until(b'\n', &mut buffer)
                .map_err(|_| fail("cannot read file"))?;
            if read == 0 {
                return Err(fail(&format!(
                    "the file has {} lines, so this range ends past it",
                    line - 1
                )));
            }
            if read > 96_000 || buffer.contains(&0) {
                return Err(fail("oversized line or binary source"));
            }
            let text = std::str::from_utf8(&buffer).map_err(|_| fail("file must be UTF-8 text"))?;
            if line >= start {
                excerpt.push_str(text);
                chars += text.chars().count();
                if chars > 24_000 {
                    return Err(fail("source excerpt exceeds 24000 characters"));
                }
            }
        }
        Ok(excerpt)
    }
    pub fn source(&self, path: &str, lines: &str, revision: &str) -> Result<String, CapError> {
        if revision.is_empty() {
            return self.read(path, if lines.is_empty() { "1" } else { lines });
        }
        samepage_sources::range(lines)?;
        let text = self.archived(path, revision)?;
        let (start, end) = lines.split_once('-').unwrap_or((lines, lines));
        let start = start
            .parse::<usize>()
            .map_err(|_| CapError::new("source_invalid", "invalid range"))?;
        let end = end
            .parse::<usize>()
            .map_err(|_| CapError::new("source_invalid", "invalid range"))?;
        let selected: Vec<_> = text
            .split_inclusive('\n')
            .skip(start - 1)
            .take(end - start + 1)
            .collect();
        if selected.len() != end - start + 1 {
            return Err(CapError::new(
                "source_invalid",
                format!(
                    "line range ends past the end at revision {revision}; the reference is stale"
                ),
            ));
        }
        let excerpt = selected.join("");
        if excerpt.chars().count() > 24000 {
            return Err(CapError::new(
                "source_invalid",
                "source excerpt exceeds 24000 characters",
            ));
        }
        Ok(excerpt)
    }
    fn archived(&self, path: &str, revision: &str) -> Result<String, CapError> {
        if revision.len() != 40
            || !revision.bytes().all(|b| b.is_ascii_hexdigit())
            || path.starts_with('/')
            || path.starts_with('~')
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(CapError::new(
                "source_invalid",
                "invalid revision or project-relative path",
            ));
        }
        // Apply the same private-path policy to archived reads, even when a
        // tracked file no longer exists in the working tree.
        let lower = path.to_ascii_lowercase();
        if lower.split('/').any(|p| {
            [
                ".git",
                ".local",
                ".claude",
                ".ssh",
                "target",
                "node_modules",
                "pkg",
            ]
            .contains(&p)
                || p.starts_with(".env")
        }) || ["secret", "credential", "password", "token", "auth.json"]
            .iter()
            .any(|s| lower.contains(s))
            || Path::new(path).extension().is_some_and(|e| {
                [
                    "pem", "key", "p12", "pfx", "crt", "der", "wasm", "png", "jpg", "jpeg", "gif",
                    "webp", "ico", "pdf", "zip", "gz", "tar", "bin", "so", "dylib", "dll", "mp4",
                    "mov", "woff", "woff2", "ttf",
                ]
                .contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
            })
        {
            return Err(CapError::new(
                "source_invalid",
                "private or binary source path",
            ));
        }
        let run = |args: &[&str]| -> Result<Vec<u8>, CapError> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.root)
                .output()
                .map_err(|e| CapError::new("source_invalid", e.to_string()))?;
            if !output.status.success() {
                return Err(CapError::new(
                    "source_invalid",
                    String::from_utf8_lossy(&output.stderr).trim(),
                ));
            }
            Ok(output.stdout)
        };
        let commit = format!("{revision}^{{commit}}");
        run(&["rev-parse", "--verify", &commit]).map_err(|_| {
            CapError::new(
                "source_invalid",
                format!("revision {revision} is not a commit in this repository"),
            )
        })?;
        let tree = run(&["--literal-pathspecs", "ls-tree", "-z", revision, "--", path])?;
        let listing = std::str::from_utf8(&tree)
            .map_err(|_| CapError::new("source_invalid", "invalid Git tree encoding"))?;
        let entries: Vec<_> = listing.split('\0').filter(|s| !s.is_empty()).collect();
        let entry = if entries.len() == 1 {
            entries[0]
        } else {
            return Err(CapError::new(
                "source_invalid",
                "path does not exist as a file at revision",
            ));
        };
        let (header, name) = entry
            .split_once('\t')
            .ok_or_else(|| CapError::new("source_invalid", "invalid Git tree entry"))?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if name != path
            || fields.len() != 3
            || !matches!(fields[0], "100644" | "100755")
            || fields[1] != "blob"
            || fields[2].len() != 40
            || !fields[2].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(CapError::new(
                "source_invalid",
                "revision path must be a regular file",
            ));
        }
        // ls-tree applies the selected project's cwd. The exact blob OID
        // avoids cat-file's repository-root path interpretation.
        let object = fields[2];
        let size = run(&["cat-file", "-s", object])?;
        if String::from_utf8_lossy(&size)
            .trim()
            .parse::<usize>()
            .map_err(|_| CapError::new("source_invalid", "invalid Git blob size"))?
            > 2 * 1024 * 1024
        {
            return Err(CapError::new("source_invalid", "source file exceeds 2 MiB"));
        }
        let bytes = run(&["cat-file", "blob", object])?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| CapError::new("source_invalid", "source is not UTF-8"))?;
        if text.contains('\0') {
            return Err(CapError::new("source_invalid", "binary source"));
        }
        Ok(text.to_string())
    }
    pub fn entry_points(&self, path: &str) -> Result<Vec<String>, CapError> {
        if Path::new(path).file_name().and_then(|v| v.to_str()) != Some("Cargo.toml") {
            return Ok(vec![]);
        }
        let manifest: toml::Value =
            self.excerpt(path, "", "")?.text.parse().map_err(|e| {
                CapError::new("source_invalid", format!("could not parse {path}: {e}"))
            })?;
        let directory = Path::new(path).parent().unwrap_or(Path::new(""));
        let mut candidates = vec![];
        if let Some(lib) = manifest.get("lib") {
            candidates.push(
                lib.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("src/lib.rs")
                    .to_string(),
            );
        } else if manifest
            .get("package")
            .and_then(|v| v.get("autolib"))
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            candidates.push("src/lib.rs".into());
        }
        if manifest
            .get("package")
            .and_then(|v| v.get("autobins"))
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            candidates.push("src/main.rs".into());
        }
        for bin in manifest
            .get("bin")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(p) = bin.get("path").and_then(|v| v.as_str()) {
                candidates.push(p.into());
            }
        }
        let mut paths = vec![];
        for c in candidates {
            let p = directory.join(c).to_string_lossy().to_string();
            if !paths.contains(&p) && self.read(&p, "1").is_ok() {
                paths.push(p);
            }
        }
        Ok(paths)
    }
    pub fn list(&self, relative: &str) -> Result<Value, CapError> {
        if !relative.is_empty()
            && (relative.starts_with('/')
                || relative.starts_with('~')
                || relative.contains('\\')
                || relative.chars().any(char::is_control)
                || relative
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == ".."))
        {
            return Err(CapError::new(
                "source_invalid",
                "list path must stay inside the project",
            ));
        }
        let dir = self
            .root
            .join(relative)
            .canonicalize()
            .map_err(|e| CapError::new("source_invalid", e.to_string()))?;
        if !dir.starts_with(&self.root) || !dir.is_dir() {
            return Err(CapError::new("source_invalid", "directory leaves project"));
        }
        // Validate the canonical directory against the same policy as source reads.
        let components = dir
            .strip_prefix(&self.root)
            .map_err(|e| CapError::new("source_invalid", e.to_string()))?;
        if components
            .components()
            .any(|c| private(&c.as_os_str().to_string_lossy()))
        {
            return Err(CapError::new("source_invalid", "private directory"));
        }
        let mut entries = fs::read_dir(&dir)
            .map_err(|e| CapError::new("source_invalid", e.to_string()))?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if private(&name) {
                    return None;
                }
                let dest = e.path().canonicalize().ok()?;
                if !dest.starts_with(&self.root)
                    || dest
                        .strip_prefix(&self.root)
                        .ok()?
                        .components()
                        .any(|c| private(&c.as_os_str().to_string_lossy()))
                {
                    return None;
                }
                let is_dir = dest.is_dir();
                if !is_dir
                    && self
                        .read(&dest.strip_prefix(&self.root).ok()?.to_string_lossy(), "1")
                        .is_err()
                {
                    return None;
                }
                Some(json!({"name":name,"is_dir":is_dir}))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| {
            b["is_dir"]
                .as_bool()
                .cmp(&a["is_dir"].as_bool())
                .then(a["name"].as_str().cmp(&b["name"].as_str()))
        });
        let truncated = entries.len() > 240;
        entries.truncate(240);
        Ok(json!({"ok":true,"path":relative,"entries":entries,"truncated":truncated}))
    }
    pub fn excerpt(&self, path: &str, lines: &str, revision: &str) -> Result<Excerpt, CapError> {
        let requested = if lines.is_empty() { "1" } else { lines };
        // Verify the same path policy before reading any whole-file metadata.
        let mut text = self.source(path, requested, revision)?;
        let (first_line, mut end) = samepage_sources::range(requested)?;
        let mut truncated = false;
        let body = if revision.is_empty() {
            let file = fs::File::open(self.root.join(path))
                .map_err(|e| CapError::new("source_invalid", e.to_string()))?;
            let mut bytes = Vec::new();
            file.take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| CapError::new("source_invalid", e.to_string()))?;
            if bytes.len() > 2 * 1024 * 1024 || bytes.contains(&0) {
                return Err(CapError::new(
                    "source_invalid",
                    "source exceeds 2 MiB or is binary",
                ));
            }
            String::from_utf8(bytes)
                .map_err(|_| CapError::new("source_invalid", "source is not UTF-8"))?
        } else {
            self.archived(path, revision)?
        };
        let total_lines = body.lines().count();
        if lines.is_empty() {
            end = total_lines.min(400);
            text = body.split_inclusive('\n').take(end).collect();
            if text.chars().count() > 24000 {
                text = text.chars().take(24000).collect();
                if let Some(newline) = text.rfind('\n') {
                    text.truncate(newline + 1);
                }
                end = text.lines().count();
                truncated = true;
            }
            truncated |= end < total_lines;
        }
        let lines = if first_line == end {
            first_line.to_string()
        } else {
            format!("{first_line}-{end}")
        };
        Ok(Excerpt {
            entry_points: vec![],
            entry_points_error: None,
            path: path.into(),
            lines,
            first_line,
            total_lines,
            text,
            truncated,
            revision: revision.into(),
        })
    }
}
fn private(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        ".git",
        ".local",
        ".claude",
        ".ssh",
        "target",
        "node_modules",
        "pkg",
    ]
    .contains(&n.as_str())
        || n.starts_with(".env")
        || ["secret", "credential", "password", "token", "auth.json"]
            .iter()
            .any(|s| n.contains(s))
}
impl Verifier for Repository {
    fn verify(&self, c: &Change<'_>) -> Result<Value, CapError> {
        let mut facts = serde_json::Map::new();
        for (id, keys) in c.attempted {
            let Some(r) = c.after.get(id).filter(|r| r.kind == "node") else {
                continue;
            };
            if !keys
                .iter()
                .any(|k| samepage_sources::KEYS.contains(&k.as_str()))
            {
                continue;
            }
            samepage_sources::check_binding(r)?;
            if !string(r, "path").is_empty() {
                self.check(string(r, "path"), string(r, "lines"))?;
                facts.insert(
                    id.clone(),
                    json!({"path":string(r,"path"),"lines":string(r,"lines")}),
                );
            }
        }
        Ok(Value::Object(facts))
    }
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub binding_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_bindings: BTreeMap<String, Vec<Binding>>,
}
pub struct SourcesHost {
    pub repo: Repository,
    pub registry: Mutex<Registry>,
    pub registry_path: PathBuf,
}
impl SourcesHost {
    pub fn open(project: &Path, state: &Path) -> Result<Self, String> {
        let registry_path = state.with_file_name("sources.json");
        let registry = match fs::read(&registry_path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| format!("sources sidecar is invalid: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self {
            repo: Repository::open(project)?,
            registry: Mutex::new(registry),
            registry_path,
        })
    }
    pub fn save_registry(&self, r: &Registry) -> Result<(), String> {
        let tmp = self.registry_path.with_extension("tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.registry_path).map_err(|e| e.to_string())
    }
    pub fn facts(&self, view: &View) -> Result<Facts, String> {
        let mut locked = self
            .registry
            .lock()
            .map_err(|_| "sources registry lock poisoned")?;
        let mut next = locked.clone();
        let mut facts = Facts {
            extras: next.extra_bindings.clone(),
            ..Default::default()
        };
        // The host supplies extra verified bindings from the optional Lanes
        // owner's durable records. Sources' portable descriptor stays acyclic.
        for r in view.records().filter(|r| r.kind == "binding") {
            let node = string(r, "node");
            let b = Binding {
                path: string(r, "path").into(),
                lines: string(r, "lines").into(),
            };
            let extra = facts.extras.entry(node.into()).or_default();
            if !extra.contains(&b) {
                extra.push(b.clone());
            }
            if !string(r, "baseline_sha256").is_empty() {
                next.binding_hashes
                    .entry(binding_key(node, &b))
                    .or_insert_with(|| string(r, "baseline_sha256").into());
            }
        }
        for n in view.records().filter(|r| r.kind == "node") {
            for b in
                effective_bindings(n, facts.extras.get(&n.id).map(Vec::as_slice).unwrap_or(&[]))
            {
                let key = binding_key(&n.id, &b);
                let fact = match self.repo.read(&b.path, &b.lines) {
                    Ok(text) => {
                        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
                        let baseline = next
                            .binding_hashes
                            .entry(key.clone())
                            .or_insert_with(|| hash.clone())
                            .clone();
                        BindingFact {
                            readable: true,
                            current_sha256: Some(hash),
                            baseline_sha256: Some(baseline),
                            error: None,
                        }
                    }
                    Err(e) => BindingFact {
                        readable: false,
                        current_sha256: None,
                        baseline_sha256: next.binding_hashes.get(&key).cloned(),
                        error: Some(e.to_string()),
                    },
                };
                facts.bindings.insert(key, fact);
            }
        }
        if next.binding_hashes != locked.binding_hashes {
            self.save_registry(&next)?;
            *locked = next;
        }
        Ok(facts)
    }
    pub fn inspect(&self, view: &View) -> Result<Value, String> {
        let facts = self.facts(view)?;
        let projection = samepage_sources::project(view, &facts).map_err(|e| e.to_string())?;
        Ok(json!({"facts":facts,"projection":projection,"revision":view.revision()}))
    }
    pub fn action(&self, name: &str, args: &Value, view: &View) -> Result<Value, String> {
        match name {
            "source_list" => self
                .repo
                .list(args["path"].as_str().unwrap_or(""))
                .map_err(|e| e.to_string()),
            "source_read" => self
                .repo
                .excerpt(
                    args["path"].as_str().unwrap_or(""),
                    args["lines"].as_str().unwrap_or(""),
                    args["revision"].as_str().unwrap_or(""),
                )
                .and_then(|mut e| {
                    if e.revision.is_empty() {
                        match self.repo.entry_points(&e.path) {
                            Ok(paths) => e.entry_points = paths,
                            Err(error) => e.entry_points_error = Some(error.to_string()),
                        }
                    }
                    serde_json::to_value(e)
                        .map_err(|e| CapError::new("source_invalid", e.to_string()))
                })
                .map_err(|e| e.to_string()),
            "sources_read" => self.inspect(view),
            _ => Err("unknown source action".into()),
        }
    }
}
