//! Browser glue for the AG-UI shared canvas.
//!
//! Wires the three channels together in one wasm module:
//! - **WS binary** — yrs sync frames mutate the local
//!   [`Scene`](ag_ui_canvas::Scene) replica; blob
//!   frames go straight to the GPU. Receive path: WS `ArrayBuffer` → ONE
//!   memcpy into an 8-byte-aligned buffer in linear memory → zero-parse
//!   header read → `Queue::write_buffer`.
//! - **SSE JSON** — reuses `ag_ui_wasm_client::subscribe_events` for the
//!   agent-narration log.
//! - **Semantic POST** — drag-end emits ONE `canvas.dragged` AG-UI Custom
//!   event via `ag_ui_wasm_client::post_json` (called as a plain Rust fn so
//!   the linker keeps it).
//!
//! Local edits are optimistic: a drag mutates the local yrs doc, renders next
//! frame, and the update ships to the server in the same tick. The
//! `applying_remote` flag stops remote applies from echoing back out.

// Browser-only crate: native workspace checks compile it as empty.
#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    BinaryType, Element, HtmlCanvasElement, MessageEvent, PointerEvent, WebSocket, WheelEvent,
};

use ag_ui_canvas::blob::AlignedBytes;
use ag_ui_canvas::codec::{decode_frame, encode_sync, Frame};
use ag_ui_canvas::scene::{authorized_blob_color, ObjectSnapshot, PropValue, Scene};
use ag_ui_canvas::sync::{greeting, handle_payload, update_message};
use ag_ui_canvas::{events as canvas_events, ObjectId};
use ag_ui_canvas_render::{Renderer, SceneObject, TextQuad};
use ag_ui_core::event::Event as AgUiEvent;
use ag_ui_core::JsonValue;
use pretext::backend::fixed::FixedWidthBackend;
use pretext::backend::FontSpec;
use pretext::gpu_layout::{GlyphAtlas, GpuTextLayout, TextAlign, TextLayoutConfig, VerticalAlign};

type ReconnectCallback = Rc<dyn Fn()>;
type ReconnectSlot = Rc<RefCell<Option<ReconnectCallback>>>;
type AnimationFrameCallback = Closure<dyn FnMut()>;
type AnimationFrameSlot = Rc<RefCell<Option<AnimationFrameCallback>>>;
type SocketHandlerSlot = Rc<RefCell<Option<SocketHandlers>>>;

/// Own the callbacks installed on one WebSocket. Replacing this value during
/// reconnect clears the old JS properties before dropping their Rust closures,
/// so a flapping server does not leak three callbacks per attempt.
struct SocketHandlers {
    socket: WebSocket,
    _on_open: Closure<dyn FnMut(web_sys::Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(web_sys::Event)>,
}

impl Drop for SocketHandlers {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
    }
}

struct DragState {
    id: ObjectId,
    from: (f64, f64),
    grab: (f32, f32),
}

/// Focus highlight: a near-white ring drawn as a slightly larger instance
/// *behind* the focused object. With painter's order and opaque objects, the
/// object masks the ring's center, leaving a halo — no shader/depth changes.
/// Packed 0xRRGGBBAA (near-white reads on any object color).
const FOCUS_RING_COLOR: u32 = 0xF4_FA_FF_FF;

/// How long the cursor must rest on an object before we report a dwell (ms).
/// Clicks are instant + explicit; dwell is the ambient "pointing while I talk"
/// signal, gated so idle cursor rests don't spam the channel.
const DWELL_MS: f64 = 450.0;

/// Fire-and-forget a semantic event to the server's `/semantic` channel.
fn post_semantic(url: String, event: AgUiEvent<JsonValue>) {
    let Ok(json) = serde_json::to_string(&event) else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        let Ok(body) = js_sys::JSON::parse(&json) else {
            return;
        };
        if let Err(e) = ag_ui_wasm_client::post_json(&url, body).await {
            web_sys::console::warn_1(&e);
        }
    });
}

#[derive(Default)]
struct Stats {
    frames: u32,
    fps: u32,
    msgs: u32,
    bytes: u64,
    msgs_per_s: u32,
    kib_per_s: f64,
    last_rate_ms: f64,
    apply_us: f64,
    upload_us: f64,
}

