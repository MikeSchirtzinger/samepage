//! Projecting a world box to the rectangle an overlay should occupy.
//!
//! The ring, the mark rail, the popovers, and the minimap viewport rectangle
//! are one problem: a world box, a camera, and a set of clipping ancestors,
//! projected to a screen rectangle.
//!
//! The failure this replaces is structural rather than a missing listener.
//! `renderAgentAttention` read `getBoundingClientRect()` at the moment a
//! presence snapshot arrived and wrote fixed pixels onto the overlay. Pan,
//! zoom, and resize called it zero times, so the ring kept the pixels it was
//! given and the board moved out from under it.
//!
//! agentviz's answer, adopted here in substance: stop synchronizing, start
//! recomputing (`archive/legacy-demo.html:2795-2878` rebuilds every label
//! position from the camera). The mechanism does not port: a
//! `requestAnimationFrame` poll paints nothing in a background or headless
//! tab, which is exactly where an agent verifies its own work. So the atlas
//! takes the principle and drives it from events, and this module is the pure
//! function those events call.

use serde::{Deserialize, Serialize};

use crate::camera::{Camera2d, Point, WorldBox};

/// How far inside a clip edge an off-screen marker sits, in screen pixels. A
/// marker exactly on the boundary is half outside the thing it is pointing
/// into.
pub const EDGE_INSET: f64 = 12.0;

/// A clipping ancestor, in the same client coordinates the overlay is
/// positioned in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl ClipRect {
    pub fn intersect(self, other: ClipRect) -> Option<ClipRect> {
        let clipped = ClipRect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (clipped.right > clipped.left && clipped.bottom > clipped.top).then_some(clipped)
    }

    fn is_finite(&self) -> bool {
        self.left.is_finite()
            && self.top.is_finite()
            && self.right.is_finite()
            && self.bottom.is_finite()
    }
}

/// Where an overlay goes, in client pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl ScreenRect {
    fn as_clip(self) -> ClipRect {
        ClipRect {
            left: self.left,
            top: self.top,
            right: self.left + self.width,
            bottom: self.top + self.height,
        }
    }

    fn center(self) -> Point {
        Point {
            x: self.left + self.width / 2.0,
            y: self.top + self.height / 2.0,
        }
    }
}

/// Which way an off-screen target lies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The arrow the edge label carries. One place, so the glyph and the
    /// direction cannot disagree.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Left => "←",
            Direction::Right => "→",
            Direction::Up => "↑",
            Direction::Down => "↓",
        }
    }
}

/// What the page draws for one anchor.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Anchor {
    /// Ring what can actually be seen. A card half out of its pane gets a ring
    /// on the part that is on screen rather than one that spills across the
    /// next column.
    Visible(ScreenRect),
    /// The target is outside every clip. Say so at the edge of the pane it
    /// lives in, pointing the way, instead of drawing a ring somewhere it
    /// would lie.
    Edge {
        x: f64,
        y: f64,
        direction: Direction,
    },
    /// There is nowhere honest to draw: the clips do not intersect at all, so
    /// there is no pane edge to point from either.
    Nowhere,
}

/// Project one world box.
///
/// `origin` is where the world layer's client coordinates start: the pane's
/// own top-left. The camera maps world to pane, and the origin maps pane to
/// client, which is what the overlay (positioned against the window) needs.
pub fn project(camera: &Camera2d, origin: Point, world: WorldBox, clips: &[ClipRect]) -> Anchor {
    if !world.x.is_finite() || !world.y.is_finite() || !world.w.is_finite() || !world.h.is_finite()
    {
        return Anchor::Nowhere;
    }
    let top_left = camera.to_screen(Point {
        x: world.x,
        y: world.y,
    });
    let rect = ScreenRect {
        left: top_left.x + origin.x,
        top: top_left.y + origin.y,
        width: (world.w * camera.scale).max(0.0),
        height: (world.h * camera.scale).max(0.0),
    };

    let mut clip: Option<ClipRect> = None;
    for candidate in clips.iter().filter(|entry| entry.is_finite()) {
        clip = match clip {
            None => Some(*candidate),
            Some(current) => match current.intersect(*candidate) {
                // Two clipping ancestors that do not overlap leave the target
                // with no visible region anywhere, and no edge to point from.
                None => return Anchor::Nowhere,
                Some(next) => Some(next),
            },
        };
    }
    let Some(clip) = clip else {
        // Nothing clips this target: whatever it projects to is what is drawn.
        return Anchor::Visible(rect);
    };

    if let Some(visible) = rect.as_clip().intersect(clip) {
        return Anchor::Visible(ScreenRect {
            left: visible.left,
            top: visible.top,
            width: visible.right - visible.left,
            height: visible.bottom - visible.top,
        });
    }

    let center = rect.center();
    let direction = if rect.left + rect.width < clip.left {
        Direction::Left
    } else if rect.left > clip.right {
        Direction::Right
    } else if rect.top + rect.height < clip.top {
        Direction::Up
    } else {
        Direction::Down
    };
    Anchor::Edge {
        x: clamp(center.x, clip.left + EDGE_INSET, clip.right - EDGE_INSET),
        y: clamp(center.y, clip.top + EDGE_INSET, clip.bottom - EDGE_INSET),
        direction,
    }
}

