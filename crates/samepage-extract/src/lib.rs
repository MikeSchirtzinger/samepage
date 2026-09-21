//! `samepage-extract` finds every execution lane a codebase actually has,
//! straight from source, so a diagram of that codebase can't quietly draw
//! one entry point when the code has two.
//!
//! The failure this crate exists to prevent: an agent (or a person) draws a
//! single box and a single arrow for a service that, in the actual source,
//! also binds a second socket, spawns a sidecar process, or kicks off a
//! background task that outlives the request that started it. [`scan`]
//! walks a project tree and returns every [`Lane`] it can find evidence for,
//! where "evidence" always means a file, a line number, and that line's own
//! text — never a description someone gave the crate out of band.
//!
//! This first pass covers Rust and JavaScript/TypeScript, using gitignore
//! aware walking plus regex and light brace-depth analysis (no
//! `tree-sitter`, no real parser). See each [`LaneKind`] variant's doc
//! comment for exactly what it looks for and how confident to be in it, and
//! the crate README for what static extraction cannot see at all.

mod cargo_manifest;
mod js_manifest;
mod rust_extract;
mod util;
mod walk;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use walk::ScanError;

/// The result of scanning one project tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// The root that was scanned, as given to [`scan`].
    pub root: PathBuf,
    /// Every lane found, in the order their files were visited and then by
    /// line within a file. No dedup surprises: the same file:line is never
    /// reported twice, even if two different patterns matched it.
    pub lanes: Vec<Lane>,
    /// How many files were actually read and scanned for patterns (after
    /// gitignore, size, and binary filtering).
    pub scanned_files: usize,
    /// Files or directories the scan chose not to look inside, and why.
    pub skipped: Vec<Skipped>,
    /// Source languages the scan recognized, sorted and deduplicated
    /// (e.g. `["javascript", "rust"]`). Derived from file extensions
    /// actually scanned, not from any manifest.
    pub languages: Vec<String>,
}

/// One file or directory the scanner declined to read, with a short reason
/// a reviewer can act on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    /// Path relative to the scanned root.
    pub path: PathBuf,
    pub reason: String,
}

/// A single execution lane: a place the codebase actually runs code as its
/// own program, listens for connections, spawns a process, calls out over
/// the network, or keeps a task alive in the background.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub kind: LaneKind,
    /// A short human label, e.g. a binary or package name.
    pub label: String,
    pub evidence: Evidence,
    /// The package or module this lane belongs to, when the scan could tell
    /// (a Cargo package name, an npm package name).
    pub package: Option<String>,
    /// A one-line, human-readable explanation of what was matched and why
    /// it counts as this kind of lane.
    pub detail: String,
}

/// What kind of execution lane a [`Lane`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    /// E1: a program the build actually produces — a Cargo `[[bin]]` target
    /// or a package with `src/main.rs` / `src/bin/*.rs`, or on the JS side a
    /// `package.json` `bin` entry, `main`, or `scripts.start`. High
    /// confidence: this is manifest-declared, not inferred from behavior.
    Binary,
    /// E2: a socket bind — `TcpListener::bind`, `UdpSocket::bind`,
    /// `axum::Server::bind`, a `.bind(` call on something listener- or
    /// socket-named, a bare `serve(` call, or the JS equivalents
    /// (`.listen(`, `createServer(`, `Bun.serve(`, `Deno.serve(`). This is
    /// the lane the "sidecar the diagram never drew" failure is most often
    /// about: a second listener nobody mentioned.
    Listener,
    /// E3: a child process — `Command::new`/`.spawn()` on the Rust side,
    /// `child_process` / `spawn`/`exec`/`execFile`/`fork`/`Bun.spawn`/
    /// `Worker` on the JS side.
    Spawn,
    /// E4: an outbound network call this codebase makes to somewhere else —
    /// an HTTP client construction and call, a raw `TcpStream::connect`, a
    /// `fetch(` to a non-relative URL, a `WebSocket(` construction. The
    /// noisiest lane kind in this crate: the `Client::new()` + `.get(`/
    /// `.post(` heuristic gates on a whole file containing both, then flags
    /// every `.get(`/`.post(` in that file, so a file that builds a
    /// `reqwest::Client` anywhere and *also* happens to call
    /// `HashMap::get(` or wire up an axum route with `.post(handler)` will
    /// get a lane for those too — confirmed against this crate's own
    /// self-scan (see the README). Treat a run of `Outbound` lanes in one
    /// file as "look here," not as a verified list of calls.
    Outbound,
    /// E5: a task started once, at or near a program's top level, that
    /// keeps running after the request or event that triggered its start
    /// has finished — `tokio::spawn`/`std::thread::spawn` at a function's
    /// own top level in `main` (or a function `main` calls whose name reads
    /// like an entry point: `serve`, `run`, `start`, `boot`, `worker`,
    /// `loop`, `tick`), or a module-top-level `setInterval(`/`cron`/
    /// `queue.process(` on the JS side. **This is the weakest heuristic in
    /// the crate.** It has no real call graph: "a function `main` calls" is
    /// resolved by a single textual search for `name(` inside `main`'s body,
    /// so an indirectly-invoked entry point (behind a trait object, a
    /// callback registry, a macro) will be missed, and a same-named
    /// function that main does *not* call could in principle be a false
    /// positive. Prefer recall of "background-shaped" spawns over recall of
    /// every spawn in the file; a spawn inside a request handler is meant
    /// to be invisible to this pass.
    Background,
}

