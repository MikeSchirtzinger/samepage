//! The 2D camera, shared by the host and the browser replica.
//!
//! Every number that decides where the board sits lives here. The page
//! forwards pointer, wheel, and key events; it does not compute a transform,
//! a scale, a fit, or an anchored zoom. The reason is not code size. The host
//! and the browser have to agree about where things are and what is readable,
//! and two implementations of that are two opinions.
//!
//! Sources for the rules, named where a rule was taken from a working editor:
//!
//! - Anchored wheel zoom: excalidraw `packages/excalidraw/viewport.ts:204`,
//!   `getViewportForZoom`, which re-solves scroll so the world point under the
//!   cursor stays under the cursor.
//! - Exponential approach with an exact snap: agentviz `src/lib.rs:8752-8793`.
//! - `cancel_and_resync` on pointerdown: agentviz
//!   `shells/shared/input-handlers.js:54-61`, whose comment records that
//!   without the resync a drag applies from the stale pre-focus pose and the
//!   camera fights itself.
//!
//! Pan is one to one in screen pixels at every zoom. agentviz scales pan by
//! camera radius because it is an orbit rig where distance changes apparent
//! size; a 2D canvas has no such term, so that rule does not port.

use serde::{Deserialize, Serialize};

/// Zoom clamp. Both ends are the numbers the shipped renderer already used.
pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 2.4;

/// The most a reveal will zoom in. Closer than this and "look at this" turns
/// into "and nothing else exists".
pub const REVEAL_MAX_SCALE: f64 = 1.15;

/// Rate of the exponential approach, per second.
const APPROACH_RATE: f64 = 8.0;
/// Below this, in screen pixels and in scale units, the approach snaps to the
/// target exactly rather than leaving a camera that is forever nearly there.
const APPROACH_EPSILON: f64 = 0.01;

/// A world-space rectangle: what the document says, before any camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl WorldBox {
    fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }
}

/// The pane the camera is looking through, in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
}

impl Viewport {
    fn is_usable(&self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width > 0.0 && self.height > 0.0
    }
}

/// A screen-space point, in CSS pixels relative to the pane's top left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// The camera itself. `x`/`y` are the translation in screen pixels applied
/// before `scale`, which is exactly what the page writes into
/// `transform: translate(...) scale(...)`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera2d {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

impl Default for Camera2d {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
        }
    }
}

/// Clamp into the zoom range, and refuse to store a value arithmetic cannot
/// recover from. A NaN scale is one bad wheel event away from a board nobody
/// can get back, so it is rejected at the setter rather than rendered.
fn clamp_scale(scale: f64, max: f64) -> f64 {
    if !scale.is_finite() {
        return 1.0;
    }
    scale.clamp(MIN_SCALE, max.min(MAX_SCALE))
}

impl Camera2d {
    pub fn new(x: f64, y: f64, scale: f64) -> Self {
        Self {
            x,
            y,
            scale: clamp_scale(scale, MAX_SCALE),
        }
    }

    /// Pan by a screen-pixel delta, one to one, at every zoom.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        self.x += dx;
        self.y += dy;
    }

    /// Zoom about a screen point, keeping the world point under it fixed.
    pub fn zoom_at(&mut self, cursor: Point, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let before = self.to_world(cursor);
        let next = clamp_scale(self.scale * factor, MAX_SCALE);
        self.scale = next;
        self.x = cursor.x - before.x * next;
        self.y = cursor.y - before.y * next;
    }

    /// Zoom about the middle of the pane. What the +/- buttons do.
    pub fn zoom_from_center(&mut self, viewport: Viewport, factor: f64) {
        if !viewport.is_usable() {
            return;
        }
        self.zoom_at(
            Point {
                x: viewport.width / 2.0,
                y: viewport.height / 2.0,
            },
            factor,
        );
    }

    pub fn to_world(&self, screen: Point) -> Point {
        Point {
            x: (screen.x - self.x) / self.scale,
            y: (screen.y - self.y) / self.scale,
        }
    }

    pub fn to_screen(&self, world: Point) -> Point {
        Point {
            x: world.x * self.scale + self.x,
            y: world.y * self.scale + self.y,
        }
    }

    /// The world rectangle the pane currently shows. This is the viewport the
    /// attention payload reports.
    pub fn world_viewport(&self, viewport: Viewport) -> Option<WorldBox> {
        if !viewport.is_usable() || self.scale <= 0.0 {
            return None;
        }
        Some(WorldBox {
            x: -self.x / self.scale,
            y: -self.y / self.scale,
            w: viewport.width / self.scale,
            h: viewport.height / self.scale,
        })
    }

    /// The CSS transform the page writes. One string, so the page never
    /// assembles a transform out of its own arithmetic.
    pub fn transform(&self) -> String {
        format!(
            "translate({}px, {}px) scale({})",
            self.x, self.y, self.scale
        )
    }
}

