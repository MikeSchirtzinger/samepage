//! Host-resolved repository reads.
//!
//! Every claim on the atlas can name a file and a line range. The *host* — not
//! the model, and not the browser — resolves that reference and reads the
//! bytes, so a source excerpt on the page is evidence rather than recollection.
//! The same resolver serves the agent's `repo_read` action and the browser's
//! `GET /atlas/source` panel, which is what makes the two of us provably
//! looking at the same text.
//!
//! It is also the example's only filesystem capability, so its refusals are
//! part of the contract: repository-relative paths only, no traversal, no
//! symlink escape, no private or credential-shaped files, UTF-8 text only,
//! bounded output.

use std::path::{Path, PathBuf};

/// Directory names never served, at any depth.
const DENIED_DIRS: &[&str] = &[
    ".git",
    ".local",
    ".claude",
    ".ssh",
    "target",
    "node_modules",
    "pkg",
];

/// Filename substrings never served, however they are spelled in the tree.
const DENIED_FILE_HINTS: &[&str] = &["secret", "credential", "password", "token", "auth.json"];

/// Extensions never served (either binary or credential-shaped).
const DENIED_EXTENSIONS: &[&str] = &[
    "pem", "key", "p12", "pfx", "crt", "der", "wasm", "png", "jpg", "jpeg", "gif", "webp", "ico",
    "pdf", "zip", "gz", "tar", "bin", "so", "dylib", "dll", "mp4", "mov", "woff", "woff2", "ttf",
];

const MAX_FILE_BYTES: u64 = 512 * 1024;
// Large source modules can be paged without expanding the whole-document
// import limit or the bytes returned in any one excerpt.
const MAX_SOURCE_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_EXCERPT_LINES: usize = 400;
const MAX_EXCERPT_CHARS: usize = 24_000;
const MAX_ENTRIES: usize = 240;

