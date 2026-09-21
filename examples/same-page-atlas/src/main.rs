//! Same Page Atlas - one CRDT map of a project, two writers.
//!
//! Run it:
//!
//! ```text
//! cargo run -p same-page-atlas                 # http://127.0.0.1:8098
//! ```
//!
//! The browser replica is built automatically at boot when `web/pkg` is
//! missing and wasm-pack is available; otherwise run
//! `examples/same-page-atlas/build-web.sh` first.
//!
//! `AGUI_PROJECT_ROOT` selects the project the atlas is *about* (default: this
//! repository). `AGUI_AGENT_CWD` selects where a managed coding agent runs
//! (default: the same root, so its own file tools see what the surface reads).
//! `AGUI_ATLAS_PROVIDER` opts an in-page provider back in; the default `none`
//! spawns nothing, so the atlas boots with zero providers and every agent
//! arrives over `/mcp`.

mod assertion;
mod atlas;
mod board;
mod cement;
mod import_archify;
mod lanes;
mod map_cement;
mod paper;
mod paper_page;
mod review;
mod session;
mod source;
mod tier;
mod timestamp;

use std::path::PathBuf;
use std::sync::Arc;

use ag_ui_surface::auth::AuthStore;
use ag_ui_surface::config;
use ag_ui_surface::services::ServiceRegistry;
use ag_ui_surface::{App, AppRecipe, CompositeSurface, Hitl, Providers};
use atlas::{AtlasExtension, AtlasState};
use board::{BoardExtension, BoardState};
use same_page_atlas_core as atlas_core;
use sha2::{Digest, Sha256};
use source::Repo;

const SEGMENT_ENCODER_SHA256: &str =
    "d80de6055095e7ba5551e6c15aaa8981b05dca77d6ae77ee82e51e2746a87e3d";
const SEGMENT_DECODER_SHA256: &str =
    "a21b65b6e1b75e2c6265b36835747a0ab9169ec1ed725139a78ce90297f95126";