/// A framing decision: where the camera should be, plus the extent it was
/// derived from. The extent is what stops a re-fit from fighting a reveal:
/// the same content extent is only ever fitted once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fit {
    pub camera: Camera2d,
    pub extent: String,
}

fn union(boxes: &[WorldBox]) -> Option<(f64, f64, f64, f64)> {
    let mut iter = boxes.iter().filter(|entry| entry.is_finite());
    let first = iter.next()?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x + first.w;
    let mut max_y = first.y + first.h;
    for entry in iter {
        min_x = min_x.min(entry.x);
        min_y = min_y.min(entry.y);
        max_x = max_x.max(entry.x + entry.w);
        max_y = max_y.max(entry.y + entry.h);
    }
    Some((min_x, min_y, max_x, max_y))
}

/// Geometry-only fit: how the union of these boxes fills this pane.
///
/// `top_inset` is the strip of the pane that chrome floating over the canvas
/// (the draw palette) already owns. A fit that ignores it puts the heading of
/// a tall drawing under the palette.
///
/// The caller decides which document kinds contribute boxes. This function
/// only answers how their union fits.
pub fn fit(
    boxes: &[WorldBox],
    viewport: Viewport,
    margin: f64,
    top_inset: f64,
    max_scale: f64,
) -> Option<Fit> {
    if !viewport.is_usable() {
        return None;
    }
    let (min_x, min_y, max_x, max_y) = union(boxes)?;
    let scale = clamp_scale(
        (max_scale.min(MAX_SCALE))
            .min((viewport.width - margin * 2.0) / (max_x - min_x).max(1.0))
            .min((viewport.height - top_inset - margin * 2.0) / (max_y - min_y).max(1.0)),
        max_scale,
    );
    Some(Fit {
        camera: Camera2d {
            x: viewport.width / 2.0 - ((min_x + max_x) / 2.0) * scale,
            y: (viewport.height + top_inset) / 2.0 - ((min_y + max_y) / 2.0) * scale,
            scale,
        },
        extent: format!("{min_x}:{min_y}:{max_x}:{max_y}"),
    })
}

