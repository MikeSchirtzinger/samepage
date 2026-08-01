//! Demo server for the AG-UI shared canvas.
//!
//! Three channels on one port:
//! - `GET /ws`       — binary frames: yrs sync (CRDT scene) + blob payloads
//! - `GET /events`   — SSE relay for validated JSON AG-UI events posted by
//!   external clients (same contract as `ag-ui-wasm-client::subscribe_events`)
//! - `POST /semantic`— validated semantic events (canvas.dragged etc.), echoed
//!   onto /events
//! - `GET /`         — static demo page + wasm pkg
//! - `GET /debug/stats` — counters for E2E verification
//!
//! This binary has no agent-provider integration. It never synthesizes agent
//! events or scene mutations; a real external client must produce them.

use parking_lot::Mutex;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tower_http::services::ServeDir;
use tracing::{info, warn};

use ag_ui_canvas::blob::BlobStore;
use ag_ui_canvas::codec::{decode_frame, encode_blob, encode_sync, BlobHeader, Frame};
use ag_ui_canvas::scene::{Author, PropValue, Scene, SceneError};
use ag_ui_canvas::sync::{authoritative_replay, handle_payload_validated};
use ag_ui_canvas::ObjectId;
use ag_ui_core::event::Event as AgUiEvent;
use ag_ui_core::JsonValue;

struct AppState {
    scene: Mutex<Scene>,
    blobs: Mutex<BlobStore>,
    /// Binary frames fanned out to every WS client.
    ws_tx: broadcast::Sender<Vec<u8>>,
    /// JSON AG-UI event strings fanned out to every SSE client.
    sse_tx: broadcast::Sender<String>,
    semantic_received: AtomicU64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    let (ws_tx, _) = broadcast::channel::<Vec<u8>>(256);
    let (sse_tx, _) = broadcast::channel::<String>(256);

    let mut scene = Scene::new();
    seed_scene(&mut scene)?;

    let state = Arc::new(AppState {
        scene: Mutex::new(scene),
        blobs: Mutex::new(BlobStore::new()),
        ws_tx,
        sse_tx,
        semantic_received: AtomicU64::new(0),
    });

    // Every committed scene transaction relayed from a client goes out to all
    // WS clients as a Sync frame.
    let update_sub = {
        let ws_tx = state.ws_tx.clone();
        let scene = state.scene.lock();
        scene.on_update(move |update| {
            let frame = encode_sync(&ag_ui_canvas::sync::update_message(update));
            let _ = ws_tx.send(frame);
        })?
    };
    // Keep the observer alive for the lifetime of the process.
    std::mem::forget(update_sub);

    warn!(
        "NO AGENT PROVIDER: ag-ui-canvas-server is running as a transport-only \
         shared canvas; this process will not emit agent-generated or synthetic traffic"
    );

    let static_dir = env_string_or(
        "CANVAS_STATIC_DIR",
        concat!(env!("CARGO_MANIFEST_DIR"), "/static"),
    )?;
    validate_static_directory(Path::new(&static_dir))?;
    info!("serving static files from {static_dir}");

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/events", get(sse_handler))
        .route("/semantic", post(semantic_handler))
        .route("/debug/stats", get(stats_handler))
        .fallback_service(ServeDir::new(static_dir))
        .with_state(state);

    let port = env_port_or("CANVAS_PORT", 8090)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn env_string_or(name: &'static str, default: &str) -> Result<String, std::io::Error> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must not be empty"),
        )),
        Err(std::env::VarError::NotPresent) => Ok(default.to_string()),
        Err(std::env::VarError::NotUnicode(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is not valid Unicode"),
        )),
    }
}

/// Fail before binding if the configured static root cannot actually be
/// served. `ServeDir` otherwise accepts a typo or regular file and leaves a
/// healthy-looking listener that answers every asset request with 404.
fn validate_static_directory(path: &Path) -> Result<(), std::io::Error> {
    if path.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "CANVAS_STATIC_DIR must not be empty",
        ));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "CANVAS_STATIC_DIR {} cannot be read: {error}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("CANVAS_STATIC_DIR {} is not a directory", path.display()),
        ));
    }
    for relative in [
        "index.html",
        "pkg/ag_ui_canvas_web.js",
        "pkg/ag_ui_canvas_web_bg.wasm",
    ] {
        let asset = path.join(relative);
        let metadata = std::fs::metadata(&asset).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "required static asset {} cannot be read: {error}",
                    asset.display()
                ),
            )
        })?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "required static asset {} is not a regular file",
                    asset.display()
                ),
            ));
        }
        std::fs::File::open(&asset).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "required static asset {} cannot be opened: {error}",
                    asset.display()
                ),
            )
        })?;
    }
    Ok(())
}