#[derive(Debug, Clone)]
pub struct Excerpt {
    pub path: String,
    pub lines: String,
    pub first_line: usize,
    pub total_lines: usize,
    pub text: String,
    /// True when the requested range was clipped by the caps above.
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// One repository root, canonicalized once at startup.
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|error| format!("project root is unreadable: {error}"))?;
        if !root.is_dir() {
            return Err("project root is not a directory".to_string());
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Discover readable Rust entry points from a package manifest. These
    /// locate implementation files; they do not prove the card's claims.
    pub fn entry_points(&self, relative: &str) -> Result<Vec<String>, String> {
        if Path::new(relative).file_name().and_then(|v| v.to_str()) != Some("Cargo.toml") {
            return Ok(Vec::new());
        }
        let manifest: toml::Value = self
            .read_whole(relative)?
            .parse()
            .map_err(|error| format!("could not parse {relative}: {error}"))?;
        let directory = Path::new(relative).parent().unwrap_or(Path::new(""));
        let mut candidates = Vec::new();
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
            candidates.push("src/lib.rs".to_string());
        }
        if manifest
            .get("package")
            .and_then(|v| v.get("autobins"))
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            candidates.push("src/main.rs".to_string());
        }
        for bin in manifest
            .get("bin")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(path) = bin.get("path").and_then(|v| v.as_str()) {
                candidates.push(path.to_string());
            }
        }
        let mut paths = Vec::new();
        for candidate in candidates {
            let path = directory.join(candidate).to_string_lossy().to_string();
            if !paths.contains(&path) && self.read(&path, "1").is_ok() {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    /// Resolve a repository-relative path to a real path inside the root.
    ///
    /// Two independent checks: the textual one rejects the obvious escapes
    /// before touching the filesystem, and the canonicalized-prefix one
    /// rejects a symlink that points out of the tree. Neither alone is enough.
    fn resolve(&self, relative: &str) -> Result<PathBuf, String> {
        let relative = relative.trim().trim_start_matches("./");
        if relative.is_empty() {
            return Ok(self.root.clone());
        }
        if relative.starts_with('/') || relative.starts_with('~') || relative.contains('\\') {
            return Err(format!("{relative:?} must be a repository-relative path"));
        }
        if relative.chars().any(|c| c.is_control()) {
            return Err(format!("{relative:?} contains control characters"));
        }
        for segment in relative.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(format!("{relative:?} must not contain . or .. segments"));
            }
            if DENIED_DIRS.contains(&segment) {
                return Err(format!("{segment:?} is not readable through this surface"));
            }
        }
        let lower = relative.to_ascii_lowercase();
        if DENIED_FILE_HINTS.iter().any(|hint| lower.contains(hint)) {
            return Err(format!(
                "{relative:?} looks credential-shaped and is not readable through this surface"
            ));
        }

        let candidate = self.root.join(relative);
        let canonical = candidate
            .canonicalize()
            .map_err(|_| format!("{relative:?} does not exist in this project"))?;
        if !canonical.starts_with(&self.root) {
            return Err(format!("{relative:?} resolves outside the project"));
        }
        Ok(canonical)
    }

    /// List one directory, directories first.
    pub fn list(&self, relative: &str) -> Result<Vec<Entry>, String> {
        let path = self.resolve(relative)?;
        if !path.is_dir() {
            return Err(format!("{relative:?} is not a directory"));
        }
        let mut entries: Vec<Entry> = std::fs::read_dir(&path)
            .map_err(|error| format!("could not list {relative:?}: {error}"))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let lower = name.to_ascii_lowercase();
                if DENIED_DIRS.contains(&name.as_str())
                    || DENIED_FILE_HINTS.iter().any(|hint| lower.contains(hint))
                {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                Some(Entry { name, is_dir })
            })
            .collect();
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        entries.truncate(MAX_ENTRIES);
        Ok(entries)
    }

    /// A whole text file, through every refusal above and none of the
    /// excerpt clipping.
    ///
    /// An excerpt is for a human to read; this is for something to be *parsed*
    /// — an `.excalidraw` document, for now. Clipping that at 400 lines would
    /// hand the parser truncated JSON, which is a confusing error about syntax
    /// standing in for a plain one about size.
    pub fn read_whole(&self, relative: &str) -> Result<String, String> {
        self.read_text(relative, MAX_FILE_BYTES)
    }

    fn read_text(&self, relative: &str, max_bytes: u64) -> Result<String, String> {
        let path = self.resolve(relative)?;
        if path.is_dir() {
            return Err(format!("{relative:?} is a directory; use repo_list"));
        }
        if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
            let extension = extension.to_ascii_lowercase();
            if DENIED_EXTENSIONS.contains(&extension.as_str()) {
                return Err(format!(
                    "{relative:?} is a {extension} file; only text sources are readable here"
                ));
            }
        }
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("could not stat {relative:?}: {error}"))?;
        if metadata.len() > max_bytes {
            return Err(format!(
                "{relative:?} is {} bytes; larger than this surface will read",
                metadata.len()
            ));
        }
        let body = std::fs::read(&path)
            .map_err(|error| format!("could not read {relative:?}: {error}"))?;
        String::from_utf8(body).map_err(|_| format!("{relative:?} is not UTF-8 text"))
    }

    /// Read a file, optionally narrowed to `N` or `N-M` (1-based, inclusive).
    pub fn read(&self, relative: &str, lines: &str) -> Result<Excerpt, String> {
        let body = self.read_text(relative, MAX_SOURCE_FILE_BYTES)?;
        let all: Vec<&str> = body.lines().collect();
        let (start, end) = parse_range(lines, all.len())?;
        let mut truncated = false;
        let mut end = end;
        if end - start + 1 > MAX_EXCERPT_LINES {
            end = start + MAX_EXCERPT_LINES - 1;
            truncated = true;
        }
        let mut text = all[start - 1..end.min(all.len())].join("\n");
        if text.chars().count() > MAX_EXCERPT_CHARS {
            text = text.chars().take(MAX_EXCERPT_CHARS).collect();
            // End at a complete line when possible, so paging never skips
            // the unseen remainder of a normal source line.
            if let Some(newline) = text.rfind('\n') {
                text.truncate(newline);
            }
            end = start + text.split('\n').count() - 1;
            truncated = true;
        }

        Ok(Excerpt {
            path: relative.trim().trim_start_matches("./").to_string(),
            lines: if start == end {
                format!("{start}")
            } else {
                format!("{start}-{end}")
            },
            first_line: start,
            total_lines: all.len(),
            text,
            truncated,
        })
    }
}