/// How an untouched board opens.
///
/// Opening a large map at its all-content fit can make every title technically
/// visible and practically unreadable. The all-content extent stays the
/// authority; only when that fit falls below the title floor does the opening
/// centre the fixed readable scale on the densest deterministic cluster. There
/// is no collision search and no camera iteration: each candidate box is
/// scored once.
///
/// `cohesion_floor` is the case that rule had no answer for: ONE connected
/// diagram, authored in one call, in answer to a request to see the whole
/// system. Framing the densest cluster of that is a fragment, and a fragment
/// of an architecture review is worse than a small whole. Measured: an 18-node
/// hierarchy opened at 81% with two of its four frames off screen and a third
/// cut off at the left edge, while its true whole-content fit was 41% and
/// every card title in it was legible. So when the caller says the content is
/// one connected thing and the whole fit clears the floor, the whole fit wins.
/// Below the floor the cluster opening still takes over, which is what stops
/// the smear this rule was built for.
pub fn opening(
    boxes: &[WorldBox],
    viewport: Viewport,
    readable_scale: f64,
    margin: f64,
    cluster_boxes: &[WorldBox],
    cohesion_floor: Option<f64>,
    max_scale: f64,
) -> Option<Fit> {
    let whole = fit(boxes, viewport, margin, 0.0, max_scale)?;
    if whole.camera.scale >= readable_scale {
        return Some(whole);
    }
    if let Some(floor) = cohesion_floor {
        if floor.is_finite() && whole.camera.scale >= floor {
            return Some(whole);
        }
    }
    let candidates: Vec<WorldBox> = cluster_boxes
        .iter()
        .copied()
        .filter(WorldBox::is_finite)
        .collect();
    if candidates.is_empty() {
        return Some(Fit {
            camera: Camera2d {
                scale: clamp_scale(readable_scale, max_scale),
                ..whole.camera
            },
            extent: whole.extent,
        });
    }

    let world_width = ((viewport.width - margin * 2.0) / readable_scale).max(1.0);
    let world_height = ((viewport.height - margin * 2.0) / readable_scale).max(1.0);
    let mut best: Option<(usize, usize, f64, f64, f64, f64, f64)> = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let center = candidate.center();
        let members: Vec<&WorldBox> = candidates
            .iter()
            .filter(|entry| {
                let point = entry.center();
                (point.0 - center.0).abs() <= world_width / 2.0
                    && (point.1 - center.1).abs() <= world_height / 2.0
            })
            .collect();
        let left = members.iter().map(|entry| entry.x).fold(f64::MAX, f64::min);
        let top = members.iter().map(|entry| entry.y).fold(f64::MAX, f64::min);
        let right = members
            .iter()
            .map(|entry| entry.x + entry.w)
            .fold(f64::MIN, f64::max);
        let bottom = members
            .iter()
            .map(|entry| entry.y + entry.h)
            .fold(f64::MIN, f64::max);
        let area = (right - left).max(1.0) * (bottom - top).max(1.0);
        let count = members.len();
        let better = match best {
            None => true,
            Some((best_count, best_index, best_area, ..)) => {
                count > best_count
                    || (count == best_count && area < best_area)
                    || (count == best_count && area == best_area && index < best_index)
            }
        };
        if better {
            best = Some((count, index, area, left, top, right, bottom));
        }
    }
    let (_, _, _, left, top, right, bottom) = best?;
    let center_x = (left + right) / 2.0;
    let center_y = (top + bottom) / 2.0;
    let scale = clamp_scale(readable_scale, max_scale);
    Some(Fit {
        camera: Camera2d {
            x: viewport.width / 2.0 - center_x * scale,
            y: viewport.height / 2.0 - center_y * scale,
            scale,
        },
        extent: whole.extent,
    })
}

/// Where the camera goes to make one target legible in the middle of the pane.
///
/// A small target needs a closer view; a very large region needs the opposite.
/// "Reveal" means make the whole target legible, not zoom in at all costs.
pub fn focus(target: WorldBox, viewport: Viewport) -> Option<Camera2d> {
    if !target.is_finite() || !viewport.is_usable() {
        return None;
    }
    let scale = clamp_scale(readable_scale_for(target, viewport), MAX_SCALE);
    let (center_x, center_y) = target.center();
    Some(Camera2d {
        x: viewport.width / 2.0 - center_x * scale,
        y: viewport.height / 2.0 - center_y * scale,
        scale,
    })
}

/// The scale at which this target is fully legible, before clamping. Exposed
/// because the reveal policy compares it against the current scale to decide
/// whether moving the camera is warranted at all.
pub fn readable_scale_for(target: WorldBox, viewport: Viewport) -> f64 {
    REVEAL_MAX_SCALE
        .min((viewport.width * 0.72) / target.w.max(1.0))
        .min((viewport.height * 0.64) / target.h.max(1.0))
}

/// An animated move toward a target pose.
///
/// Exponential approach, `alpha = 1 - exp(-rate * dt)`, with an exact snap on
/// convergence. The snap is the part that matters: an approach that only ever
/// gets closer leaves a camera permanently a fraction of a pixel off, and
/// every anchored overlay then reprojects forever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Approach {
    pub target: Camera2d,
    pub active: bool,
}

impl Approach {
    pub fn to(target: Camera2d) -> Self {
        Self {
            target,
            active: true,
        }
    }

