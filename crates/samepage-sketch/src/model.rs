//! Retained drawing geometry and interpretation; no document runtime.
use serde::Serialize;
pub const DEFAULT_STROKE_WIDTH: f64 = 2.0;
pub const OPACITY_RANGE: (f64, f64) = (0.0, 100.0);
pub const ENCLOSE_FRACTION: f64 = 0.75;
pub const GRAZE_FRACTION: f64 = 0.15;
pub const COVERAGE_GRID: usize = 4;
pub const CLOSURE_TOLERANCE: f64 = 0.34;
pub const STRIKE_SIZE_FACTOR: f64 = 1.5;
pub const POINT_SLOP: f64 = 36.0;
pub const RELATIONS: &[&str] = &[
    "encloses",
    "crosses-out",
    "points-at",
    "starts-at",
    "connects",
    "says",
    "free",
];
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Shape {
    pub id: String,
    /// One of [`FORMS`].
    pub form: String,
    /// Origin. For point-based forms the points are relative to it, so
    /// dragging a stroke is one property write rather than 200.
    pub x: f64,
    pub y: f64,
    /// Authored extent for `rect`/`ellipse`/`diamond`. Derived from the points
    /// for the others — see [`Shape::bounds`].
    pub w: f64,
    pub h: f64,
    /// Vertices relative to `(x, y)`. Empty for the box forms.
    pub points: Vec<(f64, f64)>,
    /// Node this arrow starts at, if it is bound to one. A bound endpoint is
    /// recomputed from the node's live position at render time, so the arrow
    /// follows the card the human drags instead of pointing at where it used
    /// to be. Excalidraw's `startBinding`/`endBinding`, minus the fixed-point
    /// modes we have no editor to author.
    pub from: String,
    pub to: String,
    /// One of [`HEADS`].
    pub head: String,
    /// Stroke colour, one of [`INKS`].
    pub ink: String,
    /// Fill colour, one of [`INKS`], or `none`.
    pub fill: String,
    /// Optional caption. On a box form that encloses nodes this is what names
    /// the group; on a `text` form it is the whole shape; on a `frame` it is
    /// the frame's name.
    pub label: String,
    /// Stroke width in world pixels. See [`STROKE_WIDTH_RANGE`].
    pub stroke_width: f64,
    /// One of [`STROKE_STYLES`].
    pub stroke_style: String,
    /// 0 to 100. See [`OPACITY_RANGE`].
    pub opacity: f64,
    /// One of [`ROUNDNESS`]. Meaningful for `rect`, `frame`, `line`, and
    /// `arrow`; carried on the others so an import round-trips.
    pub roundness: String,
    /// Font size for a `text` or a caption; 0 means not set. See
    /// [`FONT_SIZE_RANGE`].
    pub font_size: f64,
    /// Rotation in radians about the shape's centre, positive clockwise on
    /// screen (y grows down), the same sense Excalidraw uses. Only a sized
    /// form (`rect`, `ellipse`, `diamond`, `text`, `frame`) rotates; a stroke
    /// is rotated by rotating its points.
    pub angle: f64,
    /// Groups this shape belongs to, innermost last, as Excalidraw orders
    /// `groupIds`. A group is an id shared between shapes, not an object.
    pub groups: Vec<String>,
    /// Id of the `frame` shape this shape sits in, or empty. Excalidraw's
    /// `frameId`. A frame never sits in a frame.
    pub frame: String,
    /// Paint order. Explicit rather than emergent: "this stroke is on top of
    /// that one" is part of what the drawing means, and CRDT map iteration
    /// order is not a stable answer to it.
    pub z: f64,
    /// Whether `h` came from a browser or from [`estimate_text_height`].
    ///
    /// Only ever false for a `text`, which is the one form whose height nobody
    /// authors — it falls out of the words, exactly like a card's. Every other
    /// form is sized by what was drawn, so there is nothing to guess and this
    /// is true.
    pub measured: bool,
    pub created_by: String,
    pub touched_by: String,
}
#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
impl Node {
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.x + self.w, self.y + self.h)
    }
}
#[derive(Debug, Clone, Default)]
pub struct Drawing {
    pub nodes: Vec<Node>,
    pub shapes: Vec<Shape>,
}
impl Drawing {
    pub fn shape(&self, id: &str) -> Option<&Shape> {
        self.shapes.iter().find(|s| s.id == id)
    }
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|s| s.id == id)
    }
    pub fn binding_bounds(&self, id: &str) -> Option<(f64, f64, f64, f64)> {
        self.node(id)
            .map(Node::bounds)
            .or_else(|| self.shape(id).filter(|s| s.is_sized()).map(Shape::bounds))
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Coverage {
    Outside,
    Partial,
    Inside,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Reading {
    pub shape: String,
    pub relation: String,
    pub targets: Vec<String>,
    pub partial: Vec<String>,
}
pub fn readings(drawing: &Drawing) -> Vec<Reading> {
    drawing
        .shapes
        .iter()
        .map(|s| read_shape(drawing, s))
        .collect()
}
impl Shape {
    /// `(left, top, right, bottom)` in world units.
    ///
    /// For a rotated sized form this is the axis-aligned box around the
    /// rotated one, which is what every overlap and coverage pre-check wants.
    /// The exact region is what [`Shape::contains`] tests.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        if self.points.is_empty() {
            if self.angle == 0.0 {
                return (self.x, self.y, self.x + self.w, self.y + self.h);
            }
            let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
            let (sin, cos) = self.angle.sin_cos();
            return [
                (self.x, self.y),
                (self.x + self.w, self.y),
                (self.x, self.y + self.h),
                (self.x + self.w, self.y + self.h),
            ]
            .into_iter()
            .fold(
                (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                |(l, t, r, b), (px, py)| {
                    let (dx, dy) = (px - cx, py - cy);
                    let (rx, ry) = (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);
                    (l.min(rx), t.min(ry), r.max(rx), b.max(ry))
                },
            );
        }
        self.points.iter().fold(
            (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
            |(l, t, r, b), (px, py)| {
                let (px, py) = (self.x + px, self.y + py);
                (l.min(px), t.min(py), r.max(px), b.max(py))
            },
        )
    }

    /// The part of a shape's appearance a reader cannot infer from its form:
    /// everything that is not the default, in one parenthetical, or nothing.
    ///
    /// Reported because it is authored meaning. A dashed box says "tentative"
    /// and a faint one says "background"; a read-back that dropped them would
    /// hand the agent a different drawing from the one on screen.
    pub fn style_note(&self, atlas: &Drawing) -> String {
        let mut notes: Vec<String> = Vec::new();
        if self.stroke_style != "solid" {
            notes.push(self.stroke_style.clone());
        }
        if self.stroke_width != DEFAULT_STROKE_WIDTH {
            notes.push(if self.stroke_width < DEFAULT_STROKE_WIDTH {
                "thin".to_string()
            } else {
                format!("{:.0}px stroke", self.stroke_width)
            });
        }
        if self.opacity < OPACITY_RANGE.1 {
            notes.push(format!("{:.0}% opacity", self.opacity));
        }
        if self.angle != 0.0 {
            notes.push(format!("rotated {:.0}°", self.angle.to_degrees()));
        }
        if self.font_size > 0.0 && (self.form == "text" || !self.label.is_empty()) {
            notes.push(format!("{:.0}px type", self.font_size));
        }
        if let Some(group) = self.groups.last() {
            notes.push(format!("group {group}"));
        }
        if !self.frame.is_empty() {
            let name = atlas
                .shape(&self.frame)
                .map(|frame| frame.label.clone())
                .unwrap_or_default();
            notes.push(if name.is_empty() {
                format!("in frame {}", self.frame)
            } else {
                format!("in frame \"{name}\"")
            });
        }
        if self.form == "frame" {
            let held = atlas
                .shapes
                .iter()
                .filter(|other| other.frame == self.id)
                .count();
            if held > 0 {
                notes.push(format!("holding {held} drawn shape(s)"));
            }
        }
        if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        }
    }

    /// Vertices in world units.
    pub fn world_points(&self) -> Vec<(f64, f64)> {
        self.points
            .iter()
            .map(|(px, py)| (self.x + px, self.y + py))
            .collect()
    }

    fn center(&self) -> (f64, f64) {
        let (l, t, r, b) = self.bounds();
        ((l + r) / 2.0, (t + b) / 2.0)
    }

    pub fn is_box(&self) -> bool {
        matches!(self.form.as_str(), "rect" | "ellipse" | "diamond" | "frame")
    }

    /// Whether this form is authored with `w`/`h` rather than points.
    pub fn is_sized(&self) -> bool {
        self.is_box() || self.form == "text"
    }

    /// The unrotated box a sized form is authored as.
    fn local_box(&self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.x + self.w, self.y + self.h)
    }

    /// Bring a world point into the frame the shape was authored in, undoing
    /// its rotation about its own centre.
    fn to_local(&self, point: (f64, f64)) -> (f64, f64) {
        if self.angle == 0.0 || !self.is_sized() {
            return point;
        }
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        let (sin, cos) = (-self.angle).sin_cos();
        let (dx, dy) = (point.0 - cx, point.1 - cy);
        (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
    }

    /// Whether this shape bounds a region — something can be *inside* it.
    ///
    /// A box form always does. An ink stroke does only if the human brought it
    /// back around near where they started; an open squiggle encircles nothing,
    /// and reporting that it did would be the read-back inventing intent.
    pub fn is_closed(&self) -> bool {
        if self.is_box() {
            return true;
        }
        if self.form != "ink" || self.points.len() < 8 {
            return false;
        }
        let (first, last) = (self.points[0], self.points[self.points.len() - 1]);
        let gap = ((last.0 - first.0).powi(2) + (last.1 - first.1).powi(2)).sqrt();
        let (l, t, r, b) = self.bounds();
        let span = (r - l).max(b - t);
        span > 24.0 && gap < span * CLOSURE_TOLERANCE
    }

    /// How much of `node` this shape's region covers.
    ///
    /// Sampled over the card's own box rather than tested at its centre. The
    /// centre test called a card "inside" a loop that clipped one corner of
    /// it and equally called it inside a loop that missed everything but the
    /// middle — one number for two very different drawings. Coverage tells
    /// them apart, and [`Coverage::Partial`] is reported rather than rounded.
    pub fn covers(&self, node: &Node) -> Coverage {
        self.covers_bounds(node.bounds())
    }

    /// The same coverage measure over any box. Nodes are not the only thing a
    /// region can group: a loop around three sketches is the same gesture as a
    /// loop around three cards, and it is read with the same yardstick.
    pub fn covers_bounds(&self, (nl, nt, nr, nb): (f64, f64, f64, f64)) -> Coverage {
        let (sl, st, sr, sb) = self.bounds();
        // Bounds rejection first: nearly every (shape, node) pair fails here,
        // and the sampling below is 16 point-in-polygon tests against a stroke
        // that can carry 400 vertices.
        if nr < sl || nl > sr || nb < st || nt > sb {
            return Coverage::Outside;
        }
        let (width, height) = (nr - nl, nb - nt);
        if width <= 0.0 || height <= 0.0 {
            return Coverage::Outside;
        }
        let mut inside = 0usize;
        for row in 0..COVERAGE_GRID {
            for column in 0..COVERAGE_GRID {
                let at = |index: usize| (index as f64 + 0.5) / COVERAGE_GRID as f64;
                if self.contains((nl + width * at(column), nt + height * at(row))) {
                    inside += 1;
                }
            }
        }
        let fraction = inside as f64 / (COVERAGE_GRID * COVERAGE_GRID) as f64;
        if fraction >= ENCLOSE_FRACTION {
            Coverage::Inside
        } else if fraction >= GRAZE_FRACTION {
            Coverage::Partial
        } else {
            Coverage::Outside
        }
    }

    /// Does the world point fall inside this shape's region?
    fn contains(&self, point: (f64, f64)) -> bool {
        let point = self.to_local(point);
        let (l, t, r, b) = if self.is_sized() {
            self.local_box()
        } else {
            self.bounds()
        };
        match self.form.as_str() {
            "rect" | "frame" => point.0 >= l && point.0 <= r && point.1 >= t && point.1 <= b,
            "ellipse" => {
                let (rx, ry) = ((r - l) / 2.0, (b - t) / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
                ((point.0 - cx) / rx).powi(2) + ((point.1 - cy) / ry).powi(2) <= 1.0
            }
            "diamond" => {
                let (rx, ry) = ((r - l) / 2.0, (b - t) / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
                ((point.0 - cx) / rx).abs() + ((point.1 - cy) / ry).abs() <= 1.0
            }
            _ => point_in_polygon(point, &self.world_points()),
        }
    }
}

impl Drawing {
    /// Where a line or arrow actually starts and ends, in world units.
    ///
    /// A bound endpoint is resolved here rather than stored, so an arrow keeps
    /// touching a card or drawing shape after the human drags it somewhere
    /// else. It stops at the edge of the target facing the other end.
    pub fn shape_endpoints(&self, shape: &Shape) -> Option<((f64, f64), (f64, f64))> {
        if !matches!(shape.form.as_str(), "line" | "arrow") {
            return None;
        }
        let world = shape.world_points();
        let bound = |id: &str| {
            self.binding_bounds(id)
                .map(|(l, t, r, b)| ((l + r) / 2.0, (t + b) / 2.0))
        };
        let start = bound(&shape.from).or_else(|| world.first().copied())?;
        let end = bound(&shape.to).or_else(|| world.last().copied())?;
        let clip = |point: (f64, f64), toward: (f64, f64), id: &str| match self.binding_bounds(id) {
            Some(bounds) => box_edge_toward(bounds, point, toward),
            None => point,
        };
        Some((clip(start, end, &shape.from), clip(end, start, &shape.to)))
    }

    /// The line or arrow as painted in world coordinates. Intermediate points
    /// survive bindings, so a curved Excalidraw connector does not become a
    /// straight line merely because its ends follow two boxes.
    pub fn shape_path(&self, shape: &Shape) -> Option<Vec<(f64, f64)>> {
        let (start, end) = self.shape_endpoints(shape)?;
        let mut path = shape.world_points();
        if path.len() < 2 {
            return Some(vec![start, end]);
        }
        if let Some(first) = path.first_mut() {
            *first = start;
        }
        if let Some(last) = path.last_mut() {
            *last = end;
        }
        Some(path)
    }
}

/// Walk from a box's centre toward `toward` and stop on the box's edge.
fn box_edge_toward(
    (l, t, r, b): (f64, f64, f64, f64),
    center: (f64, f64),
    toward: (f64, f64),
) -> (f64, f64) {
    let (dx, dy) = (toward.0 - center.0, toward.1 - center.1);
    if dx.abs() < f64::EPSILON && dy.abs() < f64::EPSILON {
        return center;
    }
    let scale_x = if dx.abs() < f64::EPSILON {
        f64::MAX
    } else {
        ((if dx > 0.0 { r } else { l }) - center.0) / dx
    };
    let scale_y = if dy.abs() < f64::EPSILON {
        f64::MAX
    } else {
        ((if dy > 0.0 { b } else { t }) - center.1) / dy
    };
    let scale = scale_x.min(scale_y).clamp(0.0, 1.0);
    (center.0 + dx * scale, center.1 + dy * scale)
}

pub fn simplify(points: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (first, last) = (points[0], points[points.len() - 1]);
    let mut worst = (0usize, 0.0f64);
    for (index, point) in points.iter().enumerate().take(points.len() - 1).skip(1) {
        let distance = point_to_segment(*point, first, last);
        if distance > worst.1 {
            worst = (index, distance);
        }
    }
    if worst.1 <= tolerance {
        return vec![first, last];
    }
    let mut out = simplify(&points[..=worst.0], tolerance);
    out.pop();
    out.extend(simplify(&points[worst.0..], tolerance));
    out
}

fn point_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    if length < f64::EPSILON {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0);
    ((p.0 - (a.0 + t * dx)).powi(2) + (p.1 - (a.1 + t * dy)).powi(2)).sqrt()
}

/// Turn a stroke's centre line into the outline polygon that gets filled.
///
/// A constant-width polyline reads as a wire; a stroke that thins where the
/// hand moved fast reads as ink, and the difference is most of why a drawn
/// annotation looks like a person made it. The approach is Steve Ruiz's
/// perfect-freehand (MIT) — streamline the input, derive a per-point radius
/// from local speed, then walk both offset sides and cap the ends.
///
/// It lives here, in the crate both replicas compile, for the same reason
/// every other rule does: the browser draws this outline and the host reasons
/// about the shape, and those must not be two different opinions of where the
/// ink is. Porting ~90 lines was cheaper than a dependency in the one crate
/// that defines what the shared document means.
pub fn outline(points: &[(f64, f64)], size: f64) -> Vec<(f64, f64)> {
    if points.len() < 2 {
        return Vec::new();
    }
    let radius = (size / 2.0).max(0.5);

    // Streamline: pull each sample toward the previous one. Pointer input is
    // jittery at the pixel level and the jitter is very visible once the edge
    // is a filled boundary rather than a stroked line.
    let mut path: Vec<(f64, f64)> = vec![points[0]];
    for point in &points[1..] {
        let previous = *path.last().unwrap();
        path.push((
            previous.0 + (point.0 - previous.0) * 0.6,
            previous.1 + (point.1 - previous.1) * 0.6,
        ));
    }

    // Per-point radius from local speed, smoothed so the width does not
    // flicker between adjacent samples.
    let mut radii = Vec::with_capacity(path.len());
    for index in 0..path.len() {
        let previous = path[index.saturating_sub(1)];
        let current = path[index];
        let speed = ((current.0 - previous.0).powi(2) + (current.1 - previous.1).powi(2)).sqrt();
        let thinning = (speed / 14.0).clamp(0.0, 1.0);
        radii.push(radius * (1.0 - 0.45 * thinning));
    }
    let mut carried = radii[0];
    for radius in radii.iter_mut().skip(1) {
        *radius = *radius * 0.5 + carried * 0.5;
        carried = *radius;
    }
    // Taper the very start so a stroke begins as a point, not a stub.
    let taper = radii.len().min(4);
    for (index, radius) in radii.iter_mut().take(taper).enumerate() {
        *radius *= 0.45 + 0.55 * (index as f64 / taper as f64);
    }

    let (mut left, mut right) = (Vec::new(), Vec::new());
    for index in 0..path.len() {
        let ahead = path[(index + 1).min(path.len() - 1)];
        let behind = path[index.saturating_sub(1)];
        let (dx, dy) = (ahead.0 - behind.0, ahead.1 - behind.1);
        let length = (dx * dx + dy * dy).sqrt();
        if length < f64::EPSILON {
            continue;
        }
        let (nx, ny) = (-dy / length, dx / length);
        let r = radii[index];
        left.push((path[index].0 + nx * r, path[index].1 + ny * r));
        right.push((path[index].0 - nx * r, path[index].1 - ny * r));
    }
    if left.is_empty() {
        return Vec::new();
    }

    // Round caps, so the ends are ink rather than cut-off ribbon.
    let mut polygon = left;
    polygon.extend(cap(
        *path.last().unwrap(),
        *polygon.last().unwrap(),
        *right.last().unwrap(),
    ));
    polygon.extend(right.into_iter().rev());
    let start_cap = cap(path[0], *polygon.last().unwrap(), polygon[0]);
    polygon.extend(start_cap);
    polygon
}

/// Arc from `a` to `b` around `center`, the short way.
fn cap(center: (f64, f64), a: (f64, f64), b: (f64, f64)) -> Vec<(f64, f64)> {
    let start = (a.1 - center.1).atan2(a.0 - center.0);
    let end = (b.1 - center.1).atan2(b.0 - center.0);
    let radius = ((a.0 - center.0).powi(2) + (a.1 - center.1).powi(2)).sqrt();
    let mut sweep = end - start;
    while sweep <= 0.0 {
        sweep += std::f64::consts::TAU;
    }
    let steps = 8;
    (1..steps)
        .map(|step| {
            let angle = start + sweep * (step as f64 / steps as f64);
            (
                center.0 + angle.cos() * radius,
                center.1 + angle.sin() * radius,
            )
        })
        .collect()
}

fn is_matter(shape: &Shape) -> bool {
    !matches!(shape.form.as_str(), "arrow" | "line")
}

/// Whether `other` is drawn matter `shape` may be read against.
///
/// A shape's own frame is excluded. A frame is the panel a shape was drawn
/// in, and every member sits inside it by construction, so reading a member
/// against it would make every arrow in a panel "point at" the panel and
/// every line in one "cross it out". A stroke that is not a member is read
/// against a frame like any other box: striking through a whole panel is a
/// real gesture.
fn reads_against(shape: &Shape, other: &Shape) -> bool {
    other.id != shape.id && is_matter(other) && other.id != shape.frame
}

fn read_shape(atlas: &Drawing, shape: &Shape) -> Reading {
    let reading = |relation: &str, targets: Vec<String>, partial: Vec<String>| {
        assert!(
            RELATIONS.contains(&relation),
            "unknown reading relation {relation:?}; expected one of {}",
            RELATIONS.join(", ")
        );
        Reading {
            shape: shape.id.clone(),
            relation: relation.to_string(),
            targets,
            partial,
        }
    };

    // A text says what it says. Its box can also establish proximity, but not
    // a stored link: each card supplies its own threshold so a nearby caption
    // is judged relative to the thing it may name.
    if shape.form == "text" {
        let near = |bounds: (f64, f64, f64, f64)| {
            let threshold = (bounds.2 - bounds.0).hypot(bounds.3 - bounds.1) / 2.0;
            distance_between_boxes(shape.bounds(), bounds) <= threshold
        };
        let nearby: Vec<String> = atlas
            .nodes
            .iter()
            .filter(|node| near(node.bounds()))
            .map(|node| node.id.clone())
            .collect();
        if !nearby.is_empty() {
            return reading("says", nearby, Vec::new());
        }
        // No card is near, but the drawn layer can still anchor the words: a
        // caption written beside a sketch is labelling that sketch, and
        // reporting it as touching nothing reads the label out of the picture.
        // Cards keep priority — ink is only consulted once no card claims it.
        let nearby_matter: Vec<String> = atlas
            .shapes
            .iter()
            .filter(|other| reads_against(shape, other))
            .filter(|other| near(other.bounds()))
            .map(|other| other.id.clone())
            .collect();
        return reading("says", nearby_matter, Vec::new());
    }

    // An arrow the human drew between two cards is a relationship, and the
    // fact that it is bound means they said so explicitly.
    if !shape.from.is_empty() && !shape.to.is_empty() {
        return reading(
            "connects",
            vec![shape.from.clone(), shape.to.clone()],
            Vec::new(),
        );
    }
    // One end on a card and the other in open space. Rarer, and it used to
    // fall through to "touches no node" — which is false twice over: the arrow
    // is attached to that card, and saying otherwise then got it reported as
    // covering the card up for no reason. What it actually says is "this leads
    // somewhere that is not on the map yet", so that is what it says.
    if !shape.to.is_empty() {
        return reading("points-at", vec![shape.to.clone()], Vec::new());
    }
    if !shape.from.is_empty() {
        return reading("starts-at", vec![shape.from.clone()], Vec::new());
    }

    if shape.is_closed() {
        let (mut inside, partial) = {
            let (mut inside, mut partial) = (Vec::new(), Vec::new());
            for node in &atlas.nodes {
                match shape.covers(node) {
                    Coverage::Inside => inside.push(node.id.clone()),
                    Coverage::Partial => partial.push(node.id.clone()),
                    Coverage::Outside => {}
                }
            }
            (inside, partial)
        };
        // Drawn matter fully inside the region is grouped alongside the cards.
        // This sentence going missing was the register split's daily cost: a
        // loop around three sketches read back as touching nothing. Cards are
        // listed first; the area guard keeps two same-sized boxes from each
        // claiming to enclose the other.
        let own = shape.bounds();
        let own_area = (own.2 - own.0) * (own.3 - own.1);
        let matter_inside: Vec<String> = atlas
            .shapes
            .iter()
            .filter(|other| reads_against(shape, other))
            .filter(|other| {
                let bounds = other.bounds();
                (bounds.2 - bounds.0) * (bounds.3 - bounds.1) < own_area
            })
            .filter(|other| shape.covers_bounds(other.bounds()) == Coverage::Inside)
            .map(|other| other.id.clone())
            .collect();
        if !inside.is_empty() || !matter_inside.is_empty() {
            inside.extend(matter_inside);
            return reading("encloses", inside, partial);
        }
        // Nothing is fully in, but something is half in. That is a drawing
        // whose meaning is genuinely unclear, and saying so beats guessing.
        if !partial.is_empty() {
            return reading("free", Vec::new(), partial);
        }
    }

    if !shape.points.is_empty() {
        let world = shape.world_points();
        let (l, t, r, b) = shape.bounds();
        let (span_x, span_y) = (r - l, b - t);
        let hit: Vec<&Node> = atlas
            .nodes
            .iter()
            .filter(|node| polyline_hits_box(&world, node.bounds()))
            .collect();

        // Scribbled over exactly one card, and no bigger than it: struck out.
        if hit.len() == 1 {
            let node = hit[0];
            let (nl, nt, nr, nb) = node.bounds();
            let center = shape.center();
            let inside = center.0 >= nl && center.0 <= nr && center.1 >= nt && center.1 <= nb;
            let contained = span_x <= (nr - nl) * STRIKE_SIZE_FACTOR
                && span_y <= (nb - nt) * STRIKE_SIZE_FACTOR;
            if inside && contained {
                return reading("crosses-out", vec![node.id.clone()], Vec::new());
            }
        }

        // The same strike gesture over the drawn layer: a scribble across one
        // sketch rejects that sketch, and it deserves the same sentence a
        // struck card gets. Only consulted when no card was hit at all, so the
        // card register keeps priority.
        if hit.is_empty() {
            let struck: Vec<&Shape> = atlas
                .shapes
                .iter()
                .filter(|other| reads_against(shape, other))
                .filter(|other| polyline_hits_box(&world, other.bounds()))
                .collect();
            if struck.len() == 1 {
                let target = struck[0];
                let (tl, tt, tr, tb) = target.bounds();
                let center = shape.center();
                let inside = center.0 >= tl && center.0 <= tr && center.1 >= tt && center.1 <= tb;
                let contained = span_x <= (tr - tl) * STRIKE_SIZE_FACTOR
                    && span_y <= (tb - tt) * STRIKE_SIZE_FACTOR;
                if inside && contained {
                    return reading("crosses-out", vec![target.id.clone()], Vec::new());
                }
            }
        }

        // An arrow or line with a head is aimed at whatever its tip reaches.
        if matches!(shape.form.as_str(), "arrow" | "line")
            && let Some(tip) = world.last()
        {
            let aimed = atlas
                .nodes
                .iter()
                .filter(|node| !shape.from.eq(&node.id))
                .map(|node| (node, distance_to_box(*tip, node.bounds())))
                .filter(|(_, distance)| *distance <= POINT_SLOP)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((node, _)) = aimed {
                return reading("points-at", vec![node.id.clone()], Vec::new());
            }
            // No card in reach of the tip: the nearest drawn matter is
            // what the arrow is aimed at, by the same yardstick.
            let aimed_matter = atlas
                .shapes
                .iter()
                .filter(|other| reads_against(shape, other))
                .filter(|other| !shape.from.eq(&other.id))
                .map(|other| (other, distance_to_box(*tip, other.bounds())))
                .filter(|(_, distance)| *distance <= POINT_SLOP)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((other, _)) = aimed_matter {
                return reading("points-at", vec![other.id.clone()], Vec::new());
            }
        }
    }

    // A box that touches nothing is still worth reporting as a region.
    reading("free", Vec::new(), Vec::new())
}

/// Do two boxes overlap by more than `slop` on both axes?
pub fn overlaps(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64), slop: f64) -> bool {
    (a.2.min(b.2) - a.0.max(b.0)) > slop && (a.3.min(b.3) - a.1.max(b.1)) > slop
}

/// Distance from a point to the nearest edge of a box; zero when inside.
fn distance_to_box(p: (f64, f64), (l, t, r, b): (f64, f64, f64, f64)) -> f64 {
    let dx = (l - p.0).max(0.0).max(p.0 - r);
    let dy = (t - p.1).max(0.0).max(p.1 - b);
    (dx * dx + dy * dy).sqrt()
}

/// Distance between the nearest edges of two boxes; zero when they overlap.
fn distance_between_boxes(
    (al, at, ar, ab): (f64, f64, f64, f64),
    (bl, bt, br, bb): (f64, f64, f64, f64),
) -> f64 {
    let dx = (bl - ar).max(0.0).max(al - br);
    let dy = (bt - ab).max(0.0).max(at - bb);
    (dx * dx + dy * dy).sqrt()
}

/// Does any segment of the polyline cut through the box?
fn polyline_hits_box(points: &[(f64, f64)], bounds: (f64, f64, f64, f64)) -> bool {
    points
        .windows(2)
        .any(|pair| segment_hits_box(pair[0], pair[1], bounds))
}

/// Even-odd ray casting. `poly` is treated as closed.
fn point_in_polygon(p: (f64, f64), poly: &[(f64, f64)]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > p.1) != (yj > p.1) && p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn segment_hits_box(a: (f64, f64), b: (f64, f64), (l, t, r, bottom): (f64, f64, f64, f64)) -> bool {
    // Liang–Barsky: clip the segment against each slab, and if any run of the
    // parameter survives all four the line passes through the rectangle.
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut enter, mut exit) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.0 - l),
        (dx, r - a.0),
        (-dy, a.1 - t),
        (dy, bottom - a.1),
    ] {
        if p.abs() < f64::EPSILON {
            if q < 0.0 {
                return false; // parallel to this slab and outside it
            }
            continue;
        }
        let ratio = q / p;
        if p < 0.0 {
            enter = enter.max(ratio);
        } else {
            exit = exit.min(ratio);
        }
        if enter > exit {
            return false;
        }
    }
    true
}