/// `""` → the whole file (capped), `"12"` → one line, `"12-40"` → a range.
/// Ranges past the end of the file are an error, not a silent empty excerpt:
/// a stale line range is exactly the drift this surface exists to catch.
fn parse_range(lines: &str, total: usize) -> Result<(usize, usize), String> {
    let total = total.max(1);
    let lines = lines.trim();
    if lines.is_empty() {
        return Ok((1, total));
    }
    let (start, end) = match lines.split_once('-') {
        Some((start, end)) => (start, end),
        None => (lines, lines),
    };
    let start: usize = start
        .trim()
        .parse()
        .map_err(|_| format!("line range {lines:?} is not N or N-M"))?;
    let end: usize = end
        .trim()
        .parse()
        .map_err(|_| format!("line range {lines:?} is not N or N-M"))?;
    if start == 0 || end < start {
        return Err(format!(
            "line range {lines:?} is not a forward 1-based range"
        ));
    }
    if start > total {
        return Err(format!(
            "line {start} is past the end of the file ({total} lines) — the reference is stale"
        ));
    }
    Ok((start, end.min(total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> Repo {
        // The example's own crate root is a real, stable tree to read.
        Repo::open(env!("CARGO_MANIFEST_DIR")).expect("open repo")
    }

    #[test]
    fn reads_a_line_range() {
        let excerpt = repo().read("Cargo.toml", "1-3").expect("read");
        assert_eq!(excerpt.lines, "1-3");
        assert!(excerpt.text.contains("same-page-atlas"));
        assert_eq!(excerpt.text.lines().count(), 3);
        assert!(excerpt.total_lines > 3);
    }

    #[test]
    fn package_entry_points_are_real_readable_files() {
        assert_eq!(
            repo().entry_points("Cargo.toml").unwrap(),
            vec!["src/main.rs"]
        );
        assert!(repo().entry_points("src/main.rs").unwrap().is_empty());
        assert_eq!(
            repo().entry_points("core/Cargo.toml").unwrap(),
            vec!["core/src/lib.rs"]
        );
        assert!(repo().read("core/src/lib.rs", "1-10").is_ok());
    }

    #[test]
    fn explicit_entry_points_respect_read_boundaries_and_auto_target_flags() {
        let root = std::env::temp_dir().join(format!("atlas-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("custom.rs"), "pub fn run() {}\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname='custom'\nautobins=false\nautolib=false\n[lib]\npath='custom.rs'\n[[bin]]\npath='../outside.rs'\n").unwrap();
        let repo = Repo::open(&root).unwrap();
        assert_eq!(repo.entry_points("Cargo.toml").unwrap(), vec!["custom.rs"]);
        std::fs::write(root.join("Cargo.toml"), "[broken").unwrap();
        assert!(repo.entry_points("Cargo.toml").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_source_pages_do_not_skip_normal_lines() {
        let root =
            std::env::temp_dir().join(format!("atlas-source-pages-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let lines: Vec<_> = (0..6000)
            .map(|n| format!("{n}: {}", "x".repeat(100)))
            .collect();
        std::fs::write(root.join("long.rs"), lines.join("\n")).unwrap();
        let repo = Repo::open(&root).unwrap();
        assert!(repo.read_whole("long.rs").is_err());
        let first = repo.read("long.rs", "").unwrap();
        assert!(first.truncated);
        assert_eq!(first.total_lines, 6000);
        let end: usize = first.lines.split('-').next_back().unwrap().parse().unwrap();
        assert_eq!(first.text, lines[..end].join("\n"));
        let second = repo.read("long.rs", &format!("{}-6000", end + 1)).unwrap();
        assert_eq!(second.first_line, end + 1);
        assert!(second.text.starts_with(&lines[end]));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_to_leave_the_root() {
        let repo = repo();
        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "~/.ssh/id_rsa",
            ".git/config",
            "target/debug",
        ] {
            assert!(repo.read(bad, "").is_err(), "{bad} was not refused");
        }
    }

    #[test]
    fn refuses_credential_shaped_and_binary_files() {
        let repo = repo();
        assert!(repo.read("auth.json", "").is_err());
        assert!(repo.read("static/atlas.png", "").is_err());
    }

    #[test]
    fn a_stale_line_range_is_an_error_not_an_empty_excerpt() {
        let error = repo()
            .read("Cargo.toml", "9000-9100")
            .expect_err("must fail");
        assert!(error.contains("stale"), "{error}");
    }

    #[test]
    fn lists_directories_first() {
        let entries = repo().list("").expect("list");
        assert!(entries.iter().any(|entry| entry.name == "Cargo.toml"));
        assert!(entries
            .iter()
            .any(|entry| entry.is_dir && entry.name == "src"));
        let first_file = entries.iter().position(|entry| !entry.is_dir).unwrap_or(0);
        let last_dir = entries.iter().rposition(|entry| entry.is_dir).unwrap_or(0);
        assert!(last_dir <= first_file || first_file == 0);
    }
}
