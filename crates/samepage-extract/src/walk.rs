//! Gitignore-aware directory walking, with the always-skip rules that apply
//! regardless of what a `.gitignore` says.

use std::fmt;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::Skipped;

/// 2 MiB: files larger than this are skipped outright rather than read in
/// full, since a static source scan has no business reading build output or
/// checked-in binaries that happen to be this large.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Directory names that are always skipped, whether or not `.gitignore`
/// would already exclude them (a fixture tree or a repo without a
/// `.gitignore` entry for `target/` shouldn't get scanned regardless).
const ALWAYS_SKIP_DIRS: [&str; 4] = ["target", "node_modules", ".git", ".local"];

/// Failure modes for [`crate::scan`]. Kept small: this crate does not expect
/// scanning to fail except on real I/O errors or an unusable root.
#[derive(Debug)]
pub enum ScanError {
    NotADirectory(PathBuf),
    Io { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::NotADirectory(p) => write!(f, "not a directory: {}", p.display()),
            ScanError::Io { path, source } => {
                write!(f, "i/o error reading {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScanError::Io { source, .. } => Some(source),
            ScanError::NotADirectory(_) => None,
        }
    }
}

/// One file the walk decided to read, with its path relative to the scan
/// root and its full UTF-8 contents.
pub struct WalkedFile {
    pub rel_path: PathBuf,
    pub contents: String,
}

pub struct Walked {
    pub files: Vec<WalkedFile>,
    pub skipped: Vec<Skipped>,
}

impl Walked {
    /// Looks up a walked file by its path relative to the scan root.
    /// `None` means the walk didn't read that path at all, including
    /// because it was filtered out (too large, binary, under `tests/`, …).
    pub fn get(&self, rel_path: &Path) -> Option<&WalkedFile> {
        self.files.iter().find(|f| f.rel_path == rel_path)
    }
}

pub fn walk(root: &Path) -> Result<Walked, ScanError> {
    if !root.is_dir() {
        return Err(ScanError::NotADirectory(root.to_path_buf()));
    }

    let mut files = Vec::new();
    let mut skipped = Vec::new();

    let mut builder = WalkBuilder::new(root);
    builder.hidden(false).git_ignore(true).git_exclude(true);
    // `ignore` doesn't take a name-based always-skip list directly; filter
    // during the walk instead so a fixture tree without a `.gitignore`
    // still gets `target/`/`node_modules/`/etc. excluded.
    builder.filter_entry(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| !ALWAYS_SKIP_DIRS.contains(&name))
            .unwrap_or(true)
    });

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // unreadable dir entry; nothing to scan there
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Ok(rel_path) = path.strip_prefix(root) else {
            continue;
        };
        let rel_path = rel_path.to_path_buf();

        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(source) => {
                return Err(ScanError::Io {
                    path: rel_path,
                    source,
                });
            }
        };
        if metadata.len() > MAX_FILE_BYTES {
            skipped.push(Skipped {
                path: rel_path,
                reason: format!("too large (>{MAX_FILE_BYTES} bytes)"),
            });
            continue;
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(source) => {
                return Err(ScanError::Io {
                    path: rel_path,
                    source,
                });
            }
        };
        let contents = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                skipped.push(Skipped {
                    path: rel_path,
                    reason: "binary file".to_string(),
                });
                continue;
            }
        };

        if is_under_tests_dir(&rel_path) {
            skipped.push(Skipped {
                path: rel_path,
                reason: "test code".to_string(),
            });
            continue;
        }

        files.push(WalkedFile { rel_path, contents });
    }

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Walked { files, skipped })
}

/// True if any path component is exactly `tests` — a `tests/` integration
/// test directory (Rust's convention, and common enough in JS projects) is
/// treated as test code wholesale, per the crate's skip rule.
fn is_under_tests_dir(rel_path: &Path) -> bool {
    rel_path
        .components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("tests"))
}
