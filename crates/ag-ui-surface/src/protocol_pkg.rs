//! Serving the typed protocol client.
//!
//! The browser core (`/_agui/client.js`) does not interpret AG-UI events; it
//! hands every SSE payload to `ag-ui-wasm-client`'s `Protocol`, which is
//! `ag_ui_core::assembly` compiled to wasm32. That package has to come from
//! somewhere, and this module decides where:
//!
//! - [`App::protocol_pkg_dir`](crate::App::protocol_pkg_dir) names a built
//!   package explicitly. It must already contain [`JS_FILE`] and
//!   [`WASM_FILE`]; nothing is built. This is the deployment path.
//! - Left unset, the runtime uses the workspace checkout it was compiled from
//!   (`crates/ag-ui-wasm-client/pkg`) and rebuilds it with `wasm-pack` at
//!   startup when the package is missing or older than the sources it is
//!   built from (`ag-ui-wasm-client` and `ag-ui-core`), the same way
//!   `same-page-atlas` builds its own replica. A package that is already
//!   current costs one stat walk, which matters when several hosts start at
//!   once (the meeting-room tests spawn a handful). A cold build is logged
//!   with its duration. Without `wasm-pack`, a stale package is served with a
//!   warning.
//!
//! In every case the outcome is one of: a directory to serve at
//! [`ROUTE_PREFIX`], or a typed refusal naming what to do. The page never
//! silently falls back to interpreting events in JavaScript.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Where the browser fetches the package from.
pub const ROUTE_PREFIX: &str = "/_agui/protocol";
/// The wasm-bindgen glue module the page imports.
pub const JS_FILE: &str = "ag_ui_wasm_client.js";
/// The compiled protocol client the glue module instantiates.
pub const WASM_FILE: &str = "ag_ui_wasm_client_bg.wasm";
/// The crate `wasm-pack` builds, relative to this crate's manifest directory.
const CLIENT_CRATE_RELATIVE: &str = "../ag-ui-wasm-client";

/// Why the protocol client cannot be served.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolPkgError {
    #[error("protocol package {dir} is missing {missing}; {hint}")]
    Missing {
        dir: PathBuf,
        missing: &'static str,
        hint: String,
    },
    #[error("protocol client crate {crate_dir} is not a checkout (no Cargo.toml); set App::protocol_pkg_dir(...) to a built package")]
    NotACheckout { crate_dir: PathBuf },
    #[error("wasm-pack could not be started for {crate_dir}: {source}")]
    Spawn {
        crate_dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("wasm-pack build of {crate_dir} failed ({status}):\n{stderr_tail}")]
    Build {
        crate_dir: PathBuf,
        status: String,
        stderr_tail: String,
    },
}

/// The decision, separated from the filesystem and the subprocess so it can
/// be tested as a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Serve `dir` as it is.
    Serve { dir: PathBuf, stale_warning: bool },
    /// Run `wasm-pack build` in `crate_dir`, then serve `dir`.
    Build { crate_dir: PathBuf, dir: PathBuf },
    /// Refuse to start.
    Refuse(String),
}

/// Facts the plan is decided on.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// Both [`JS_FILE`] and [`WASM_FILE`] exist in the candidate directory.
    pub built: bool,
    /// The built package is older than a source file it is built from.
    /// Meaningless when `built` is false.
    pub stale: bool,
    /// `wasm-pack` is on `PATH`.
    pub wasm_pack: bool,
    /// The client crate's `Cargo.toml` exists next to this crate's source.
    pub checkout: bool,
}

/// Decide what to do. `explicit` is the builder's directory when set.
pub fn plan(explicit: Option<&Path>, default_dir: &Path, crate_dir: &Path, facts: Facts) -> Plan {
    let hint = format!(
        "build it with `wasm-pack build {} --release --target web --out-dir pkg` or point App::protocol_pkg_dir(...) at a built package",
        crate_dir.display()
    );
    match (
        explicit,
        facts.built,
        facts.stale,
        facts.wasm_pack,
        facts.checkout,
    ) {
        (Some(dir), true, _, _, _) => Plan::Serve {
            dir: dir.to_path_buf(),
            stale_warning: false,
        },
        (Some(dir), false, _, _, _) => Plan::Refuse(format!(
            "App::protocol_pkg_dir({}) does not contain {JS_FILE} and {WASM_FILE}; {hint}",
            dir.display()
        )),
        // Current: nothing to do, whatever tools are around.
        (None, true, false, _, _) => Plan::Serve {
            dir: default_dir.to_path_buf(),
            stale_warning: false,
        },
        // Missing or stale, and it can be built here.
        (None, _, _, true, true) => Plan::Build {
            crate_dir: crate_dir.to_path_buf(),
            dir: default_dir.to_path_buf(),
        },
        // Stale and it cannot be rebuilt: serve, and say so.
        (None, true, true, _, _) => Plan::Serve {
            dir: default_dir.to_path_buf(),
            stale_warning: true,
        },
        (None, false, _, false, _) => Plan::Refuse(format!(
            "the typed protocol client is not built and wasm-pack is not on PATH (`cargo install wasm-pack`); {hint}"
        )),
        (None, false, _, true, false) => Plan::Refuse(format!(
            "the typed protocol client is not built and {} is not a checkout to build it from; {hint}",
            crate_dir.display()
        )),
    }
}

