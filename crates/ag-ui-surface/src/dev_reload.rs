//! Development auto-reload.
//!
//! The runtime already serves every browser asset with `Cache-Control:
//! no-store`, so a rebuilt wasm bundle or an edited stylesheet lands on the
//! next reload. What was missing was the reload. With
//! [`App::dev_reload`](crate::App::dev_reload) on, the runtime polls the
//! directories it serves and broadcasts one [`EVENT_NAME`] custom event per
//! quiet-settled batch of changes. The shared browser core reacts:
//!
//! - a `.wgsl` change is handed to the page's shader hook
//!   (`window.__aguiShader.reload(url)`), which swaps the pipeline with no
//!   reload and no Rust rebuild;
//! - anything else reloads the page.
//!
//! A reload is cheap here because the page holds no state of its own: a
//! surface's shared state lives on the host (a CRDT replica re-syncs over
//! `/ws`, a room re-reads its panes), and the transcript is replayed on
//! connect. That is the property this leans on instead of patching DOM in
//! place: fast rebuild plus reconnect, and the state comes back.
//!
//! Polling, not inotify/FSEvents: no dependency, one directory walk per
//! tick, and a tick is 500 ms. The walk cost is logged once at startup so a
//! large mount is visible rather than a mystery.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::narration::surface_event;
use crate::runtime_state::RuntimeState;

/// The custom event name broadcast on `/events`.
pub const EVENT_NAME: &str = "surface.dev.changed";
/// How often the served directories are walked.
pub const INTERVAL: Duration = Duration::from_millis(500);
/// Above this many files the watcher refuses rather than spin a core.
const MAX_FILES: usize = 20_000;

/// Which served tree a root is, which decides how the browser reacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// `App::static_dir`, served at `/`.
    Static,
    /// `App::pkg_dir`, served at `/pkg`.
    Pkg,
    /// The typed protocol client, served at `/_agui/protocol`.
    Protocol,
    /// An `App::mount`.
    Mount,
    /// A `.wgsl` file under any root: hot-swapped, never a reload.
    Shader,
}

/// One directory the runtime serves, with the URL prefix it serves it at.
#[derive(Debug, Clone)]
pub struct Root {
    pub prefix: String,
    pub dir: PathBuf,
    pub kind: Kind,
}

/// One changed file, as the browser receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub kind: Kind,
    /// Path relative to its root, `/`-separated.
    pub path: String,
    /// The URL the file is served at.
    pub url: String,
}

/// `file -> modified time` for every file under every root.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Snapshot(BTreeMap<PathBuf, SystemTime>);

impl Snapshot {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Why the watcher did not start.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("dev reload refused: {files} files under the served directories exceeds the {MAX_FILES} cap; narrow App::mount(...) or leave dev_reload off")]
    TooManyFiles { files: usize },
}

/// Walk every root. Symlinks are not followed, hidden entries are skipped.
pub fn scan(roots: &[Root]) -> Result<Snapshot, WatchError> {
    let mut files = BTreeMap::new();
    for root in roots {
        walk(&root.dir, &mut files)?;
    }
    Ok(Snapshot(files))
}

fn walk(dir: &Path, into: &mut BTreeMap<PathBuf, SystemTime>) -> Result<(), WatchError> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // A root that vanished mid-session is reported by the diff as its
        // files disappearing; an unreadable one is simply not watched.
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            walk(&path, into)?;
        } else if file_type.is_file() {
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                into.insert(path, modified);
                if into.len() > MAX_FILES {
                    return Err(WatchError::TooManyFiles { files: into.len() });
                }
            }
        }
    }
    Ok(())
}

/// Every file whose modified time differs, appeared, or disappeared between
/// two snapshots, attributed to the root that serves it.
pub fn diff(roots: &[Root], before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    let mut changes = Vec::new();
    for (path, modified) in &after.0 {
        if before.0.get(path) != Some(modified) {
            changes.extend(attribute(roots, path));
        }
    }
    for path in before.0.keys() {
        if !after.0.contains_key(path) {
            changes.extend(attribute(roots, path));
        }
    }
    changes
}

fn attribute(roots: &[Root], path: &Path) -> Option<Change> {
    // Longest matching root wins, so a mount inside the static dir is
    // reported under the mount's prefix.
    let root = roots
        .iter()
        .filter(|root| path.starts_with(&root.dir))
        .max_by_key(|root| root.dir.as_os_str().len())?;
    let relative = path.strip_prefix(&root.dir).ok()?;
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let kind = if path.extension().is_some_and(|ext| ext == "wgsl") {
        Kind::Shader
    } else {
        root.kind
    };
    let url = format!("{}/{relative}", root.prefix.trim_end_matches('/'));
    Some(Change {
        kind,
        path: relative,
        url,
    })
}