const ORT_WEBGPU_SHA256: &str = "46988a5a025f49449850f39f95eb0d21e40e67b3beb13a0b54efd3ab5d83f60e";
const ORT_WASM_SHA256: &str = "78feeeb3d08f6bcee94d938ed322f69073bb8076b5f9d34697a574ffba8deb48";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    let dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(dir)
        .parent()
        .and_then(|examples| examples.parent())
        .ok_or("could not locate the workspace root")?
        .to_path_buf();

    let providers = match config::optional_path("AGUI_MODELS")? {
        Some(path) => Providers::from_required_file(path)?,
        None => Providers::from_file(format!("{dir}/models.json"))?,
    };
    let static_dir = config::path_or("AGUI_STATIC_DIR", &format!("{dir}/static"))?;
    let pkg_dir = config::path_or("AGUI_PKG_DIR", &format!("{dir}/web/pkg"))?;
    let model_dir = config::path_or("AGUI_ATLAS_MODEL_DIR", &format!("{dir}/.local/models"))?;
    let ort_dir = config::path_or(
        "AGUI_ATLAS_ORT_DIR",
        &format!("{dir}/node_modules/onnxruntime-web/dist"),
    )?;
    let auth_path = config::path_or("AGUI_AUTH", &format!("{dir}/.local/auth.json"))?;
    let activity_path = config::path_or(
        "AGUI_ACTIVITY_STATE",
        &format!("{dir}/.local/activity.json"),
    )?;
    let state_path = config::path_or("AGUI_ATLAS_STATE", &format!("{dir}/.local/atlas.json"))?;
    let board_path = config::path_or("AGUI_BOARD_STATE", &format!("{dir}/.local/board.json"))?;
    let recipe_path = config::path_or("AGUI_APP_RECIPE", &format!("{dir}/agui.app.toml"))?;
    let prompt_path = config::path_or("AGUI_PROMPT", &format!("{dir}/prompt.md"))?;
    let project_root = config::optional_path("AGUI_PROJECT_ROOT")?.unwrap_or(workspace_root);
    let agent_cwd =
        config::optional_path("AGUI_AGENT_CWD")?.unwrap_or_else(|| project_root.clone());
    let addr = config::string_or("AGUI_ADDR", "127.0.0.1:8098")?;
    let final_response_only = config::bool_or("AGUI_FINAL_RESPONSE_ONLY", || false)?;
    // A repository-relative Excalidraw or Archify JSON diagram to land on the board at boot, so
    // "open this diagram where we can both touch it" is one command instead of
    // start-the-server, attach over `/mcp`, then call `atlas_import` by hand.
    let boot_import = config::optional_nonempty("AGUI_ATLAS_IMPORT")?;

    // Off by default: the agent-ink numeric layer is real and tested, but
    // exposing seven more tools to every agent that attaches is a decision
    // this surface should not make silently. `AGUI_AGENT_INK=1` (or
    // true/on/yes, case-insensitive) opts in for the whole process lifetime;
    // see `AtlasState::agent_ink_enabled` for what it gates.
    // `AGUI_LIVING_INK` is the name this flag had before the rename; it is
    // still honoured so an existing launch recipe keeps working, and the log
    // says which name was read.
    let agent_ink = if config::optional_nonempty("AGUI_AGENT_INK")?.is_some() {
        config::bool_or("AGUI_AGENT_INK", || false)?
    } else {
        let legacy = config::bool_or("AGUI_LIVING_INK", || false)?;
        if legacy {
            tracing::warn!(
                "AGUI_LIVING_INK is the old name for AGUI_AGENT_INK; set AGUI_AGENT_INK instead"
            );
        }
        legacy
    };

    // Terminal-first, same shape as same-page-room: the board opens with no
    // in-page agent, and a real provider id opts one back in. `none` is the
    // models.json placeholder that spawns nothing and answers nothing, so a
    // machine with zero ACP adapters still reaches a usable board.
    let default_provider = resolve_default_provider(std::env::var("AGUI_ATLAS_PROVIDER").ok());

    // The page is a real CRDT peer, not a view, so the web build is not
    // optional: the host refuses to serve a read-only surface that looks
    // collaborative. When the build is missing but the wasm toolchain is
    // present, build it here so a cold checkout still reaches a usable board
    // in one command; when the toolchain is absent, refuse with the exact
    // command rather than a half-built page.
    if !pkg_dir.is_dir() {
        let build_script = PathBuf::from(dir).join("build-web.sh");
        if config::command_available("wasm-pack") {
            tracing::info!(
                "web/pkg is missing and wasm-pack is available; building the browser replica now via {}",
                build_script.display()
            );
            let status = std::process::Command::new(&build_script)
                .status()
                .map_err(|error| format!("could not run {}: {error}", build_script.display()))?;
            if !status.success() {
                return Err(format!(
                    "the automatic web build failed ({} exited with {status}); run it by hand and inspect its output",
                    build_script.display()
                )
                .into());
            }
            if !pkg_dir.is_dir() {
                return Err(format!(
                    "the web build reported success but {} is still missing; run examples/same-page-atlas/build-web.sh by hand",
                    pkg_dir.display()
                )
                .into());
            }
        } else {
            return Err(format!(
                "the browser replica has not been built: {} is missing, and wasm-pack is not installed.\n\
                 Run examples/same-page-atlas/build-web.sh first - without it the page \
                 has no CRDT peer, and a read-only view of a collaborative surface \
                 would be a lie.",
                pkg_dir.display()
            )
            .into());
        }
    }

    // The semantic import is allowed to exist only as the pinned real path.
    // Refuse before binding if the source, models, or browser runtime are
    // absent or have drifted. A different model or a CPU fallback would not
    // be evidence for this learning test.
    for source in atlas_core::SEGMENT_SOURCES {
        let relative = source.url.strip_prefix('/').ok_or_else(|| {
            format!(
                "generated segment source URL {:?} is not absolute",
                source.url
            )
        })?;
        verify_pinned_asset(&static_dir.join(relative), None, source.sha256)?;
    }
    verify_pinned_asset(
        &model_dir.join("mobilesam-vit-t-encoder.onnx"),
        Some(27_996_238),
        SEGMENT_ENCODER_SHA256,
    )?;
    verify_pinned_asset(
        &model_dir.join("mobilesam-vit-t-decoder.onnx"),
        Some(16_496_934),
        SEGMENT_DECODER_SHA256,
    )?;
    verify_pinned_asset(
        &ort_dir.join("ort.webgpu.min.mjs"),
        Some(67_169),
        ORT_WEBGPU_SHA256,
    )?;
    verify_pinned_asset(
        &ort_dir.join("ort-wasm-simd-threaded.jsep.wasm"),
        Some(26_827_543),
        ORT_WASM_SHA256,
    )?;

    // Read at startup, not compiled in, so the standing instructions can be
    // edited and the server restarted without a rebuild. A bounded lesson or
    // evaluation may append its own instructions without copying the shared
    // Same Page operating contract into a second prompt file.
    let base_prompt = std::fs::read_to_string(&prompt_path)
        .map_err(|error| format!("could not read {}: {error}", prompt_path.display()))?;
    let prompt = match config::optional_path("AGUI_PROMPT_APPEND")? {
        Some(path) => {
            let appendix = std::fs::read_to_string(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            compose_prompt(&base_prompt, &appendix)
        }
        None => base_prompt,
    };

    let repo = Arc::new(Repo::open(&project_root)?);
    tracing::info!(root = %repo.root().display(), "atlas is about this project");

    // One host service, declared by the extension that needs it rather than
    // threaded into its constructor. Composition refuses to build if `atlas`
    // asks for `repository` and nothing here provides it, which is what makes
    // the recipe's `services = ["repository"]` line an enforced claim instead
    // of a comment. The canonical repository-evidence service (ADR 0009, order
    // 0) replaces this app-local `Repo` behind the same name.
    let mut services = ServiceRegistry::new();
    services.provide("repository", repo)?;

    // The MCP attach token: pinned by `AGUI_MCP_TOKEN` when an outside
    // terminal agent needs to read it, otherwise generated here and pinned
    // into the process environment so the recipe below and the runtime's
    // `/mcp` route accept the exact same value.
    let (mcp_token, mcp_token_source) = resolve_mcp_token(
        std::env::var("AGUI_MCP_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    );
    if matches!(mcp_token_source, McpTokenSource::Random) {
        std::env::set_var("AGUI_MCP_TOKEN", &mcp_token);
    }

    let recipe = AppRecipe::from_file(&recipe_path)?;
    let app = App::new()
        .try_surface(move |transport| {
            // Two artifacts, one host. The map is what the project is made of;
            // the board is what we are saying about it. They compose rather
            // than each getting their own server, so a terminal agent that
            // attaches sees one tool catalog and a person watching /events
            // sees both halves in the order they happened.
            let atlas = AtlasState::open(transport.clone(), state_path.clone(), agent_ink)?;
            let board = BoardState::open(transport, board_path.clone(), atlas.clone())?;
            let surface = CompositeSurface::from_recipe_with_services(
                &recipe,
                vec![
                    Box::new(AtlasExtension::new(atlas.clone())),
                    Box::new(BoardExtension::new(board)),
                ],
                &services,
            )
            .map_err(|error| error.to_string())?;
            // After composition, because composition is what binds the
            // `repository` service the import reads through. Only into an
            // empty document: a restart with the variable still set must not
            // land the same diagram twice.
            if let Some(path) = &boot_import {
                if atlas.is_empty()? {
                    let summary = atlas.import_file(path, 0.0, 0.0)?;
                    tracing::info!(%summary, "boot import");
                } else {
                    tracing::info!(%path, "atlas already has content; boot import skipped");
                }
            }
            Ok::<_, String>(surface)
        })?
        .providers(providers)
        // Terminal-first: `none` spawns nothing and answers nothing, so a
        // machine with no ACP adapters still opens the board and every agent
        // arrives over `/mcp` under its own name. Set `AGUI_ATLAS_PROVIDER` to
        // a real provider id to opt an in-page agent back in.
        .default_provider(default_provider)
        .auth(Arc::new(AuthStore::open(auth_path)?))
        .activity_store(activity_path)
        .prompt(prompt)
        .final_response_only(final_response_only)
        .static_dir(static_dir)
        .pkg_dir(pkg_dir)
        .mount("/model-assets", model_dir)
        .mount("/ort", ort_dir)
        .agent_cwd(agent_cwd)
        .voice(false)
        .hitl(Hitl {
            barge_in: true,
            decisions: false,
            focus: true,
        });

    // Paste-ready MCP attach recipe. The runtime prints its own "agents attach
    // at" line too; this one states the token source so an operator knows
    // whether the value survives a restart.
    tracing::info!(
        "MCP attach recipe: curl -sS http://{addr}/mcp -H 'Authorization: Bearer {mcp_token}' -H 'MCP-Protocol-Version: 2025-06-18' -H 'Content-Type: application/json' -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-06-18\"}}}}'"
    );
    tracing::info!(
        "Claude MCP wait recipe: export MCP_TOOL_TIMEOUT=240000 before starting Claude Code. The value is milliseconds and keeps await_atlas open for up to four minutes instead of the client cancelling at its shorter default."
    );
    tracing::info!(
        "MCP token: {mcp_token} ({})",
        match mcp_token_source {
            McpTokenSource::Env => "from AGUI_MCP_TOKEN",
            McpTokenSource::Random => "randomly generated at boot; set AGUI_MCP_TOKEN to pin it",
        }
    );

    let _session = session::Session::publish(&project_root, &addr, &mcp_token)?;
    tracing::info!(
        "Same Page agents: run dev/samepage read from this repository, then dev/samepage wait"
    );
    app.serve(&addr).await?;
    Ok(())
}

fn verify_pinned_asset(
    path: &std::path::Path,
    expected_bytes: Option<u64>,
    expected_sha256: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "required semantic-import asset {} cannot be read: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "required semantic-import asset {} is not a regular file",
            path.display()
        )
        .into());
    }
    if let Some(expected_bytes) = expected_bytes {
        if metadata.len() != expected_bytes {
            return Err(format!(
                "required semantic-import asset {} has {} bytes, expected {expected_bytes}",
                path.display(),
                metadata.len()
            )
            .into());
        }
    }
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "required semantic-import asset {} cannot be opened: {error}",
            path.display()
        )
    })?;
    let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
    if actual_sha256 != expected_sha256 {
        return Err(format!(
            "required semantic-import asset {} has sha256 {actual_sha256}, expected {expected_sha256}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn compose_prompt(base: &str, appendix: &str) -> String {
    format!("{}\n\n{}\n", base.trim_end(), appendix.trim())
}

/// Which provider id the atlas starts on when `AGUI_PROVIDER` (or the legacy
/// `CANVAS_PROVIDER`) is not set. `AGUI_ATLAS_PROVIDER` wins, then the `none`
/// placeholder, so a machine with zero ACP adapters still boots to a usable
/// board. An override is returned as-is: the runtime rejects an unknown id at
/// serve time rather than silently substituting a default.
fn resolve_default_provider(env: Option<String>) -> String {
    env.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "none".to_string())
}

/// Where the effective MCP attach token came from, for the boot recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpTokenSource {
    /// `AGUI_MCP_TOKEN` pinned it, so an outside terminal agent can read the
    /// same value from the environment.
    Env,
    /// Generated fresh this process; the recipe prints it because nothing else
    /// could.
    Random,
}