/// The checkout this crate was compiled from, which is where the default
/// package lives and where `wasm-pack` builds it.
pub fn default_layout() -> (PathBuf, PathBuf) {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(CLIENT_CRATE_RELATIVE);
    let dir = crate_dir.join("pkg");
    (crate_dir, dir)
}

fn is_built(dir: &Path) -> bool {
    dir.join(JS_FILE).is_file() && dir.join(WASM_FILE).is_file()
}

/// The newest modification time under `path` (a file or a directory walked
/// without following symlinks), or `None` if nothing readable is there.
fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.is_file() {
        return metadata.modified().ok();
    }
    if !metadata.is_dir() {
        return None;
    }
    std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter_map(|entry| newest_mtime(&entry.path()))
        .max()
}

/// Whether the built package predates any source it is built from: the
/// client crate and `ag-ui-core`, sources and manifests.
pub fn is_stale(dir: &Path, crate_dir: &Path) -> bool {
    let Some(built) = std::fs::metadata(dir.join(WASM_FILE))
        .and_then(|m| m.modified())
        .ok()
    else {
        return true;
    };
    let core_dir = crate_dir.join("../ag-ui-core");
    [
        crate_dir.join("src"),
        crate_dir.join("Cargo.toml"),
        core_dir.join("src"),
        core_dir.join("Cargo.toml"),
    ]
    .iter()
    .filter_map(|path| newest_mtime(path))
    .any(|source| source > built)
}

/// Resolve the directory to serve, building it when the plan says so.
pub async fn resolve(explicit: Option<&Path>) -> Result<PathBuf, ProtocolPkgError> {
    let (crate_dir, default_dir) = default_layout();
    let candidate = explicit.unwrap_or(&default_dir);
    let built = is_built(candidate);
    let facts = Facts {
        built,
        stale: built && is_stale(candidate, &crate_dir),
        wasm_pack: crate::config::command_available("wasm-pack"),
        checkout: crate_dir.join("Cargo.toml").is_file(),
    };
    match plan(explicit, &default_dir, &crate_dir, facts) {
        Plan::Serve { dir, stale_warning } => {
            if stale_warning {
                tracing::warn!(
                    dir = %dir.display(),
                    "serving a protocol client that is older than its sources; wasm-pack is not on PATH so it cannot be rebuilt"
                );
            } else {
                tracing::info!(dir = %dir.display(), "serving the typed protocol client");
            }
            Ok(dir)
        }
        Plan::Build { crate_dir, dir } => {
            build(&crate_dir).await?;
            if !is_built(&dir) {
                return Err(ProtocolPkgError::Missing {
                    dir,
                    missing: "its wasm-pack output after a successful build",
                    hint: "check the wasm-pack `--out-dir`".to_string(),
                });
            }
            tracing::info!(dir = %dir.display(), "serving the typed protocol client");
            Ok(dir)
        }
        Plan::Refuse(reason) => Err(ProtocolPkgError::Missing {
            dir: candidate.to_path_buf(),
            missing: "the typed protocol client",
            hint: reason,
        }),
    }
}