struct App {
    scene: Scene,
    renderer: Renderer,
    /// Validated text configuration shared by every label refresh. Keeping the
    /// parsed font here makes an invalid built-in specification a startup
    /// error instead of a per-frame fallback.
    text_atlas: GlyphAtlas,
    text_font: FontSpec,
    /// Sorted snapshot mirroring the renderer's instance list; also the
    /// pick list (world coords).
    objects: Vec<ObjectSnapshot>,
    /// The live socket. A cell (not a bare `WebSocket`) so the reconnect path
    /// can swap in a fresh socket after the server drops without the rest of
    /// the app holding a stale handle.
    socket: Rc<RefCell<WebSocket>>,
    stats: Stats,
    drag: Option<DragState>,
    pan_last: Option<(f32, f32)>,
    /// Object under the cursor right now (hover), used to drive the highlight
    /// ring and the dwell timer. Distinct from `focus_id`: a drag/click pins
    /// focus even as the cursor keeps moving.
    hover_id: Option<ObjectId>,
    /// Object to draw a highlight ring behind (the learner's visible focus).
    focus_id: Option<ObjectId>,
    /// When the current hover began (ms), for the dwell threshold.
    hover_since: f64,
    /// Whether the current hover already emitted its one dwell event.
    dwell_sent: bool,
    semantic_url: String,
    hud: Option<Element>,
    dpr: f64,
    /// Once the user pans/zooms/drags, we stop auto-framing so their view
    /// sticks. Reset when the scene empties (a new lesson clears the canvas).
    user_camera: bool,
    /// The world rect we've auto-framed so far this lesson. We only ever grow
    /// it (zoom out to keep new content visible), never fidget inward.
    framed: Option<(f32, f32, f32, f32)>,
    /// Owns the EventSource callback. Dropping this would close narration SSE,
    /// so the live app retains it for the full canvas lifecycle.
    event_subscription: Option<ag_ui_wasm_client::Subscription>,
}