    /// Advance `current` toward the target by `dt` seconds. Returns the new
    /// pose; `active` goes false on the step that snaps.
    pub fn step(&mut self, current: Camera2d, dt: f64) -> Camera2d {
        if !self.active {
            return current;
        }
        if !dt.is_finite() || dt <= 0.0 {
            return current;
        }
        let alpha = 1.0 - (-APPROACH_RATE * dt).exp();
        let next = Camera2d {
            x: current.x + (self.target.x - current.x) * alpha,
            y: current.y + (self.target.y - current.y) * alpha,
            scale: current.scale + (self.target.scale - current.scale) * alpha,
        };
        if (self.target.x - next.x).abs() < APPROACH_EPSILON
            && (self.target.y - next.y).abs() < APPROACH_EPSILON
            && (self.target.scale - next.scale).abs() < APPROACH_EPSILON
        {
            self.active = false;
            return self.target;
        }
        next
    }

    /// Stop animating and adopt the pose the camera is actually at.
    ///
    /// Called on pointerdown, before any drag delta is applied. Without it the
    /// drag starts from the pose the animation was aiming at rather than the
    /// one on screen, and the camera fights the hand.
    pub fn cancel_and_resync(&mut self, current: Camera2d) {
        self.active = false;
        self.target = current;
    }
}

/// A rectangle in minimap pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MinimapRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The whole board drawn small, with the pane's own rectangle on top of it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MinimapView {
    /// World units to minimap pixels.
    pub scale: f64,
    /// Where the content sits inside the minimap.
    pub content: MinimapRect,
    /// Where the pane's current world viewport sits inside the minimap. It
    /// can extend past the content, and does whenever the human has panned
    /// off the board, which is exactly when a minimap earns its space.
    pub viewport: MinimapRect,
}

/// Lay the board out inside a minimap of this size.
///
/// `None` when there is no content or no room: the caller draws an empty
/// frame rather than a minimap of nothing, because a control that vanishes
/// when the board is empty is a control nobody finds when they need it.
pub fn minimap_view(
    content: WorldBox,
    seen: WorldBox,
    width: f64,
    height: f64,
    margin: f64,
) -> Option<MinimapView> {
    if !content.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let content_w = content.w.max(1.0);
    let content_h = content.h.max(1.0);
    let scale = ((width - margin * 2.0) / content_w).min((height - margin * 2.0) / content_h);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let offset_x = (width - content_w * scale) / 2.0 - content.x * scale;
    let offset_y = (height - content_h * scale) / 2.0 - content.y * scale;
    let place = |box_: WorldBox| MinimapRect {
        x: box_.x * scale + offset_x,
        y: box_.y * scale + offset_y,
        w: box_.w * scale,
        h: box_.h * scale,
    };
    Some(MinimapView {
        scale,
        content: place(content),
        viewport: place(seen),
    })
}

/// The world point under a minimap pixel.
pub fn minimap_to_world(view: &MinimapView, content: WorldBox, x: f64, y: f64) -> Point {
    Point {
        x: content.x + (x - view.content.x) / view.scale,
        y: content.y + (y - view.content.y) / view.scale,
    }
}