/// One registered anchor and where it went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlacedAnchor {
    pub id: String,
    #[serde(flatten)]
    pub anchor: Anchor,
}

/// Project a whole registered set in one call.
///
/// The set is what is on screen right now (the rings and rails), not every
/// object in the document, so this is O(registered) per camera write and
/// there is no frame loop anywhere.
pub fn project_all(
    camera: &Camera2d,
    origin: Point,
    anchors: &[(String, WorldBox)],
    clips: &[ClipRect],
) -> Vec<PlacedAnchor> {
    anchors
        .iter()
        .map(|(id, world)| PlacedAnchor {
            id: id.clone(),
            anchor: project(camera, origin, *world, clips),
        })
        .collect()
}

/// `clamp` that survives an inverted range. A clip narrower than twice the
/// inset puts `low` above `high`, and `f64::clamp` panics on that rather than
/// returning something drawable. A pane too narrow to inset into gets the
/// marker in its middle, which is inside it, which is the whole requirement.
fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if low > high {
        return (low + high) / 2.0;
    }
    value.max(low).min(high)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANE: ClipRect = ClipRect {
        left: 0.0,
        top: 0.0,
        right: 1000.0,
        bottom: 800.0,
    };

    fn world(x: f64, y: f64, w: f64, h: f64) -> WorldBox {
        WorldBox { x, y, w, h }
    }

    fn origin() -> Point {
        Point { x: 0.0, y: 0.0 }
    }

    #[test]
    fn a_visible_box_projects_through_the_camera() {
        let camera = Camera2d::new(100.0, 50.0, 0.5);
        let anchor = project(
            &camera,
            origin(),
            world(200.0, 100.0, 400.0, 200.0),
            &[PANE],
        );
        assert_eq!(
            anchor,
            Anchor::Visible(ScreenRect {
                left: 200.0,
                top: 100.0,
                width: 200.0,
                height: 100.0,
            })
        );
    }

    /// The regression this module exists for: the ring kept the pixels it was
    /// handed while the board moved under it.
    #[test]
    fn the_projection_follows_a_pan_and_a_zoom() {
        let mut camera = Camera2d::new(0.0, 0.0, 1.0);
        let box_ = world(300.0, 300.0, 100.0, 100.0);
        let before = project(&camera, origin(), box_, &[PANE]);
        camera.pan(-120.0, 40.0);
        let panned = project(&camera, origin(), box_, &[PANE]);
        assert_ne!(before, panned);
        let Anchor::Visible(rect) = panned else {
            panic!("still on screen")
        };
        assert_eq!(rect.left, 180.0);
        assert_eq!(rect.top, 340.0);

        camera.zoom_at(Point { x: 0.0, y: 0.0 }, 2.0);
        let Anchor::Visible(zoomed) = project(&camera, origin(), box_, &[PANE]) else {
            panic!("still on screen")
        };
        assert_eq!(zoomed.width, 200.0, "the ring has to grow with the card");
    }

    #[test]
    fn the_pane_origin_moves_the_ring_into_client_space() {
        let camera = Camera2d::default();
        let shifted = project(
            &camera,
            Point { x: 400.0, y: 60.0 },
            world(0.0, 0.0, 10.0, 10.0),
            &[],
        );
        assert_eq!(
            shifted,
            Anchor::Visible(ScreenRect {
                left: 400.0,
                top: 60.0,
                width: 10.0,
                height: 10.0,
            })
        );
    }

    /// A card half out of its pane rings the half that is on screen. Without
    /// the clip it draws a confident rectangle over the neighbouring column.
    #[test]
    fn a_partly_clipped_target_rings_only_what_is_visible() {
        let camera = Camera2d::default();
        let anchor = project(
            &camera,
            origin(),
            world(900.0, 100.0, 400.0, 100.0),
            &[PANE],
        );
        let Anchor::Visible(rect) = anchor else {
            panic!("part of it is on screen")
        };
        assert_eq!(rect.left, 900.0);
        assert_eq!(rect.width, 100.0, "clipped at the pane's right edge");
    }

    #[test]
    fn an_off_screen_target_degrades_to_an_edge_marker_with_a_direction() {
        let camera = Camera2d::default();
        let anchor = project(
            &camera,
            origin(),
            world(1400.0, 400.0, 100.0, 100.0),
            &[PANE],
        );
        let Anchor::Edge { x, y, direction } = anchor else {
            panic!("it is off the right edge")
        };
        assert_eq!(direction, Direction::Right);
        assert_eq!(direction.arrow(), "→");
        assert_eq!(x, PANE.right - EDGE_INSET, "pinned inside the pane");
        assert_eq!(y, 450.0, "still at the target's own height");
    }

    #[test]
    fn every_direction_is_reachable() {
        let camera = Camera2d::default();
        for (box_, expected) in [
            (world(-400.0, 400.0, 100.0, 100.0), Direction::Left),
            (world(1400.0, 400.0, 100.0, 100.0), Direction::Right),
            (world(400.0, -400.0, 100.0, 100.0), Direction::Up),
            (world(400.0, 1200.0, 100.0, 100.0), Direction::Down),
        ] {
            let Anchor::Edge { direction, .. } = project(&camera, origin(), box_, &[PANE]) else {
                panic!("{expected:?} case is off screen")
            };
            assert_eq!(direction, expected);
        }
    }

    #[test]
    fn the_edge_marker_stays_inside_a_pane_narrower_than_the_inset() {
        let camera = Camera2d::default();
        let sliver = ClipRect {
            left: 500.0,
            top: 0.0,
            right: 510.0,
            bottom: 800.0,
        };
        let Anchor::Edge { x, .. } =
            project(&camera, origin(), world(2000.0, 0.0, 10.0, 10.0), &[sliver])
        else {
            panic!("off screen")
        };
        assert!(
            (500.0..=510.0).contains(&x),
            "an inverted clamp range must not throw the marker out of the pane: {x}"
        );
    }

    #[test]
    fn two_clipping_ancestors_intersect() {
        let camera = Camera2d::default();
        let column = ClipRect {
            left: 200.0,
            top: 0.0,
            right: 600.0,
            bottom: 800.0,
        };
        let Anchor::Visible(rect) = project(
            &camera,
            origin(),
            world(100.0, 100.0, 900.0, 100.0),
            &[PANE, column],
        ) else {
            panic!("part of it is inside both")
        };
        assert_eq!(rect.left, 200.0);
        assert_eq!(rect.width, 400.0);
    }

    #[test]
    fn clips_that_do_not_overlap_leave_nowhere_to_draw() {
        let camera = Camera2d::default();
        let elsewhere = ClipRect {
            left: 2000.0,
            top: 0.0,
            right: 2400.0,
            bottom: 100.0,
        };
        assert_eq!(
            project(
                &camera,
                origin(),
                world(0.0, 0.0, 10.0, 10.0),
                &[PANE, elsewhere]
            ),
            Anchor::Nowhere
        );
    }

    #[test]
    fn an_unclipped_surface_draws_where_the_camera_says() {
        let camera = Camera2d::new(10.0, 10.0, 1.0);
        assert_eq!(
            project(&camera, origin(), world(0.0, 0.0, 40.0, 20.0), &[]),
            Anchor::Visible(ScreenRect {
                left: 10.0,
                top: 10.0,
                width: 40.0,
                height: 20.0,
            })
        );
    }

    #[test]
    fn a_target_with_no_geometry_has_nowhere_to_go() {
        let camera = Camera2d::default();
        assert_eq!(
            project(&camera, origin(), world(f64::NAN, 0.0, 10.0, 10.0), &[PANE]),
            Anchor::Nowhere
        );
    }

    #[test]
    fn a_zero_sized_target_still_projects_to_a_point_on_screen() {
        let camera = Camera2d::default();
        let anchor = project(&camera, origin(), world(500.0, 400.0, 0.0, 0.0), &[PANE]);
        // A zero-area rectangle cannot intersect anything, so it degrades
        // rather than silently vanishing. Down is the fallback direction.
        assert!(matches!(anchor, Anchor::Edge { .. }), "{anchor:?}");
    }

    #[test]
    fn a_whole_registered_set_projects_in_one_pass() {
        let camera = Camera2d::default();
        let placed = project_all(
            &camera,
            origin(),
            &[
                ("on".to_string(), world(100.0, 100.0, 50.0, 50.0)),
                ("off".to_string(), world(5000.0, 100.0, 50.0, 50.0)),
            ],
            &[PANE],
        );
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[0].id, "on");
        assert!(matches!(placed[0].anchor, Anchor::Visible(_)));
        assert!(matches!(placed[1].anchor, Anchor::Edge { .. }));
    }
}