impl App {
    /// Rebuild the renderer's instance list from the CRDT scene. `particles`
    /// objects render via their blob, and `label` objects render as text — both
    /// are excluded from the disc/square instance list.
    fn refresh_objects(&mut self) -> Result<(), String> {
        let objects = self
            .scene
            .snapshot()
            .map_err(|error| format!("canvas state read failed: {error}"))?;
        self.objects = objects;
        // Blob payloads are a side channel, but scene BlobRefs are the
        // authority for their lifetime and uniform color. Retaining from the
        // snapshot makes a CRDT clear/drop remove stale GPU clouds, including
        // after reconnect when there is no separate blob tombstone frame.
        let active_clouds: Vec<(u64, u32)> = self
            .objects
            .iter()
            .filter(|object| object.kind == "particles")
            .filter_map(|object| object.blob_ref.map(|blob| (blob.blob_id, object.color)))
            .collect();
        let active_blob_ids: Vec<u64> = active_clouds.iter().map(|(id, _)| *id).collect();
        self.renderer.retain_blobs(&active_blob_ids);
        for (blob_id, color) in active_clouds {
            self.renderer.set_blob_color(blob_id, color);
        }
        // Drop a stale focus/hover whose object vanished (e.g. a clear()).
        let stale = self
            .focus_id
            .as_ref()
            .is_some_and(|fid| !self.objects.iter().any(|o| o.id == *fid));
        if stale {
            self.focus_id = None;
            self.hover_id = None;
        }
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| !self.objects.iter().any(|object| object.id == drag.id))
        {
            self.drag = None;
        }
        self.rebuild_instances();
        let labels = self.refresh_labels().map_err(|error| error.to_string());
        self.autofit();
        labels
    }

    /// Build the renderer's instance list from the current snapshot, inserting a
    /// highlight ring behind the focused object. Split out from `refresh_objects`
    /// so a hover change can restyle without re-snapshotting the scene or
    /// re-running autofit — keeping hover cheap and jitter-free.
    fn rebuild_instances(&mut self) {
        let focus = self.focus_id.as_ref();
        let mut instances: Vec<SceneObject> = Vec::with_capacity(self.objects.len() + 1);
        for o in self
            .objects
            .iter()
            .filter(|o| o.kind != "particles" && o.kind != "label")
        {
            let kind = match o.kind.as_str() {
                "square" => 1u32,
                "line" => 2,
                _ => 0,
            };
            // A ring behind the focused disc/square (lines aren't pickable).
            if kind != 2 && focus == Some(&o.id) {
                let ring = o.scale + (o.scale * 0.30).max(0.15);
                instances.push(SceneObject {
                    x: o.x,
                    y: o.y,
                    x2: o.x,
                    y2: o.y,
                    scale: ring,
                    color: FOCUS_RING_COLOR,
                    kind,
                });
            }
            instances.push(SceneObject {
                x: o.x,
                y: o.y,
                x2: o.x2,
                y2: o.y2,
                scale: o.scale,
                color: o.color,
                kind,
            });
        }
        self.renderer.set_objects(&instances);
    }

    /// Keep the whole drawing in view until the learner takes the camera. We
    /// only ever zoom OUT to include new content (union of bounds), so the
    /// picture stays framed as the agent builds it up without the camera
    /// fidgeting inward on every added dot. A `clear` (scene → empty) resets
    /// this so the next lesson re-frames from scratch.
    fn autofit(&mut self) {
        if self.objects.is_empty() {
            self.framed = None;
            self.user_camera = false; // a fresh lesson reclaims auto-framing
            return;
        }
        if self.user_camera {
            return;
        }
        let Some(bounds) = self.content_bounds() else {
            return;
        };
        let target = match self.framed {
            Some(p) => (
                p.0.min(bounds.0),
                p.1.min(bounds.1),
                p.2.max(bounds.2),
                p.3.max(bounds.3),
            ),
            None => bounds,
        };
        // Only re-frame when content grew past what we've already framed.
        if self.framed != Some(target) {
            self.renderer
                .frame_bounds(target.0, target.1, target.2, target.3);
            self.framed = Some(target);
        }
    }

    /// World-space bounding box over the drawable objects (discs/squares by
    /// their radius, lines by both endpoints, labels by their anchor). `None`
    /// when nothing positionable is present.
    fn content_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        let mut any = false;
        for o in &self.objects {
            if o.kind == "particles" {
                continue;
            }
            let r = if o.kind == "label" {
                0.0
            } else {
                o.scale.max(0.0)
            };
            let mut acc = |x: f32, y: f32| {
                min_x = min_x.min(x - r);
                min_y = min_y.min(y - r);
                max_x = max_x.max(x + r);
                max_y = max_y.max(y + r);
            };
            acc(o.x, o.y);
            if o.kind == "line" {
                acc(o.x2, o.y2);
            }
            any = true;
        }
        any.then_some((min_x, min_y, max_x, max_y))
    }

    /// Rebuild the renderer's text glyphs from `label` objects, running each
    /// label's string through `pretext`'s layout engine (line-breaking +
    /// per-glyph placement) and mapping the result onto world-anchored
    /// [`TextQuad`]s the atlas pipeline draws.
    fn refresh_labels(&mut self) -> pretext::Result<()> {
        let layout = GpuTextLayout::new(&self.text_atlas);
        let backend = FixedWidthBackend::new();
        // Uniform advance, matching the monospace atlas and the backend's
        // line-break width so wrapping and placement agree.
        let advance = |_c: char| 0.6_f32;
        let config = TextLayoutConfig {
            align: TextAlign::Center,
            vertical_align: VerticalAlign::Center,
            glyph_advance: 0.6,
            line_spacing: 1.35,
            max_width: None,
            ..TextLayoutConfig::default()
        };

        let mut quads: Vec<TextQuad> = Vec::new();
        for o in self.objects.iter().filter(|o| o.kind == "label") {
            let Some(text) = o.text.as_deref() else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let [r, g, b, a] = o.color.to_be_bytes();
            let color = [
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                a as f32 / 255.0,
            ];
            let glyphs = layout.layout_label_proportional(
                text,
                [o.x, o.y, 0.0],
                o.scale,
                color,
                &config,
                &backend,
                &self.text_font,
                advance,
            )?;
            quads.extend(glyphs.iter().map(|g| TextQuad {
                world_pos: [g.world_pos[0], g.world_pos[1]],
                size: g.font_size,
                offset: g.glyph_offset,
                uv_min: g.atlas_uv_min,
                uv_max: g.atlas_uv_max,
                color: g.color,
            }));
        }
        self.renderer.set_text_quads(&quads);
        Ok(())
    }

    /// Topmost object under a world point. Discs hit on radius, squares on
    /// their AABB. Snapshot is draw-ordered, so scan back-to-front.
    fn pick(&self, wx: f32, wy: f32) -> Option<&ObjectSnapshot> {
        self.objects.iter().rev().find(|o| {
            if o.kind == "particles" || o.kind == "label" || o.kind == "line" {
                return false;
            }
            let (dx, dy) = (wx - o.x, wy - o.y);
            if o.kind == "square" {
                dx.abs() <= o.scale && dy.abs() <= o.scale
            } else {
                dx * dx + dy * dy <= o.scale * o.scale
            }
        })
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// Report a label-layout failure at event-callback boundaries, where the DOM
/// APIs require `FnMut(())` and cannot carry a Rust `Result` back to `start`.
fn report_refresh_error(error: &str) {
    web_sys::console::error_1(&JsValue::from_str(&format!(
        "[canvas] scene refresh failed; rendered state was not replaced: {error}"
    )));
}

fn browser_window() -> Result<web_sys::Window, JsValue> {
    web_sys::window().ok_or_else(|| JsValue::from_str("browser window is unavailable"))
}

fn browser_document(window: &web_sys::Window) -> Result<web_sys::Document, JsValue> {
    window
        .document()
        .ok_or_else(|| JsValue::from_str("browser document is unavailable"))
}

/// Boot the canvas app. Resolves once the GPU surface and channels are wired;
/// rendering and networking continue via forgotten closures.
#[wasm_bindgen]
pub async fn start(
    canvas_id: String,
    ws_url: String,
    sse_url: String,
    semantic_url: String,
) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let window = browser_window()?;
    let document = browser_document(&window)?;

    let canvas: HtmlCanvasElement = document
        .get_element_by_id(&canvas_id)
        .ok_or_else(|| JsValue::from_str("canvas element not found"))?
        .dyn_into()?;

    let dpr = window.device_pixel_ratio();
    let width = (canvas.client_width() as f64 * dpr) as u32;
    let height = (canvas.client_height() as f64 * dpr) as u32;
    canvas.set_width(width.max(1));
    canvas.set_height(height.max(1));

    let instance = ag_ui_canvas_render::default_instance();
    let renderer = Renderer::new_with_surface(
        &instance,
        ag_ui_canvas_render::wgpu::SurfaceTarget::Canvas(canvas.clone()),
        width,
        height,
    )
    .await
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let text_font = FontSpec::new("16px monospace")
        .map_err(|e| JsValue::from_str(&format!("invalid canvas label font: {e}")))?;

    let scene = Scene::new();
    let ws = WebSocket::new(&ws_url)?;
    ws.set_binary_type(BinaryType::Arraybuffer);
    // The live socket lives in a cell so the reconnect path can swap a fresh
    // one in without the rest of the app holding a stale handle.
    let socket: Rc<RefCell<WebSocket>> = Rc::new(RefCell::new(ws));

    let app = Rc::new(RefCell::new(App {
        scene,
        renderer,
        text_atlas: GlyphAtlas::default_ascii(),
        text_font,
        objects: Vec::new(),
        socket: socket.clone(),
        stats: Stats::default(),
        drag: None,
        pan_last: None,
        hover_id: None,
        focus_id: None,
        hover_since: 0.0,
        dwell_sent: true, // nothing hovered yet
        semantic_url,
        hud: document.get_element_by_id("hud"),
        dpr,
        user_camera: false,
        framed: None,
        event_subscription: None,
    }));

    // ── outbound: local scene commits → WS Sync frames ──────────────────
    // Self-contained closure: touches only the flag and the live socket, never
    // App — it fires synchronously inside scene mutations while App may be
    // mutably borrowed. Reads the socket from the cell so it always uses the
    // current connection after a reconnect.
    let applying_remote = Rc::new(Cell::new(false));
    {
        let applying = applying_remote.clone();
        let socket = socket.clone();
        let sub = app
            .borrow()
            .scene
            .on_update(move |update| {
                if applying.get() {
                    return;
                }
                let s = socket.borrow();
                if s.ready_state() != WebSocket::OPEN {
                    return;
                }
                let frame = encode_sync(&update_message(update));
                if let Err(e) = s.send_with_u8_array(&frame) {
                    web_sys::console::warn_1(&e);
                    let _ = s.close();
                }
            })
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        std::mem::forget(sub);
    }

    // ── WS lifecycle + auto-reconnect ────────────────────────────────────
    // Wire open/message/close on the socket, and on close swap in a fresh
    // socket and re-wire after a short backoff. The SSE caption channel
    // already auto-reconnects; this gives the pixel channel the same
    // resilience, so a host restart heals without a manual page reload.
    wire_and_reconnect(
        app.clone(),
        socket.clone(),
        applying_remote.clone(),
        ws_url.clone(),
    );

    // ── SSE narration log (reuses the existing wasm client) ─────────────
    {
        let log_el = document.get_element_by_id("events");
        let cb = Closure::wrap(Box::new(move |evt: JsValue| {
            let Some(el) = log_el.as_ref() else { return };
            let line = describe_event(&evt);
            let old = el.text_content().unwrap_or_default();
            let trimmed: Vec<&str> = old.lines().take(9).collect();
            el.set_text_content(Some(&format!("{line}\n{}", trimmed.join("\n"))));
        }) as Box<dyn FnMut(JsValue)>);
        let func: js_sys::Function = cb.into_js_value().unchecked_into();
        let subscription = ag_ui_wasm_client::subscribe_events(&sse_url, func)?;
        app.borrow_mut().event_subscription = Some(subscription);
    }

    // ── pointer interaction ──────────────────────────────────────────────
    {
        let app = app.clone();
        let on_down = Closure::wrap(Box::new(move |e: PointerEvent| {
            let mut app = app.borrow_mut();
            let (sx, sy) = (
                e.offset_x() as f32 * app.dpr as f32,
                e.offset_y() as f32 * app.dpr as f32,
            );
            let (wx, wy) = app.renderer.screen_to_world(sx, sy);
            if let Some(hit) = app.pick(wx, wy) {
                let id = hit.id.clone();
                let grab = (wx - hit.x, wy - hit.y);
                let from = (hit.x as f64, hit.y as f64);
                app.drag = Some(DragState {
                    id: id.clone(),
                    from,
                    grab,
                });
                app.focus_id = Some(id);
                app.hover_id = None; // pointer down: hover resumes on release
                app.rebuild_instances();
            } else {
                app.pan_last = Some((sx, sy));
                if app.focus_id.take().is_some() {
                    app.hover_id = None;
                    app.rebuild_instances();
                }
            }
            if let Some(target) = e.target() {
                if let Ok(el) = target.dyn_into::<Element>() {
                    let _ = el.set_pointer_capture(e.pointer_id());
                }
            }
        }) as Box<dyn FnMut(_)>);
        canvas.set_onpointerdown(Some(on_down.as_ref().unchecked_ref()));
        on_down.forget();
    }
    {
        let app = app.clone();
        let on_move = Closure::wrap(Box::new(move |e: PointerEvent| {
            let mut app = app.borrow_mut();
            let (sx, sy) = (
                e.offset_x() as f32 * app.dpr as f32,
                e.offset_y() as f32 * app.dpr as f32,
            );
            if let Some(drag) = &app.drag {
                let id = drag.id.clone();
                let grab = drag.grab;
                let (wx, wy) = app.renderer.screen_to_world(sx, sy);
                let (nx, ny) = (wx - grab.0, wy - grab.1);
                app.user_camera = true; // manual interaction: stop auto-framing
                                        // Optimistic local mutation; ships over WS via on_update.
                if let Err(error) = app.scene.set_props(
                    &id,
                    &[
                        ("x", PropValue::Num(nx as f64)),
                        ("y", PropValue::Num(ny as f64)),
                    ],
                ) {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "drag cancelled because object {id} is no longer mutable: {error}"
                    )));
                    app.drag = None;
                    if app.focus_id.as_ref() == Some(&id) {
                        app.focus_id = None;
                    }
                    if let Err(refresh_error) = app.refresh_objects() {
                        report_refresh_error(&refresh_error);
                    }
                    return;
                }
                if let Err(error) = app.refresh_objects() {
                    report_refresh_error(&error);
                }
            } else if let Some((lx, ly)) = app.pan_last {
                app.user_camera = true; // manual pan: stop auto-framing
                app.renderer.pan(sx - lx, sy - ly);
                app.pan_last = Some((sx, sy));
            } else {
                // Bare hover: highlight what's under the cursor and (re)arm the
                // dwell timer. Only rebuilds when the target actually changes,
                // so ordinary mouse motion is free.
                let (wx, wy) = app.renderer.screen_to_world(sx, sy);
                let hit = app.pick(wx, wy).map(|o| o.id.clone());
                if hit != app.hover_id {
                    app.hover_id = hit.clone();
                    app.focus_id = hit;
                    app.hover_since = now_ms();
                    app.dwell_sent = false;
                    app.rebuild_instances();
                }
            }
        }) as Box<dyn FnMut(_)>);
        canvas.set_onpointermove(Some(on_move.as_ref().unchecked_ref()));
        on_move.forget();
    }
    {
        let app = app.clone();
        let on_up = Closure::wrap(Box::new(move |_: PointerEvent| {
            let mut app = app.borrow_mut();
            app.pan_last = None;
            let Some(drag) = app.drag.take() else { return };
            // ONE semantic event per release — the agent-legible summary.
            let Some(object) = app.objects.iter().find(|o| o.id == drag.id) else {
                if app.focus_id.as_ref() == Some(&drag.id) {
                    app.focus_id = None;
                    app.rebuild_instances();
                }
                return;
            };
            let (to, kind) = ((object.x as f64, object.y as f64), object.kind.clone());
            let moved = (to.0 - drag.from.0).hypot(to.1 - drag.from.1);
            let url = app.semantic_url.clone();
            drop(app);
            // A press that didn't move the object is a click — the explicit
            // "I mean THIS one" deixis; a real displacement is a drag.
            let event = if moved < 0.06 {
                AgUiEvent::<JsonValue>::Custom(canvas_events::focus(
                    drag.id.as_str(),
                    &kind,
                    drag.from,
                    "click",
                    "human",
                ))
            } else {
                AgUiEvent::<JsonValue>::Custom(canvas_events::dragged(
                    drag.id.as_str(),
                    drag.from,
                    to,
                    "human",
                ))
            };
            post_semantic(url, event);
        }) as Box<dyn FnMut(_)>);
        canvas.set_onpointerup(Some(on_up.as_ref().unchecked_ref()));
        on_up.forget();
    }
    {
        let app = app.clone();
        let on_wheel = Closure::wrap(Box::new(move |e: WheelEvent| {
            e.prevent_default();
            let mut app = app.borrow_mut();
            app.user_camera = true; // manual zoom: stop auto-framing
            let factor = if e.delta_y() < 0.0 { 1.1 } else { 1.0 / 1.1 };
            let (cx, cy) = (
                e.offset_x() as f32 * app.dpr as f32,
                e.offset_y() as f32 * app.dpr as f32,
            );
            app.renderer.zoom_at(factor, cx, cy);
        }) as Box<dyn FnMut(_)>);
        canvas.set_onwheel(Some(on_wheel.as_ref().unchecked_ref()));
        on_wheel.forget();
    }

    // ── resize ───────────────────────────────────────────────────────────
    {
        let app = app.clone();
        let canvas = canvas.clone();
        let resize_window = window.clone();
        let on_resize = Closure::wrap(Box::new(move |_: web_sys::Event| {
            let mut app = app.borrow_mut();
            let dpr = resize_window.device_pixel_ratio();
            app.dpr = dpr;
            let w = (canvas.client_width() as f64 * dpr) as u32;
            let h = (canvas.client_height() as f64 * dpr) as u32;
            canvas.set_width(w.max(1));
            canvas.set_height(h.max(1));
            app.renderer.resize(w, h);
        }) as Box<dyn FnMut(_)>);
        window.add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref())?;
        on_resize.forget();
    }

    // ── render loop ──────────────────────────────────────────────────────
    spawn_raf_loop(app)?;
    Ok(())
}