/// The camera that puts a world point in the middle of the pane, keeping the
/// current zoom. What a click on the minimap asks for.
pub fn centered_on(point: Point, viewport: Viewport, scale: f64) -> Option<Camera2d> {
    if !viewport.is_usable() || !point.x.is_finite() || !point.y.is_finite() {
        return None;
    }
    let scale = clamp_scale(scale, MAX_SCALE);
    Some(Camera2d {
        x: viewport.width / 2.0 - point.x * scale,
        y: viewport.height / 2.0 - point.y * scale,
        scale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANE: Viewport = Viewport {
        width: 1000.0,
        height: 800.0,
    };

    fn boxed(x: f64, y: f64, w: f64, h: f64) -> WorldBox {
        WorldBox { x, y, w, h }
    }

    #[test]
    fn pan_is_one_to_one_at_every_zoom() {
        for scale in [0.2, 1.0, 2.4] {
            let mut camera = Camera2d::new(10.0, 20.0, scale);
            camera.pan(37.0, -11.0);
            assert_eq!(camera.x, 47.0);
            assert_eq!(camera.y, 9.0);
            assert_eq!(camera.scale, scale, "pan must not touch the zoom");
        }
    }

    #[test]
    fn zoom_keeps_the_world_point_under_the_cursor() {
        let mut camera = Camera2d::new(-120.0, 55.0, 0.7);
        let cursor = Point { x: 640.0, y: 210.0 };
        let before = camera.to_world(cursor);
        camera.zoom_at(cursor, 1.1);
        let after = camera.to_world(cursor);
        assert!((before.x - after.x).abs() < 1e-9, "{before:?} {after:?}");
        assert!((before.y - after.y).abs() < 1e-9, "{before:?} {after:?}");
        assert!((camera.scale - 0.77).abs() < 1e-9);
    }

    #[test]
    fn zoom_clamps_at_both_ends_and_stays_anchored_there() {
        let mut camera = Camera2d::new(0.0, 0.0, MAX_SCALE);
        let cursor = Point { x: 300.0, y: 300.0 };
        let before = camera.to_world(cursor);
        camera.zoom_at(cursor, 4.0);
        assert_eq!(camera.scale, MAX_SCALE);
        let after = camera.to_world(cursor);
        assert!((before.x - after.x).abs() < 1e-9);

        let mut camera = Camera2d::new(0.0, 0.0, MIN_SCALE);
        camera.zoom_at(cursor, 0.1);
        assert_eq!(camera.scale, MIN_SCALE);
    }

    #[test]
    fn a_non_finite_gesture_cannot_destroy_the_view() {
        let mut camera = Camera2d::new(4.0, 5.0, 1.0);
        camera.pan(f64::NAN, 3.0);
        camera.zoom_at(Point { x: 0.0, y: 0.0 }, f64::INFINITY);
        camera.zoom_at(Point { x: 0.0, y: 0.0 }, 0.0);
        assert_eq!(camera, Camera2d::new(4.0, 5.0, 1.0));
    }

    #[test]
    fn screen_and_world_round_trip() {
        let camera = Camera2d::new(-40.5, 12.25, 0.63);
        let world = Point { x: 314.0, y: -88.0 };
        let back = camera.to_world(camera.to_screen(world));
        assert!((back.x - world.x).abs() < 1e-9);
        assert!((back.y - world.y).abs() < 1e-9);
    }

    #[test]
    fn fit_centers_the_union_and_respects_the_margin() {
        let fitted = fit(
            &[
                boxed(0.0, 0.0, 200.0, 100.0),
                boxed(300.0, 200.0, 100.0, 100.0),
            ],
            PANE,
            40.0,
            0.0,
            1.0,
        )
        .expect("boxes and a pane fit");
        // 400 x 300 of content into 920 x 720 of usable pane: the pane is
        // bigger, so the fit is capped at max_scale rather than magnifying.
        assert_eq!(fitted.camera.scale, 1.0);
        let center = Point { x: 200.0, y: 150.0 };
        let on_screen = fitted.camera.to_screen(center);
        assert!((on_screen.x - PANE.width / 2.0).abs() < 1e-9);
        assert!((on_screen.y - PANE.height / 2.0).abs() < 1e-9);
        assert_eq!(fitted.extent, "0:0:400:300");
    }

    #[test]
    fn a_top_inset_pushes_the_content_below_the_palette() {
        let plain = fit(&[boxed(0.0, 0.0, 4000.0, 4000.0)], PANE, 40.0, 0.0, 1.0).unwrap();
        let inset = fit(&[boxed(0.0, 0.0, 4000.0, 4000.0)], PANE, 40.0, 120.0, 1.0).unwrap();
        assert!(
            inset.camera.scale < plain.camera.scale,
            "the inset takes height away from the fit"
        );
        assert!(
            inset.camera.to_screen(Point { x: 0.0, y: 0.0 }).y
                > plain.camera.to_screen(Point { x: 0.0, y: 0.0 }).y,
            "content has to move down, out from under the palette"
        );
    }

    #[test]
    fn an_empty_document_has_no_fit() {
        assert!(fit(&[], PANE, 40.0, 0.0, 1.0).is_none());
        assert!(fit(
            &[boxed(0.0, 0.0, 10.0, 10.0)],
            Viewport {
                width: 0.0,
                height: 0.0
            },
            40.0,
            0.0,
            1.0
        )
        .is_none());
    }

    #[test]
    fn the_opening_keeps_a_whole_fit_that_is_already_readable() {
        let boxes = [
            boxed(0.0, 0.0, 300.0, 200.0),
            boxed(400.0, 0.0, 300.0, 200.0),
        ];
        let opened = opening(&boxes, PANE, 0.8148, 40.0, &boxes, None, 1.0).unwrap();
        assert_eq!(
            opened.camera,
            fit(&boxes, PANE, 40.0, 0.0, 1.0).unwrap().camera
        );
    }

    #[test]
    fn the_opening_frames_the_densest_cluster_below_the_title_floor() {
        // Three cards together, one card a long way off. The whole fit is a
        // smear; the cluster is what a person can read.
        let boxes = [
            boxed(0.0, 0.0, 200.0, 120.0),
            boxed(220.0, 0.0, 200.0, 120.0),
            boxed(110.0, 160.0, 200.0, 120.0),
            boxed(9000.0, 9000.0, 200.0, 120.0),
        ];
        let readable = 0.8148;
        let opened = opening(&boxes, PANE, readable, 40.0, &boxes, None, 1.0).unwrap();
        assert!((opened.camera.scale - readable).abs() < 1e-9);
        let cluster_center = Point { x: 210.0, y: 140.0 };
        let on_screen = opened.camera.to_screen(cluster_center);
        assert!(
            (on_screen.x - PANE.width / 2.0).abs() < 1.0,
            "{on_screen:?}"
        );
        assert!(
            (on_screen.y - PANE.height / 2.0).abs() < 1.0,
            "{on_screen:?}"
        );
    }

    #[test]
    fn one_connected_diagram_opens_whole_above_the_cohesion_floor() {
        // Eighteen cards laid out as a hierarchy that fits at about 41%: below
        // the title floor, above the cohesion floor. The measured regression
        // this guards is the 81% opening that cut three frames off.
        let mut boxes = Vec::new();
        for row in 0..6 {
            for column in 0..3 {
                boxes.push(boxed(
                    f64::from(column) * 700.0,
                    f64::from(row) * 340.0,
                    600.0,
                    240.0,
                ));
            }
        }
        let readable = 0.8148;
        let cohesion = 0.29;
        let whole = fit(&boxes, PANE, 40.0, 0.0, 1.0).unwrap();
        assert!(
            whole.camera.scale < readable,
            "premise: below the title floor"
        );
        assert!(
            whole.camera.scale >= cohesion,
            "premise: above the cohesion floor"
        );

        let cohesive = opening(&boxes, PANE, readable, 40.0, &boxes, Some(cohesion), 1.0).unwrap();
        assert_eq!(
            cohesive.camera, whole.camera,
            "a connected diagram opens whole"
        );

        let fragmented = opening(&boxes, PANE, readable, 40.0, &boxes, None, 1.0).unwrap();
        assert!(
            fragmented.camera.scale > whole.camera.scale,
            "without the cohesion floor the old cluster opening still applies"
        );
    }

    #[test]
    fn focus_centers_the_target_and_never_zooms_past_the_reveal_cap() {
        let target = boxed(1000.0, 1000.0, 20.0, 20.0);
        let camera = focus(target, PANE).unwrap();
        assert!(camera.scale <= REVEAL_MAX_SCALE);
        let on_screen = camera.to_screen(Point {
            x: 1010.0,
            y: 1010.0,
        });
        assert!((on_screen.x - PANE.width / 2.0).abs() < 1e-9);
        assert!((on_screen.y - PANE.height / 2.0).abs() < 1e-9);
    }

    #[test]
    fn focus_zooms_out_for_a_region_larger_than_the_pane() {
        let camera = focus(boxed(0.0, 0.0, 4000.0, 4000.0), PANE).unwrap();
        assert!(camera.scale < 1.0);
        assert!(camera.scale >= MIN_SCALE);
    }

    #[test]
    fn the_world_viewport_is_what_the_pane_shows() {
        let camera = Camera2d::new(-100.0, -50.0, 0.5);
        let seen = camera.world_viewport(PANE).unwrap();
        assert_eq!(seen.x, 200.0);
        assert_eq!(seen.y, 100.0);
        assert_eq!(seen.w, 2000.0);
        assert_eq!(seen.h, 1600.0);
    }

    #[test]
    fn the_approach_converges_and_snaps_exactly() {
        let target = Camera2d::new(300.0, -200.0, 1.8);
        let mut approach = Approach::to(target);
        let mut current = Camera2d::default();
        let mut steps = 0;
        while approach.active && steps < 1000 {
            current = approach.step(current, 1.0 / 60.0);
            steps += 1;
        }
        assert!(!approach.active, "the approach never finished");
        assert_eq!(current, target, "convergence has to be exact, not nearly");
        assert!(steps < 200, "took {steps} frames to arrive");
    }

    #[test]
    fn a_finished_approach_stops_moving_the_camera() {
        let mut approach = Approach::default();
        let current = Camera2d::new(5.0, 6.0, 0.9);
        assert_eq!(approach.step(current, 0.016), current);
    }

    #[test]
    fn cancel_and_resync_adopts_the_pose_on_screen() {
        let mut approach = Approach::to(Camera2d::new(1000.0, 1000.0, 2.0));
        let on_screen = Camera2d::new(12.0, -4.0, 0.8);
        approach.cancel_and_resync(on_screen);
        assert!(!approach.active);
        assert_eq!(approach.target, on_screen);
        assert_eq!(
            approach.step(on_screen, 0.016),
            on_screen,
            "a cancelled approach must not drag the camera back"
        );
    }

    #[test]
    fn the_minimap_fits_the_board_and_marks_where_you_are_looking() {
        let content = boxed(0.0, 0.0, 2000.0, 1000.0);
        let seen = boxed(500.0, 250.0, 500.0, 250.0);
        let view = minimap_view(content, seen, 200.0, 140.0, 6.0).expect("a minimap");
        // 188/2000 = 0.094 beats 128/1000 = 0.128, so width binds.
        assert!((view.scale - 0.094).abs() < 1e-9);
        assert!(view.content.w <= 200.0 && view.content.h <= 140.0);
        assert!(
            view.viewport.x > view.content.x && view.viewport.y > view.content.y,
            "the viewport marker sits inside the content it is inside"
        );
        // Round trip: the middle of the viewport marker is the middle of what
        // the pane is looking at.
        let middle = minimap_to_world(
            &view,
            content,
            view.viewport.x + view.viewport.w / 2.0,
            view.viewport.y + view.viewport.h / 2.0,
        );
        assert!((middle.x - 750.0).abs() < 1e-6, "{middle:?}");
        assert!((middle.y - 375.0).abs() < 1e-6, "{middle:?}");
    }

    #[test]
    fn a_viewport_panned_off_the_board_still_has_a_marker() {
        let content = boxed(0.0, 0.0, 1000.0, 1000.0);
        let view = minimap_view(content, boxed(4000.0, 0.0, 500.0, 500.0), 200.0, 200.0, 6.0)
            .expect("a minimap");
        assert!(
            view.viewport.x > view.content.x + view.content.w,
            "off the board is a fact the minimap has to be able to show"
        );
    }

    #[test]
    fn an_empty_or_impossible_minimap_declines_rather_than_drawing_nonsense() {
        assert!(minimap_view(
            boxed(0.0, 0.0, 100.0, 100.0),
            boxed(0.0, 0.0, 10.0, 10.0),
            0.0,
            0.0,
            6.0
        )
        .is_none());
        assert!(minimap_view(
            boxed(f64::NAN, 0.0, 100.0, 100.0),
            boxed(0.0, 0.0, 10.0, 10.0),
            200.0,
            200.0,
            6.0
        )
        .is_none());
        // A minimap smaller than twice its own margin has no room for content.
        assert!(minimap_view(
            boxed(0.0, 0.0, 100.0, 100.0),
            boxed(0.0, 0.0, 10.0, 10.0),
            8.0,
            8.0,
            6.0
        )
        .is_none());
    }

    #[test]
    fn centering_keeps_the_zoom_and_puts_the_point_in_the_middle() {
        let camera = centered_on(Point { x: 900.0, y: 400.0 }, PANE, 0.6).expect("a camera");
        assert_eq!(camera.scale, 0.6);
        let on_screen = camera.to_screen(Point { x: 900.0, y: 400.0 });
        assert!((on_screen.x - PANE.width / 2.0).abs() < 1e-9);
        assert!((on_screen.y - PANE.height / 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_transform_is_the_one_the_page_writes() {
        let camera = Camera2d::new(-12.5, 30.0, 0.5);
        assert_eq!(camera.transform(), "translate(-12.5px, 30px) scale(0.5)");
    }
}
