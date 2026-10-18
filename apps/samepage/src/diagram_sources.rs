//! Native source facts. Runs inside the host's pre-publication validation on
//! actions, untrusted batches and replica frames, including lines-only edits.
use samepage_capability::{CapError, Change};
use samepage_doc::host::Verifier;
use samepage_map::prop_str;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

pub struct DiagramSources {
    root: PathBuf,
}
impl DiagramSources {
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
}
impl Verifier for DiagramSources {
    fn verify(&self, change: &Change<'_>) -> Result<Value, CapError> {
        let mut facts = serde_json::Map::new();
        for (id, keys) in change.attempted {
            let Some(record) = change.after.get(id) else {
                continue;
            };
            if record.kind != "node" || !keys.iter().any(|k| k == "path" || k == "lines") {
                continue;
            }
            samepage_diagrams::check_binding(record)?;
            let path = prop_str(record, "path").unwrap_or_default();
            let lines = prop_str(record, "lines").unwrap_or_default();
            if !path.is_empty() {
                self.check(path, lines)?;
                facts.insert(id.clone(), json!({"path":path,"lines":lines}));
            }
        }
        Ok(Value::Object(facts))
    }
}