fn env_port_or(name: &'static str, default: u16) -> Result<u16, std::io::Error> {
    match std::env::var(name) {
        Ok(value) => parse_port(name, Some(&value), default),
        Err(std::env::VarError::NotPresent) => parse_port(name, None, default),
        Err(std::env::VarError::NotUnicode(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is not valid Unicode"),
        )),
    }
}

fn parse_port(
    name: &'static str,
    value: Option<&str>,
    default: u16,
) -> Result<u16, std::io::Error> {
    let Some(value) = value else {
        return Ok(default);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must not be empty"),
        ));
    }
    let port = value.parse::<u16>().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid {name} {value:?}: {error}"),
        )
    })?;
    if port == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must be between 1 and 65535"),
        ));
    }
    Ok(port)
}

/// Initial scene: explicit static demo content, not agent output.
fn seed_scene(scene: &mut Scene) -> Result<(), SceneError> {
    let mk = |scene: &mut Scene,
              kind: &str,
              x: f64,
              y: f64,
              scale: f64,
              color: u32|
     -> Result<ObjectId, SceneError> {
        let id = scene.create_object(kind, Author::Named("server-seed".to_string()))?;
        scene.set_props(
            &id,
            &[
                ("x", PropValue::Num(x)),
                ("y", PropValue::Num(y)),
                ("scale", PropValue::Num(scale)),
                ("color", PropValue::Num(color as f64)),
            ],
        )?;
        Ok(id)
    };

    mk(scene, "disc", -160.0, 80.0, 36.0, 0xFF5A66FF)?;
    mk(scene, "disc", 0.0, 120.0, 28.0, 0x52E08CFF)?;
    mk(scene, "square", 160.0, 60.0, 30.0, 0xFFC23DFF)?;
    mk(scene, "square", 90.0, -110.0, 24.0, 0x6FD3FFFF)?;
    Ok(())
}

// ── WebSocket: yrs sync + blobs ─────────────────────────────────────────

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !websocket_origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "untrusted WebSocket Origin").into_response();
    }
    upgrade
        .on_upgrade(move |socket| ws_connection(state, socket))
        .into_response()
}

fn websocket_origin_allowed(headers: &HeaderMap) -> bool {
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return true;
    };
    if origins.next().is_some() {
        return false;
    }
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return false;
    }
    let Some(host) = uri.host() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