/// Schedule `f` to run once after `ms` milliseconds (a one-shot setTimeout).
fn set_timeout<F: FnOnce() + 'static>(ms: i32, f: F) {
    let cb = Closure::once_into_js(f);
    match browser_window() {
        Ok(window) => {
            if let Err(error) =
                window.set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms)
            {
                web_sys::console::warn_1(&error);
            }
        }
        Err(error) => web_sys::console::warn_1(&error),
    }
}

/// Wire a socket's open/message/close handlers and keep the connection alive:
/// when the socket closes (server restart, network blip), open a fresh one and
/// re-wire after a short backoff. The socket cell always holds the live socket,
/// so the outbound path and HUD follow the reconnect automatically.
fn wire_and_reconnect(
    app: Rc<RefCell<App>>,
    socket: Rc<RefCell<WebSocket>>,
    applying: Rc<Cell<bool>>,
    ws_url: String,
) {
    // The reconnect thunk is self-referential (on_close calls it again), so it
    // lives in a cell that the close handler reads.
    let reconnect: ReconnectSlot = Rc::new(RefCell::new(None));
    let handlers: SocketHandlerSlot = Rc::new(RefCell::new(None));

    // Attach handlers to one specific socket.
    let wire: Rc<dyn Fn(&WebSocket)> = {
        let app = app.clone();
        let applying = applying.clone();
        let reconnect = reconnect.clone();
        let handlers = handlers.clone();
        Rc::new(move |ws: &WebSocket| {
            // onopen → mutual SyncStep1: ask the server what it has.
            let on_open = {
                let app = app.clone();
                let ws_open = ws.clone();
                let on_open = Closure::wrap(Box::new(move |_: web_sys::Event| {
                    let hello = match greeting(&app.borrow().scene) {
                        Ok(hello) => encode_sync(&hello),
                        Err(error) => {
                            web_sys::console::error_1(&JsValue::from_str(&format!(
                                "[canvas] cannot start authoritative sync: {error}"
                            )));
                            let _ = ws_open.close();
                            return;
                        }
                    };
                    if let Err(e) = ws_open.send_with_u8_array(&hello) {
                        web_sys::console::warn_1(&e);
                        let _ = ws_open.close();
                    }
                }) as Box<dyn FnMut(_)>);
                ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                on_open
            };
            // onmessage → apply frames; sync replies go back on THIS socket.
            let on_message = {
                let app = app.clone();
                let applying = applying.clone();
                let ws_reply = ws.clone();
                let on_message = Closure::wrap(Box::new(move |e: MessageEvent| {
                    let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() else {
                        web_sys::console::warn_1(&JsValue::from_str(
                            "[canvas] non-binary websocket message; reconnecting",
                        ));
                        let _ = ws_reply.close();
                        return;
                    };
                    // The single receive-path memcpy: JS ArrayBuffer → aligned
                    // buffer in wasm linear memory.
                    let view = js_sys::Uint8Array::new(&abuf);
                    let mut buf = AlignedBytes::zeroed(view.length() as usize);
                    view.copy_to(buf.as_bytes_mut());

                    let mut app = app.borrow_mut();
                    app.stats.msgs += 1;
                    app.stats.bytes += buf.len() as u64;

                    match decode_frame(buf.as_bytes()) {
                        Ok(Frame::Sync(payload)) => {
                            let t0 = now_ms();
                            applying.set(true);
                            let replies = handle_payload(&app.scene, payload);
                            applying.set(false);
                            app.stats.apply_us = (now_ms() - t0) * 1000.0;
                            match replies {
                                Ok(replies) => {
                                    for reply in replies {
                                        if let Err(error) =
                                            ws_reply.send_with_u8_array(&encode_sync(&reply))
                                        {
                                            web_sys::console::warn_1(&error);
                                            let _ = ws_reply.close();
                                            return;
                                        }
                                    }
                                }
                                Err(err) => {
                                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                                        "[canvas] sync error; reconnecting: {err}"
                                    )));
                                    let _ = ws_reply.close();
                                    return;
                                }
                            }
                            if let Err(error) = app.refresh_objects() {
                                report_refresh_error(&error);
                                let _ = ws_reply.close();
                                return;
                            }
                        }
                        Ok(Frame::Blob { header, payload }) => {
                            let t0 = now_ms();
                            if let Some(color) = authorized_blob_color(&app.objects, &header) {
                                if let Err(error) = app.renderer.upload_blob_colored(
                                    header.blob_id,
                                    header.generation,
                                    payload,
                                    color,
                                ) {
                                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                                        "[canvas] rejected malformed blob upload: {error}"
                                    )));
                                    let _ = ws_reply.close();
                                    return;
                                }
                                app.stats.upload_us = (now_ms() - t0) * 1000.0;
                            } else {
                                web_sys::console::warn_1(&JsValue::from_str(&format!(
                                    "[canvas] rejected unreferenced or mismatched blob {} generation {}",
                                    header.blob_id, header.generation
                                )));
                                let _ = ws_reply.close();
                                return;
                            }
                        }
                        Ok(_) => {
                            web_sys::console::warn_1(&JsValue::from_str(
                                "[canvas] unexpected frame; reconnecting",
                            ));
                            let _ = ws_reply.close();
                        }
                        Err(err) => {
                            web_sys::console::warn_1(&JsValue::from_str(&format!(
                                "[canvas] bad frame; reconnecting: {err}"
                            )));
                            let _ = ws_reply.close();
                        }
                    }
                }) as Box<dyn FnMut(_)>);
                ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
                on_message
            };
            // onclose → reconnect after a short backoff.
            let on_close = {
                let reconnect = reconnect.clone();
                let on_close = Closure::wrap(Box::new(move |_: web_sys::Event| {
                    if let Some(rc) = reconnect.borrow().as_ref().cloned() {
                        set_timeout(1000, move || rc());
                    }
                }) as Box<dyn FnMut(_)>);
                ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
                on_close
            };
            *handlers.borrow_mut() = Some(SocketHandlers {
                socket: ws.clone(),
                _on_open: on_open,
                _on_message: on_message,
                _on_close: on_close,
            });
        })
    };

    // reconnect(): open a fresh socket, store it, wire it. On failure to even
    // construct the socket, try again shortly.
    {
        let socket = socket.clone();
        let wire = wire.clone();
        let reconnect_cell = reconnect.clone();
        let thunk: Rc<dyn Fn()> = Rc::new(move || match WebSocket::new(&ws_url) {
            Ok(ws) => {
                ws.set_binary_type(BinaryType::Arraybuffer);
                *socket.borrow_mut() = ws.clone();
                wire(&ws);
            }
            Err(e) => {
                web_sys::console::warn_1(&e);
                if let Some(rc) = reconnect_cell.borrow().as_ref().cloned() {
                    set_timeout(1000, move || rc());
                }
            }
        });
        *reconnect.borrow_mut() = Some(thunk);
    }

    // Wire the initial socket that's already in the cell.
    let ws = socket.borrow().clone();
    wire(&ws);
}