/// Where a [`Lane`] came from: the file, the line, and the line's own text,
/// so a reviewer never has to take the crate's word for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Relative to the scanned root.
    pub path: PathBuf,
    /// 1-based line number.
    pub line: u32,
    /// The matched line, trimmed, capped at 200 chars.
    pub snippet: String,
    /// SHA-256 of the whole file this evidence came from, so a reader can
    /// tell whether the file has changed since the report was produced.
    pub sha256: String,
}

/// Scans a project tree and returns every lane it can find evidence for.
///
/// Respects `.gitignore`, and always skips `target/`, `node_modules/`,
/// `.git/`, `.local/`, files over 2 MiB, and files that don't decode as
/// UTF-8 (the crate's binary-file test, since a static scan has no other
/// reliable way to tell source from a build artifact).
pub fn scan(root: &Path) -> Result<Report, ScanError> {
    let walked = walk::walk(root)?;

    let mut lanes = Vec::new();
    let mut languages = std::collections::BTreeSet::new();

    // E1: binaries, resolved from manifests, not from source scanning.
    lanes.extend(cargo_manifest::binary_lanes(root, &walked)?);
    lanes.extend(js_manifest::binary_lanes(root, &walked)?);

    // E2-E5: everything else, resolved line-by-line from source files.
    for file in &walked.files {
        let Some(lang) = source_language(&file.rel_path) else {
            continue;
        };
        languages.insert(lang.to_string());
        match lang {
            "rust" => lanes.extend(rust_extract::extract(root, &file.rel_path, &file.contents)?),
            "javascript" | "typescript" => {
                lanes.extend(js_manifest::extract_js(root, &file.rel_path, &file.contents)?)
            }
            _ => {}
        }
    }

    dedupe_lanes(&mut lanes);
    lanes.sort_by(|a, b| {
        (&a.evidence.path, a.evidence.line).cmp(&(&b.evidence.path, b.evidence.line))
    });

    Ok(Report {
        root: root.to_path_buf(),
        lanes,
        scanned_files: walked.files.len(),
        skipped: walked.skipped,
        languages: languages.into_iter().collect(),
    })
}

/// The same file:line reported by more than one pattern collapses to one
/// lane (first write wins), per the crate's dedup rule.
fn dedupe_lanes(lanes: &mut Vec<Lane>) {
    let mut seen = std::collections::HashSet::new();
    lanes.retain(|lane| seen.insert((lane.evidence.path.clone(), lane.evidence.line)));
}

fn source_language(rel_path: &Path) -> Option<&'static str> {
    match rel_path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Some("rust"),
        Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => Some("javascript"),
        Some("ts") | Some("tsx") => Some("typescript"),
        _ => None,
    }
}