async fn build(crate_dir: &Path) -> Result<(), ProtocolPkgError> {
    if !crate_dir.join("Cargo.toml").is_file() {
        return Err(ProtocolPkgError::NotACheckout {
            crate_dir: crate_dir.to_path_buf(),
        });
    }
    tracing::info!(crate_dir = %crate_dir.display(), "building the typed protocol client with wasm-pack");
    let started = Instant::now();
    let output = tokio::process::Command::new("wasm-pack")
        .args(["build", "--release", "--target", "web", "--out-dir", "pkg"])
        .arg(crate_dir)
        .output()
        .await
        .map_err(|source| ProtocolPkgError::Spawn {
            crate_dir: crate_dir.to_path_buf(),
            source,
        })?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_tail = stderr
            .lines()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ProtocolPkgError::Build {
            crate_dir: crate_dir.to_path_buf(),
            status: output.status.to_string(),
            stderr_tail,
        });
    }
    tracing::info!(
        elapsed_ms = elapsed.as_millis(),
        "typed protocol client built"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn facts(built: bool, wasm_pack: bool, checkout: bool) -> Facts {
        Facts {
            built,
            stale: false,
            wasm_pack,
            checkout,
        }
    }

    fn stale(wasm_pack: bool, checkout: bool) -> Facts {
        Facts {
            built: true,
            stale: true,
            wasm_pack,
            checkout,
        }
    }

    #[test]
    fn an_explicit_directory_is_served_as_is_or_refused_never_built() {
        let explicit = Path::new("/deploy/protocol");
        let default = Path::new("/src/crates/ag-ui-wasm-client/pkg");
        let crate_dir = Path::new("/src/crates/ag-ui-wasm-client");
        assert_eq!(
            plan(Some(explicit), default, crate_dir, facts(true, true, true)),
            Plan::Serve {
                dir: explicit.to_path_buf(),
                stale_warning: false
            }
        );
        match plan(Some(explicit), default, crate_dir, facts(false, true, true)) {
            Plan::Refuse(reason) => assert!(reason.contains("/deploy/protocol")),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_current_default_package_is_served_without_a_build() {
        // Several hosts starting at once (the meeting-room tests spawn a
        // handful) must not each run wasm-pack when nothing changed.
        let default = Path::new("/src/crates/ag-ui-wasm-client/pkg");
        let crate_dir = Path::new("/src/crates/ag-ui-wasm-client");
        for (wasm_pack, checkout) in [(true, true), (false, true), (true, false), (false, false)] {
            assert_eq!(
                plan(None, default, crate_dir, facts(true, wasm_pack, checkout)),
                Plan::Serve {
                    dir: default.to_path_buf(),
                    stale_warning: false
                }
            );
        }
    }

    #[test]
    fn a_missing_or_stale_default_is_rebuilt_when_it_can_be_and_served_stale_when_it_cannot() {
        let default = Path::new("/src/crates/ag-ui-wasm-client/pkg");
        let crate_dir = Path::new("/src/crates/ag-ui-wasm-client");
        let build = Plan::Build {
            crate_dir: crate_dir.to_path_buf(),
            dir: default.to_path_buf(),
        };
        assert_eq!(plan(None, default, crate_dir, stale(true, true)), build);
        assert_eq!(
            plan(None, default, crate_dir, facts(false, true, true)),
            build
        );
        assert_eq!(
            plan(None, default, crate_dir, stale(false, true)),
            Plan::Serve {
                dir: default.to_path_buf(),
                stale_warning: true
            }
        );
        // Stale, wasm-pack present, but not a checkout: serve what is there.
        assert_eq!(
            plan(None, default, crate_dir, stale(true, false)),
            Plan::Serve {
                dir: default.to_path_buf(),
                stale_warning: true
            }
        );
    }

    #[test]
    fn staleness_compares_the_package_against_its_sources() {
        let root = std::env::temp_dir().join(format!(
            "ag-ui-protocol-pkg-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let crate_dir = root.join("crates/ag-ui-wasm-client");
        let core_dir = root.join("crates/ag-ui-core");
        let pkg = crate_dir.join("pkg");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::create_dir_all(core_dir.join("src")).unwrap();
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(crate_dir.join("src/lib.rs"), "//").unwrap();
        std::fs::write(core_dir.join("src/lib.rs"), "//").unwrap();
        assert!(is_stale(&pkg, &crate_dir), "no package at all is stale");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(pkg.join(WASM_FILE), b"\0asm").unwrap();
        assert!(
            !is_stale(&pkg, &crate_dir),
            "a package newer than every source is current"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(core_dir.join("src/assembly.rs"), "// new").unwrap();
        assert!(
            is_stale(&pkg, &crate_dir),
            "a core source newer than the package makes it stale"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nothing_built_and_nothing_to_build_with_is_a_refusal_that_says_how() {
        let default = Path::new("/src/crates/ag-ui-wasm-client/pkg");
        let crate_dir = Path::new("/src/crates/ag-ui-wasm-client");
        match plan(None, default, crate_dir, facts(false, false, true)) {
            Plan::Refuse(reason) => {
                assert!(reason.contains("cargo install wasm-pack"));
                assert!(reason.contains("wasm-pack build"));
            }
            other => panic!("expected refusal, got {other:?}"),
        }
        match plan(None, default, crate_dir, facts(false, true, false)) {
            Plan::Refuse(reason) => assert!(reason.contains("not a checkout")),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_default_layout_points_at_the_sibling_client_crate() {
        let (crate_dir, dir) = default_layout();
        assert!(crate_dir.ends_with("ag-ui-wasm-client"));
        assert_eq!(dir, crate_dir.join("pkg"));
        assert!(crate_dir.join("Cargo.toml").is_file());
    }
}