async fn ws_connection(state: Arc<AppState>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let mut frames = state.ws_tx.subscribe();

    // Handshake: ask what the client has, then push an ordered full scene
    // update before the particle blob. The client can therefore validate the
    // blob against an already-live BlobRef, including on a fresh connection.
    {
        let replay_result = {
            let scene = state.scene.lock();
            let blobs = state.blobs.lock();
            authoritative_replay(&scene, &blobs)
        };
        let replay = match replay_result {
            Ok(frames) => frames,
            Err(error) => {
                warn!(%error, "failed to build authoritative websocket replay");
                let _ = sink.send(WsMessage::Close(None)).await;
                return;
            }
        };
        for frame in replay {
            if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
                return;
            }
        }
    }

    // Drive both halves from one task. Lag means this client missed state and
    // must reconnect for a fresh CRDT handshake; keeping an independent
    // inbound loop alive would leave the transport open but divergent.
    loop {
        tokio::select! {
            biased;
            outbound = frames.recv() => match outbound {
                Ok(frame) => {
                    if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("ws client lagged by {n} frames; closing for full resync");
                    let _ = sink.send(WsMessage::Close(None)).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = sink.send(WsMessage::Close(None)).await;
                    return;
                }
            },
            inbound = stream.next() => {
                let data = match inbound {
                    Some(Ok(WsMessage::Binary(data))) => data,
                    Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
                    Some(Ok(WsMessage::Text(_))) => {
                        warn!("text frame on binary-only state websocket; closing");
                        let _ = sink.send(WsMessage::Close(None)).await;
                        return;
                    }
                };
                // Inbound: client frames → scene / blob store. Any integrity
                // failure closes the socket so reconnect performs a complete
                // authoritative replay instead of preserving divergent state.
                match decode_frame(&data) {
                    Ok(Frame::Sync(payload)) => {
                        let replies = {
                            let scene = state.scene.lock();
                            let blobs = state.blobs.lock();
                            handle_payload_validated(&scene, payload, |candidate| {
                                authoritative_replay(candidate, &blobs)
                                    .map(|_| ())
                                    .map_err(|error| error.to_string())
                            })
                        };
                        match replies {
                            Ok(replies) => {
                                for reply in replies {
                                    let _ = state.ws_tx.send(encode_sync(&reply));
                                }
                            }
                            Err(error) => {
                                warn!(%error, "sync error from client; closing for resync");
                                let _ = sink.send(WsMessage::Close(None)).await;
                                return;
                            }
                        }
                    }
                    Ok(Frame::BlobRequest { blob_id, .. }) => {
                        let frame = {
                            let blobs = state.blobs.lock();
                            blobs
                                .get(blob_id)
                                .ok_or_else(|| format!("requested blob {blob_id} is missing"))
                                .and_then(|entry| {
                                    let element_size = entry.dtype.size();
                                    if !entry.bytes.len().is_multiple_of(element_size) {
                                        return Err(format!(
                                            "requested blob {blob_id} has a misaligned byte length"
                                        ));
                                    }
                                    let element_count = u32::try_from(
                                        entry.bytes.len() / element_size,
                                    )
                                    .map_err(|_| {
                                        format!("requested blob {blob_id} exceeds u32 elements")
                                    })?;
                                    encode_blob(
                                        &BlobHeader {
                                            dtype: entry.dtype,
                                            ndim: 2,
                                            blob_id,
                                            generation: entry.generation,
                                            element_count,
                                            shape: entry.shape,
                                        },
                                        entry.bytes.as_bytes(),
                                    )
                                    .map(|frame| frame.as_bytes().to_vec())
                                    .map_err(|error| error.to_string())
                                })
                        };
                        match frame {
                            Ok(frame) => {
                                if sink.send(WsMessage::Binary(frame.into())).await.is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                warn!(blob_id, %error, "blob request failed; closing for resync");
                                let _ = sink.send(WsMessage::Close(None)).await;
                                return;
                            }
                        }
                    }
                    Ok(Frame::Blob { .. }) => {
                        warn!("client attempted blob write; closing (single-writer)");
                        let _ = sink.send(WsMessage::Close(None)).await;
                        return;
                    }
                    Ok(Frame::BlobAck { .. }) => {}
                    Err(error) => {
                        warn!(%error, "bad frame from client; closing for resync");
                        let _ = sink.send(WsMessage::Close(None)).await;
                        return;
                    }
                }
            }
        }
    }
}

// ── SSE: JSON AG-UI events ──────────────────────────────────────────────

async fn sse_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let rx = state.sse_tx.subscribe();

    // Stay quiet until an external client posts a real typed event. Inventing a
    // run lifecycle event here would make a transport connection look like an
    // agent run.
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
        .scan((), |_, item| async move {
            match item {
                Ok(json) => Some(json),
                Err(error) => {
                    warn!(%error, "SSE client lagged; closing for clean reconnect");
                    None
                }
            }
        })
        .map(|json| Ok::<_, std::convert::Infallible>(SseEvent::default().data(json)));

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Semantic channel ────────────────────────────────────────────────────

async fn semantic_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JsonValue>,
) -> Response {
    // This route feeds the typed SSE event stream. Reject malformed input
    // before it can affect counters or reach subscribers.
    if let Err(e) = serde_json::from_value::<AgUiEvent>(body.clone()) {
        warn!("rejecting semantic POST that is not a typed AG-UI event ({e})");
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "ok": false,
                "error": "body is not a valid AG-UI event"
            })),
        )
            .into_response();
    }
    state.semantic_received.fetch_add(1, Ordering::Relaxed);
    let _ = state.sse_tx.send(body.to_string());
    Json(serde_json::json!({ "ok": true })).into_response()
}

