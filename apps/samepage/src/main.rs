mod diagram_sources;
mod host;
mod surface;

use std::path::{Path, PathBuf};
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
    let panes_focus = manifest.panes.is_some();
    advertise(&project_root, &addr)?;
    App::new()
        .hitl(ag_ui_surface::Hitl { focus: panes_focus, ..Default::default() })
        .try_surface(move |transport| surface::SamePageSurface::open(document, manifest, transport, &source_root))?
        .providers(Providers::from_required_file(models)?)
        .default_provider("none")
        .auth(auth)
        .activity_enabled(with_trace)
        .activity_store(activity)
        .people_store(state_dir.join("people.json"))
        .prompt("Work on the shared SamePage map. Read the live map before acting. Use the declared actions and report refusals clearly.")
        .protocol_pkg_dir(protocol)
        .static_dir(static_dir)
        .mount("/capabilities", artifacts)
        .agent_cwd(project_root)
        .serve(&addr)
        .await?;
    Ok(())
}

/// Tell agents working in the project that this host is running: write
/// `.samepage/sessions/<pid>.json` naming the URL, the MCP endpoint, the first
/// tool to call and the wait tool. The advert is removed when the host stops,
/// and adverts this app left behind for stopped hosts are removed at start.
fn advertise(project: &Path, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = project.join(".samepage").join("sessions");
    std::fs::create_dir_all(&sessions)?;
    if let Ok(entries) = std::fs::read_dir(&sessions) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(advert) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if advert["kind"] != "samepage-host" {
                continue;
            }
            let pid = advert["pid"].as_u64().unwrap_or(0);
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !alive {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let path = sessions.join(format!("{}.json", std::process::id()));
    let advert = serde_json::json!({
        "schema_version": 1,
        "kind": "samepage-host",
        "pid": std::process::id(),
        "repository": project.display().to_string(),
        "url": format!("http://{addr}"),
        "mcp": format!("http://{addr}/mcp"),
        "mcp_token": "the AGUI_MCP_TOKEN this host was started with, when one was set",
        "entry_tool": "page_read",
        "wait_tool": "await_input",
        "instructions": surface::AGENT_GUIDE,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&advert)?)?;
    tokio::spawn(async move {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => return,
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        let _ = std::fs::remove_file(&path);
        std::process::exit(0);
    });
    Ok(())
}