/// Resolve the effective MCP token and its provenance.
///
/// `AGUI_MCP_TOKEN` pins the token (an outside terminal agent needs a value it
/// can read before the process starts). Without it a fresh token is generated,
/// and the caller pins it into the process environment so the runtime's `/mcp`
/// route accepts the exact value the boot recipe prints. The recipe must never
/// advertise a token the route does not actually accept.
fn resolve_mcp_token(env_token: Option<String>) -> (String, McpTokenSource) {
    match env_token {
        Some(token) => (token, McpTokenSource::Env),
        None => (uuid::Uuid::new_v4().to_string(), McpTokenSource::Random),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_provider_is_the_none_preset_unless_overridden() {
        assert_eq!(resolve_default_provider(None), "none");
        assert_eq!(resolve_default_provider(Some(String::new())), "none");
        assert_eq!(resolve_default_provider(Some("   ".to_string())), "none");
        assert_eq!(resolve_default_provider(Some("codex".to_string())), "codex");
        assert_eq!(
            resolve_default_provider(Some("  claude  ".to_string())),
            "claude"
        );
    }

    #[test]
    fn a_lesson_appendix_extends_the_shared_prompt_without_copying_it() {
        let prompt = compose_prompt("shared contract\n", "\nlesson contract\n");
        assert_eq!(prompt, "shared contract\n\nlesson contract\n");
    }

    #[test]
    fn the_epsilon_lesson_uses_direct_answers_and_attention_instead_of_decorative_motion() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let shared = std::fs::read_to_string(root.join("prompt.md"))
            .expect("shared Atlas prompt is readable");
        let lesson =
            std::fs::read_to_string(root.join("tests/epsilon-hypersphere/agent-prompt.md"))
                .expect("epsilon lesson prompt is readable");
        let driver = std::fs::read_to_string(
            root.join("../../dev/atlas-experiment-lib/drivers/same-page-atlas-epsilon-lesson.mjs"),
        )
        .expect("epsilon lesson proof driver is readable");
        let browser = std::fs::read_to_string(root.join("static/extensions/atlas/index.js"))
            .expect("Atlas browser extension is readable");
        let styles = std::fs::read_to_string(root.join("static/styles.css"))
            .expect("Atlas styles are readable");

        assert!(
            shared.contains("learner's utterance together with the current")
                && shared.contains("`WHERE THE HUMAN IS`"),
            "the shared prompt must fuse utterance and current attention before validating wording"
        );
        assert!(
            shared.contains("Follow an active explanation flow")
                && shared.contains("`atlas_explanation_advance`"),
            "the shared prompt must teach from any authored explanation flow"
        );
        assert!(
            lesson.contains("contains no topic-specific lesson sequence")
                && lesson.contains("A `Structured learner interaction` says a")
                && !lesson.contains("YES -> Inside")
                && !lesson.contains("1.58"),
            "the managed lesson appendix must preserve the turn boundary without hiding lesson semantics"
        );
        assert!(
            !driver.contains("epsilonLessonMotion")
                && !driver.contains("atlas_segment_motion")
                && !driver.contains("{ kind: \"replay-motion\""),
            "the epsilon lesson must not use panel motion that only draws attention"
        );
        assert!(
            driver.contains("id: \"epsilon-hypersphere-v1\"")
                && driver.contains("target_ids: [ids.inside]")
                && driver.matches("control: \"button\"").count() == 2,
            "the direct answers must live in flow data over stable segmented targets"
        );
        assert!(
            browser.contains("submitDirectExplanation")
                && browser.contains("agui-direct-interaction-v1")
                && browser.contains("waitForHostExplanationCommit")
                && browser.contains("source: \"evidence-target\""),
            "a direct visual answer must commit before sending structured agent context"
        );
        assert!(
            styles.contains("explanation-attention 760ms ease-out 1")
                && browser.contains("animationend")
                && browser.contains("prefers-reduced-motion: reduce"),
            "attention must be bounded, self-removing, and respect reduced motion"
        );
        assert!(
            !lesson.contains("the only accepted answers"),
            "an exact-label gate must not override an unambiguous pointed answer"
        );
        assert!(
            !lesson.contains("motion stored, not visually verified"),
            "internal proof language must not be prescribed as lesson copy"
        );
    }

    #[test]
    fn learner_mode_is_a_topic_neutral_boundary_from_authoring() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let shell =
            std::fs::read_to_string(root.join("static/shell.js")).expect("Atlas shell is readable");
        let browser = std::fs::read_to_string(root.join("static/extensions/atlas/index.js"))
            .expect("Atlas browser extension is readable");
        let styles = std::fs::read_to_string(root.join("static/styles.css"))
            .expect("Atlas styles are readable");
        let html = std::fs::read_to_string(root.join("static/index.html"))
            .expect("Atlas shell HTML is readable");
        let panes = std::fs::read_to_string(root.join("static/panes.js"))
            .expect("Atlas pane controller is readable");
        let epsilon_driver = std::fs::read_to_string(
            root.join("../../dev/atlas-experiment-lib/drivers/same-page-atlas-epsilon-lesson.mjs"),
        )
        .expect("Epsilon lesson driver is readable");
        let evidence_driver = std::fs::read_to_string(
            root.join("../../dev/atlas-experiment-lib/drivers/same-page-atlas-evidence-loop.mjs"),
        )
        .expect("Evidence Loop driver is readable");

        assert!(
            shell.contains("requestedMode === \"learn\"")
                && shell.contains("if (atlasMode === \"author\") installPaneLayout"),
            "the page shell must make lesson consumption an explicit generic mode"
        );
        assert!(
            browser.contains("const learnerMode = document.body.dataset.atlasMode === \"learn\"")
                && browser.contains("atlasPane.append(explanationPanel, learnerDestination)")
                && browser.contains("source: \"canvas-target\"")
                && browser.contains("learnerResponseForm")
                && browser.contains("A stored motion program is inert lesson data"),
            "learner mode must allocate the lesson outside the canvas and accept both visual and written responses"
        );
        assert!(
            styles.contains("body[data-atlas-mode=\"learn\"] .workspace-dock")
                && styles.contains(".atlas-pane.atlas-learner-mode.has-explanation-flow")
                && styles.contains("grid-row: 3"),
            "learner mode must remove author chrome and own desktop and narrow layouts"
        );
        assert!(
            html.contains("data-dock-tab=\"learn\"")
                && html.contains("id=\"learning-view\"")
                && panes.contains("[\"chat\", \"guide\", \"board\", \"inspector\", \"learn\"]")
                && panes.contains("lastWorkActive"),
            "the ordinary workspace must own learning tangents and remember the work view to restore"
        );
        assert!(
            !browser.to_lowercase().contains("epsilon")
                && !browser.to_lowercase().contains("hypersphere")
                && !browser.to_lowercase().contains("evidence loop"),
            "the browser primitive must not contain lesson-specific branches"
        );
        assert!(
            epsilon_driver.contains("searchParams.set(\"atlas-mode\", \"learn\")")
                && evidence_driver.contains("searchParams.set(\"atlas-mode\", \"learn\")")
                && evidence_driver.contains("the second-topic lesson did not enter learner mode"),
            "two unrelated lessons must exercise the same learner-mode boundary"
        );
    }

    #[test]
    fn learner_navigation_is_local_catalog_driven_and_cross_topic() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let browser = std::fs::read_to_string(root.join("static/extensions/atlas/index.js"))
            .expect("Atlas browser extension is readable");
        let styles = std::fs::read_to_string(root.join("static/styles.css"))
            .expect("Atlas styles are readable");
        let catalog_text = std::fs::read_to_string(root.join("static/learner-catalog.json"))
            .expect("user-facing learner catalog is readable");
        let catalog: serde_json::Value =
            serde_json::from_str(&catalog_text).expect("user-facing learner catalog is valid JSON");
        let topics = catalog["topics"]
            .as_array()
            .expect("learner catalog has topics");
        let proof_text = std::fs::read_to_string(root.join("static/learner-catalog-proof.json"))
            .expect("learner proof catalog is readable");
        let proof: serde_json::Value =
            serde_json::from_str(&proof_text).expect("learner proof catalog is valid JSON");
        let proof_topics = proof["topics"]
            .as_array()
            .expect("learner proof catalog has topics");
        let driver = std::fs::read_to_string(root.join(
            "../../dev/atlas-experiment-lib/drivers/same-page-atlas-learner-navigation-audit.mjs",
        ))
        .expect("learner navigation audit is readable");

        assert_eq!(
            catalog["schema"], "atlas-learner-catalog-v1",
            "the app catalog must pin the generic learner catalog schema"
        );
        assert!(
            !topics.is_empty()
                && proof_topics.len() >= 2
                && topics.len() < proof_topics.len()
                && proof_topics.iter().all(|topic| topic["id"].is_string()
                    && topic["source_id"].is_string()
                    && topic["href"].is_string()),
            "the second-topic hardcoding falsifier must stay out of the user-facing saved list"
        );
        assert!(
            browser.contains("function setLearnerView(next, options = {})")
                && browser.contains("history[method]")
                && browser.contains("renderLearnerTopics")
                && browser.contains("renderLearningWorkspace")
                && browser.contains("learnerWorkspaceUrl")
                && browser.contains("return-to")
                && browser.contains("hideTangent")
                && browser.contains("schema: \"agui-direct-interaction-v1\"")
                && browser.contains("kind: \"request\"")
                && browser.contains("source: \"atlas.learning\"")
                && browser.contains("atlas-learner-catalog-v1"),
            "learning tangents must return to work, be removable, and accept new clarifications generically"
        );
        let local_navigation = browser
            .split("function setLearnerView(next, options = {})")
            .nth(1)
            .and_then(|source| source.split("if (learnerMode) {").next())
            .expect("setLearnerView function body is bounded");
        assert!(
            !local_navigation.contains("doc.control_explanation")
                && !local_navigation.contains("controlExplanation(")
                && !local_navigation.contains("advanceExplanation("),
            "leaving or browsing topics must not mutate the shared explanation cursor"
        );
        assert!(
            styles.contains(".learner-session-screen")
                && styles.contains(".learner-topic-grid")
                && styles.contains(".learning-request-form")
                && styles.contains(".learning-recent-card"),
            "refresher switching and the ordinary workspace learning panel must have explicit layouts"
        );
        assert!(
            driver.contains("workspaceUnchanged: true")
                && driver.contains("removalPersisted: true")
                && driver.contains("initialAtlas.nodes.length === 0")
                && driver.matches("trustedClick").count() >= 5
                && driver.matches("trustedNavigationClick").count() >= 3
                && !driver.to_lowercase().contains("epsilon")
                && !driver.to_lowercase().contains("evidence loop"),
            "the browser audit must prove a true workspace return, removal, and cross-topic navigation without naming either refresher"
        );
    }

    #[test]
    fn the_none_preset_is_declared_in_the_atlas_model_file() {
        // `.default_provider("none")` is rejected at serve time unless a
        // provider with that id is actually in the registry, so the preset in
        // models.json is part of the selection contract, not just a string.
        let models = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/models.json"))
            .expect("atlas models.json is readable");
        let parsed: serde_json::Value =
            serde_json::from_str(&models).expect("atlas models.json is valid JSON");
        let ids: Vec<&str> = parsed["providers"]
            .as_array()
            .expect("models.json has a providers array")
            .iter()
            .filter_map(|row| row.get("id").and_then(serde_json::Value::as_str))
            .collect();
        assert!(
            ids.contains(&"none"),
            "the none preset must exist so it can be the default provider: {ids:?}"
        );
    }

    #[test]
    fn the_mcp_token_source_is_env_when_pinned_and_random_otherwise() {
        let (token, source) = resolve_mcp_token(Some("pinned".to_string()));
        assert_eq!(token, "pinned");
        assert_eq!(source, McpTokenSource::Env);

        let (token, source) = resolve_mcp_token(None);
        assert!(!token.is_empty(), "a random token must be non-empty");
        assert_eq!(source, McpTokenSource::Random);
    }
}