async fn stats_handler(State(state): State<Arc<AppState>>) -> Response {
    let snapshot = match state.scene.lock().snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response();
        }
    };
    let objects: Vec<JsonValue> = snapshot
        .into_iter()
        .map(|o| {
            serde_json::json!({
                "id": o.id.as_str(),
                "kind": o.kind,
                "owner": o.owner,
                "x": o.x,
                "y": o.y,
                "color": format!("{:08x}", o.color),
            })
        })
        .collect();
    Json(serde_json::json!({
        "semantic_received": state.semantic_received.load(Ordering::Relaxed),
        "ws_subscribers": state.ws_tx.receiver_count(),
        "sse_subscribers": state.sse_tx.receiver_count(),
        "objects": objects,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_parser_rejects_present_invalid_values() {
        assert_eq!(parse_port("PORT", None, 8090).expect("default"), 8090);
        assert_eq!(
            parse_port("PORT", Some(" 8091 "), 8090).expect("valid port"),
            8091
        );
        for value in ["", "   ", "0", "not-a-port", "65536", "-1"] {
            assert!(
                parse_port("PORT", Some(value), 8090).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn static_directory_preflight_rejects_missing_and_non_directory_roots() {
        let root = std::env::temp_dir().join(format!(
            "ag-ui-canvas-server-static-preflight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        let static_dir = root.join("static");
        let static_file = root.join("index.html");
        let missing = root.join("missing");
        std::fs::create_dir_all(&static_dir).expect("create static directory");
        std::fs::create_dir_all(static_dir.join("pkg")).expect("create package directory");
        std::fs::write(&static_file, b"not a directory").expect("create static file");

        assert!(validate_static_directory(Path::new("")).is_err());
        assert!(validate_static_directory(&missing).is_err());
        assert!(validate_static_directory(&static_file).is_err());
        assert!(validate_static_directory(&static_dir).is_err());
        std::fs::write(static_dir.join("index.html"), b"<!doctype html>")
            .expect("create static entry");
        std::fs::write(
            static_dir.join("pkg/ag_ui_canvas_web.js"),
            b"export default 1",
        )
        .expect("create package JavaScript");
        std::fs::write(static_dir.join("pkg/ag_ui_canvas_web_bg.wasm"), b"\0asm")
            .expect("create package WebAssembly");
        assert!(validate_static_directory(&static_dir).is_ok());

        std::fs::remove_dir_all(&root).expect("remove static preflight fixture");
    }

    #[test]
    fn seeded_scene_is_explicit_static_content_without_agent_or_blob_state() {
        let mut scene = Scene::new();
        seed_scene(&mut scene).expect("seed static demo scene");
        let snapshot = scene.snapshot().expect("snapshot static demo scene");

        assert_eq!(snapshot.len(), 4);
        assert!(snapshot.iter().all(|object| object.owner == "server-seed"));
        assert!(snapshot.iter().all(|object| object.blob_ref.is_none()));
    }

    #[tokio::test]
    async fn invalid_semantic_event_is_rejected_without_publication() {
        let (ws_tx, _) = broadcast::channel(4);
        let (sse_tx, _) = broadcast::channel(4);
        let mut subscriber = sse_tx.subscribe();
        let state = Arc::new(AppState {
            scene: Mutex::new(Scene::new()),
            blobs: Mutex::new(BlobStore::new()),
            ws_tx,
            sse_tx,
            semantic_received: AtomicU64::new(0),
        });

        let response = semantic_handler(
            State(state.clone()),
            Json(serde_json::json!({ "not": "an AG-UI event" })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(state.semantic_received.load(Ordering::Relaxed), 0);
        assert!(matches!(
            subscriber.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn websocket_origin_gate_accepts_native_and_loopback_clients_only() {
        let mut headers = HeaderMap::new();
        assert!(websocket_origin_allowed(&headers));

        for origin in [
            "http://localhost:8090",
            "https://127.0.0.1",
            "http://[::1]:8090",
        ] {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert!(websocket_origin_allowed(&headers), "rejected {origin}");
        }

        for origin in [
            "https://evil.example",
            "http://localhost.evil.example",
            "file://localhost",
        ] {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert!(!websocket_origin_allowed(&headers), "accepted {origin}");
        }
    }
}