/// The classic self-referential requestAnimationFrame loop. The closure is
/// held in an Rc cycle that lives for the page's lifetime — intentional.
fn spawn_raf_loop(app: Rc<RefCell<App>>) -> Result<(), JsValue> {
    let cell: AnimationFrameSlot = Rc::new(RefCell::new(None));
    let cell2 = cell.clone();

    *cell.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        let mut dwell_post: Option<AgUiEvent<JsonValue>> = None;
        let mut dwell_url = String::new();
        {
            let mut app = app.borrow_mut();
            app.renderer.render();

            // Rolling 1s rates for the HUD.
            app.stats.frames += 1;
            let now = now_ms();
            if now - app.stats.last_rate_ms >= 1000.0 {
                let dt = (now - app.stats.last_rate_ms) / 1000.0;
                app.stats.fps = (app.stats.frames as f64 / dt) as u32;
                app.stats.msgs_per_s = (app.stats.msgs as f64 / dt) as u32;
                app.stats.kib_per_s = app.stats.bytes as f64 / dt / 1024.0;
                app.stats.frames = 0;
                app.stats.msgs = 0;
                app.stats.bytes = 0;
                app.stats.last_rate_ms = now;
            }
            if let Some(hud) = &app.hud {
                let s = &app.stats;
                hud.set_text_content(Some(&format!(
                    "fps {:>3}   ws {:>3} msg/s  {:>8.1} KiB/s\n\
                     yrs apply {:>7.1} µs   blob upload {:>7.1} µs\n\
                     objects {}   ws_state {}",
                    s.fps,
                    s.msgs_per_s,
                    s.kib_per_s,
                    s.apply_us,
                    s.upload_us,
                    app.objects.len(),
                    app.socket.borrow().ready_state(),
                )));
            }

            // Dwell: the cursor has rested on an object → report it once. This
            // is the ambient "pointing while I talk" signal (a click is the
            // explicit one). Fired from the RAF tick so it lands without a
            // further mouse move, then latched via `dwell_sent`.
            if !app.dwell_sent {
                let hover = app.hover_id.clone();
                if let Some(id) = hover {
                    if now - app.hover_since >= DWELL_MS {
                        app.dwell_sent = true;
                        let found = app
                            .objects
                            .iter()
                            .find(|o| o.id.as_str() == id.as_str())
                            .map(|o| (o.kind.clone(), (o.x as f64, o.y as f64)));
                        if let Some((kind, world)) = found {
                            dwell_url = app.semantic_url.clone();
                            dwell_post = Some(AgUiEvent::Custom(canvas_events::focus(
                                id.as_str(),
                                &kind,
                                world,
                                "dwell",
                                "human",
                            )));
                        }
                    }
                }
            }
        }
        if let Some(event) = dwell_post {
            post_semantic(dwell_url, event);
        }
        // Re-arm.
        if let Some(closure) = cell2.borrow().as_ref() {
            match browser_window().and_then(|window| {
                window
                    .request_animation_frame(closure.as_ref().unchecked_ref())
                    .map(|_| ())
            }) {
                Ok(()) => {}
                Err(error) => web_sys::console::warn_1(&error),
            }
        }
    }) as Box<dyn FnMut()>));

    let borrow = cell.borrow();
    let closure = borrow
        .as_ref()
        .ok_or_else(|| JsValue::from_str("animation-frame closure was not initialized"))?;
    browser_window()?.request_animation_frame(closure.as_ref().unchecked_ref())?;
    // `cell` and the closure form an Rc cycle and stay alive forever — the
    // page-lifetime render loop. (Same forget-discipline as the WS closures.)
    Ok(())
}

/// One-line summary of an incoming AG-UI event for the HUD log.
fn describe_event(evt: &JsValue) -> String {
    let get = |key: &str| {
        js_sys::Reflect::get(evt, &JsValue::from_str(key))
            .ok()
            .and_then(|v| v.as_string())
    };
    let ty = get("type").unwrap_or_else(|| "?".into());
    match ty.as_str() {
        "CUSTOM" => {
            let name = get("name").unwrap_or_default();
            let value = js_sys::Reflect::get(evt, &JsValue::from_str("value"))
                .ok()
                .and_then(|v| js_sys::JSON::stringify(&v).ok())
                .map(String::from)
                .unwrap_or_default();
            format!("{name} {value}")
        }
        _ => ty,
    }
}
