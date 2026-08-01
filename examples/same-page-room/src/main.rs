//! Same Page Room — a place to meet, with nothing about the meeting decided.
//!
//! ```text
//! cargo run -p same-page-room        # http://127.0.0.1:8100
//! ```
//!
//! There is no build step: a pane is data, and the vocabulary that renders it
//! is already compiled in. Changing what the room shows never means restarting
//! the server, and changing how the room *looks* never means editing CSS.
//!
//! `AGUI_PROJECT_ROOT` selects the project the room is about (default: this
//! repository) — it bounds every `source` pane and seeds the runnable catalog.
//! `AGUI_AGENT_CWD` selects where a managed coding agent runs (default: the
//! same root, so the agent's own file tools see what the room reads).

mod catalog;
mod layout;
mod room;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use ag_ui_surface::auth::AuthStore;
use ag_ui_surface::config;
use ag_ui_surface::{App, AppRecipe, CompositeSurface, Hitl, Providers};
use catalog::Workspace;
use room::{RoomExtension, RoomState};

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
    let auth_path = config::path_or("AGUI_AUTH", &format!("{dir}/.local/auth.json"))?;
    let state_path = config::path_or("AGUI_ROOM_STATE", &format!("{dir}/.local/room.json"))?;
    let recipe_path = config::path_or("AGUI_APP_RECIPE", &format!("{dir}/agui.app.toml"))?;
    let prompt_path = config::path_or("AGUI_PROMPT", &format!("{dir}/prompt.md"))?;
    let project_root = config::optional_path("AGUI_PROJECT_ROOT")?.unwrap_or(workspace_root);
    let agent_cwd =
        config::optional_path("AGUI_AGENT_CWD")?.unwrap_or_else(|| project_root.clone());
    let addr = config::string_or("AGUI_ADDR", "127.0.0.1:8100")?;

    // Read at startup rather than compiled in, so the standing instructions can
    // be edited and the server restarted without a rebuild.
    let prompt = std::fs::read_to_string(&prompt_path)
        .map_err(|error| format!("could not read {}: {error}", prompt_path.display()))?;

    let workspace = Arc::new(Workspace::open(&project_root)?);
    tracing::info!(
        root = %workspace.root().display(),
        runnable = workspace.option_count(),
        "the room is about this project"
    );

    // Terminal-first: the room opens on the `none` placeholder — nothing is
    // spawned, and every agent arrives over `/mcp` with its own name. Set
    // `AGUI_ROOM_PROVIDER=codex` (or any provider id) to give the room an
    // in-page agent again. (Codex rather than Claude there: the bundled
    // `claude` ACP agent bills API credits, not a Code subscription, and dies
    // during the setup prime without a credit balance.)
    let default_provider = std::env::var("AGUI_ROOM_PROVIDER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "none".to_string());

    let recipe = AppRecipe::from_file(&recipe_path)?;
    App::new()
        .try_surface(move |transport| {
            let state = RoomState::open(transport, state_path.clone(), workspace.clone())?;
            CompositeSurface::from_recipe(&recipe, vec![Box::new(RoomExtension::new(state))])
                .map_err(|error| error.to_string())
        })?
        .providers(providers)
        .default_provider(default_provider)
        .auth(Arc::new(AuthStore::open(auth_path)?))
        .prompt(prompt)
        .static_dir(static_dir)
        .agent_cwd(agent_cwd)
        .voice(false)
        .hitl(Hitl {
            barge_in: true,
            decisions: false,
            focus: true,
        })
        .serve(&addr)
        .await?;
    Ok(())
}