/// Poll `roots` forever, broadcasting each settled batch of changes.
///
/// A batch is emitted only after one quiet tick: a `wasm-pack` build or a
/// `cargo` copy touches several files over a few hundred milliseconds, and
/// reloading the page against a half-written bundle is the failure this
/// avoids.
pub async fn watch(rt: Arc<RuntimeState>, roots: Vec<Root>) {
    let started = Instant::now();
    let mut current = match scan(&roots) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(%error, "dev reload watcher not started");
            return;
        }
    };
    tracing::info!(
        files = current.len(),
        roots = roots.len(),
        walk_ms = started.elapsed().as_millis(),
        "dev reload: watching served directories"
    );
    let mut pending: Vec<Change> = Vec::new();
    loop {
        tokio::time::sleep(INTERVAL).await;
        let next = match scan(&roots) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(%error, "dev reload watcher stopped");
                return;
            }
        };
        let changes = diff(&roots, &current, &next);
        current = next;
        if !changes.is_empty() {
            pending.extend(changes);
            continue;
        }
        if pending.is_empty() {
            continue;
        }
        pending.sort_by(|a, b| a.url.cmp(&b.url));
        pending.dedup();
        tracing::info!(
            changed = ?pending.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            "dev reload: broadcasting"
        );
        let value =
            serde_json::to_value(Batch { changes: &pending }).unwrap_or(serde_json::Value::Null);
        surface_event(&rt, EVENT_NAME, value);
        pending.clear();
    }
}

#[derive(Serialize)]
struct Batch<'a> {
    changes: &'a [Change],
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ag-ui-dev-reload-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_touched_file_is_reported_at_the_url_it_is_served_from() {
        let dir = temp_root("static");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/app.js"), "1").unwrap();
        std::fs::write(dir.join(".hidden"), "1").unwrap();
        let roots = vec![Root {
            prefix: "/".into(),
            dir: dir.clone(),
            kind: Kind::Static,
        }];
        let before = scan(&roots).unwrap();
        assert_eq!(before.len(), 1, "hidden files are not watched");
        assert!(diff(&roots, &before, &before).is_empty());

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("nested/app.js"), "22").unwrap();
        let after = scan(&roots).unwrap();
        assert_eq!(
            diff(&roots, &before, &after),
            vec![Change {
                kind: Kind::Static,
                path: "nested/app.js".into(),
                url: "/nested/app.js".into()
            }]
        );

        std::fs::remove_file(dir.join("nested/app.js")).unwrap();
        let gone = scan(&roots).unwrap();
        assert_eq!(
            diff(&roots, &after, &gone).len(),
            1,
            "a removed file is a change"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_shader_is_its_own_kind_and_a_mount_keeps_its_prefix() {
        let dir = temp_root("mount");
        std::fs::write(dir.join("shader.wgsl"), "@vertex").unwrap();
        std::fs::write(dir.join("board.js"), "1").unwrap();
        let roots = vec![Root {
            prefix: "/extensions/canvas".into(),
            dir: dir.clone(),
            kind: Kind::Mount,
        }];
        let after = scan(&roots).unwrap();
        let changes = diff(&roots, &Snapshot::default(), &after);
        assert_eq!(changes.len(), 2);
        let shader = changes.iter().find(|c| c.path == "shader.wgsl").unwrap();
        assert_eq!(shader.kind, Kind::Shader);
        assert_eq!(shader.url, "/extensions/canvas/shader.wgsl");
        let js = changes.iter().find(|c| c.path == "board.js").unwrap();
        assert_eq!(js.kind, Kind::Mount);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_wire_shape_is_kebab_kinds_under_changes() {
        let value = serde_json::to_value(Batch {
            changes: &[Change {
                kind: Kind::Protocol,
                path: "ag_ui_wasm_client_bg.wasm".into(),
                url: "/_agui/protocol/ag_ui_wasm_client_bg.wasm".into(),
            }],
        })
        .unwrap();
        assert_eq!(value["changes"][0]["kind"], "protocol");
        assert_eq!(
            value["changes"][0]["url"],
            "/_agui/protocol/ag_ui_wasm_client_bg.wasm"
        );
    }
}
