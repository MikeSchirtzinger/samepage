mod diagram_sources;
mod host;
mod surface;

use std::path::PathBuf;
use std::sync::Arc;

use ag_ui_surface::auth::AuthStore;
use ag_ui_surface::{App, Providers};
use samepage_capability::Composition;
use samepage_doc::host::Host;
use samepage_map::Map;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if args.len() != 3 || args[0] != "import-legacy" {
            return Err(
                "usage: samepage import-legacy <legacy-base64-state> <new-envelope>".into(),
            );
        }
        let source = PathBuf::from(&args[1]);
        let target = PathBuf::from(&args[2]);
        let composition = Composition::new(vec![samepage_map::descriptor()])?;
        let host = Host::import_legacy(&source, &target, composition, vec![Arc::new(Map)])?;
        let view = host.view();
        let cards = view
            .records()
            .filter(|record| record.kind == "node")
            .count();
        let links = view
            .records()
            .filter(|record| record.kind == "edge")
            .count();
        host.save()?;
        println!(
            "imported {cards} cards and {links} links into {}",
            target.display()
        );
        return Ok(());
    }
    let asset_root = std::env::var_os("SAMEPAGE_APP_ASSETS_DIR")
        .map(PathBuf::from)
        .ok_or("SAMEPAGE_APP_ASSETS_DIR is required")?;
    let static_dir = asset_root.join("web");
    let models = asset_root.join("models.json");
    let protocol = asset_root.join("protocol");
    if !static_dir.is_dir() {
        return Err(format!(
            "SamePage web asset directory is missing: {}",
            static_dir.display()
        )
        .into());
    }
    if !models.is_file() {
        return Err(format!("SamePage models asset is missing: {}", models.display()).into());
    }
    if !protocol.join("ag_ui_wasm_client.js").is_file()
        || !protocol.join("ag_ui_wasm_client_bg.wasm").is_file()
    {
        return Err(format!(
            "SamePage typed protocol package is missing from: {}",
            protocol.display()
        )
        .into());
    }
    let state_dir = std::env::var_os("SAMEPAGE_STATE_DIR")
        .map(PathBuf::from)
        .ok_or("SAMEPAGE_STATE_DIR is required")?;
    let artifacts = std::env::var_os("SAMEPAGE_CAPABILITIES_DIR")
        .map(PathBuf::from)
        .ok_or("SAMEPAGE_CAPABILITIES_DIR is required")?;
    let addr = std::env::var("SAMEPAGE_ADDR").unwrap_or_else(|_| "127.0.0.1:8190".to_string());
    std::fs::create_dir_all(&state_dir)?;
    let manifest = surface::SelectedArtifacts::open(&artifacts)?;
    let with_trace = manifest.trace_enabled();
    let document = state_dir.join("document.json");
    let auth = Arc::new(AuthStore::open(state_dir.join("auth.json"))?);
    let activity = state_dir.join("activity.json");
    let project_root = std::env::var_os("SAMEPAGE_PROJECT_ROOT")
        .map(PathBuf::from)
        .ok_or("SAMEPAGE_PROJECT_ROOT is required")?;
    let source_root = project_root.clone();
    App::new()
        .try_surface(move |transport| surface::SamePageSurface::open(document, manifest, transport, &source_root))?
        .providers(Providers::from_required_file(models)?)
        .default_provider("none")
        .auth(auth)
        .activity_enabled(with_trace)
        .activity_store(activity)
        .prompt("Work on the shared SamePage map. Read the live map before acting. Use the declared actions and report refusals clearly.")
        .protocol_pkg_dir(protocol)
        .static_dir(static_dir)
        .mount("/capabilities", artifacts)
        .agent_cwd(project_root)
        .serve(&addr)
        .await?;
    Ok(())
}
