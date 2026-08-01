//! Structure-driven diagram layout: the agent hands over *nodes + edges* and
//! the host computes a clean 2D placement.
//!
//! The rule this module exists to enforce is the same one the canvas read-back
//! follows — **semantics in, coordinates out**. A model asked to place boxes
//! itself does it badly (overlaps, off-frame, wrong scale) and then burns a
//! slow read → clear → redraw loop fixing it. Asked instead for the structure
//! it actually knows, it gets a correct picture on the first call.
//!
//! The geometry is the "brain" ported from agentviz's `particle_dag` layout
//! (Kahn longest-path rank → strata), flattened from 3D to the plane and given
//! box sizing so labels fit their nodes. It was first written for
//! teaching-canvas and lives here because three surfaces now want it:
//!
//! - **teaching-canvas** draws it as scene objects in a ±10 world frame.
//! - **same-page-room** renders it as an SVG pane node.
//! - **same-page-atlas** emits it as editable shapes that export to
//!   `.excalidraw`.
//!
//! Those three disagree about units, so [`layout`] deliberately does *not*
//! fit its output to a frame: it returns natural, label-derived units and a
//! [`Layout::bounds`], and the caller decides. A surface with a fixed world
//! frame calls [`Layout::fit`]; one that can frame itself to content (SVG
//! `viewBox`, an atlas placement origin) just scales the bounds it is given.
//!
//! Two halves, usable independently:
//!
//! - [`Spec`] parses and *fully validates* the agent's JSON before anything is
//!   mutated, so a typo is an error rather than half a diagram.
//! - [`layout`] is pure geometry over labels and index pairs — no JSON, no
//!   scene, no serde — so it unit-tests in isolation.
//!
//! Colours stay `String` here on purpose. Each surface has its own colour
//! vocabulary (teaching-canvas parses hex to `u32`, the atlas maps onto its
//! named `INKS`), and the one thing this module must not do is pick for them.

use std::collections::VecDeque;

use ag_ui_core::JsonValue;

/// Which way ranks flow. `Right` = roots on the left, edges point right (reads
/// like a pipeline / computation graph). `Down` = roots on top, edges point down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Right,
    Down,
}

impl Direction {
    pub fn parse(s: &str) -> Direction {
        match s.trim().to_ascii_lowercase().as_str() {
            "down" | "vertical" | "top-down" | "td" => Direction::Down,
            _ => Direction::Right,
        }
    }
}

/// One placed node, in whatever units the caller ends up scaling to.
///
/// Two sizes, because two things want different answers. `half` is the square
/// extent the layout *reserves* — spacing and overlap are computed from it, so
/// a surface that draws square objects (teaching-canvas) can use it directly.
/// `w`/`h` are what the label actually needs: a one-line label is far wider
/// than it is tall, and drawing it in its reserved square leaves a mostly
/// empty box. A surface that can draw a rectangle should use `w`/`h` and get
/// the extra room as breathing space rather than as dead pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    /// Half the reserved square. Spacing and overlap-freedom are stated in
    /// terms of this, so `w <= 2*half` and `h <= 2*half` always hold.
    pub half: f64,
    /// Width the label needs.
    pub w: f64,
    /// Height the label needs.
    pub h: f64,
}

impl Placed {
    /// `(left, top, right, bottom)` of the drawn rectangle.
    pub fn rect(&self) -> (f64, f64, f64, f64) {
        (
            self.x - self.w / 2.0,
            self.y - self.h / 2.0,
            self.x + self.w / 2.0,
            self.y + self.h / 2.0,
        )
    }

    /// The point where the segment to `other` crosses this box's edge.
    ///
    /// Shared rather than re-derived per surface because getting it wrong is
    /// invisible in a unit test and obvious on screen: trimming to a circle of
    /// radius `half` (the easy version) leaves the arrowhead floating in space
    /// beside a wide box, and not trimming at all buries it under the box.
    pub fn edge_toward(&self, other: &Placed) -> (f64, f64) {
        let (dx, dy) = (other.x - self.x, other.y - self.y);
        if dx == 0.0 && dy == 0.0 {
            return (self.x, self.y);
        }
        // Largest t with |t*dx| <= w/2 and |t*dy| <= h/2 — the ray's exit point.
        let t_x = if dx == 0.0 {
            f64::INFINITY
        } else {
            (self.w / 2.0) / dx.abs()
        };
        let t_y = if dy == 0.0 {
            f64::INFINITY
        } else {
            (self.h / 2.0) / dy.abs()
        };
        let t = t_x.min(t_y);
        (self.x + dx * t, self.y + dy * t)
    }
}

/// The computed placement: a position per input node (same order/indices as the
/// labels passed in), plus the text size that fits those boxes.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub nodes: Vec<Placed>,
    pub text_size: f64,
}

// Tuning, in the natural units [`layout`] works in. Boxes are sized to their
// label at BASE_TEXT; callers scale the whole result afterwards.
const BASE_TEXT: f64 = 0.62; // label height
const MONO_ADVANCE: f64 = 0.6; // pretext fixed-width glyph advance
const PAD: f64 = 0.42; // node padding around its label
const MIN_HALF: f64 = 0.72;
const GAP_PRIMARY: f64 = 1.7; // gap between rank layers
/// Rank gap when any edge carries a label. A label is drawn at the midpoint of
/// the trimmed segment between two boxes, and at the default gap it is
/// routinely wider than the space it has — so it overhangs both boxes. Ranks
/// that have something written between them simply need more room.
const GAP_PRIMARY_LABELLED: f64 = 3.6;
const GAP_CROSS: f64 = 1.15; // gap between siblings in a layer

impl Layout {
    /// `(left, top, right, bottom)` covering every box. Empty layouts are a
    /// zero-extent box at the origin rather than an inverted one, so callers
    /// mapping this onto an SVG `viewBox` never produce a negative dimension.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        if self.nodes.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        self.nodes.iter().fold(
            (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
            |(l, t, r, b), p| {
                (
                    l.min(p.x - p.half),
                    t.min(p.y - p.half),
                    r.max(p.x + p.half),
                    b.max(p.y + p.half),
                )
            },
        )
    }

    /// Uniformly scale positions, boxes and text together so nothing leaves a
    /// `±extent` frame. Proportions are preserved, so labels still fit their
    /// boxes. Only ever shrinks: a diagram smaller than the frame is left at
    /// its natural size rather than blown up to fill it.
    pub fn fit(mut self, extent: f64) -> Layout {
        let mut widest = 0.0f64;
        for p in &self.nodes {
            widest = widest.max(p.x.abs() + p.half).max(p.y.abs() + p.half);
        }
        if widest <= extent || widest <= 0.0 || extent <= 0.0 {
            return self;
        }
        let k = extent / widest;
        for p in &mut self.nodes {
            p.x *= k;
            p.y *= k;
            p.half *= k;
            p.w *= k;
            p.h *= k;
        }
        self.text_size *= k;
        self
    }

    /// Scale by `k` and move the whole diagram so its top-left bound sits at
    /// `(origin_x, origin_y)`. This is the mapping a surface with a
    /// top-left-origin pixel space (the atlas, an SVG) wants, as opposed to
    /// the centred world frame [`fit`](Layout::fit) serves.
    pub fn place(mut self, k: f64, origin_x: f64, origin_y: f64) -> Layout {
        for p in &mut self.nodes {
            p.x *= k;
            p.y *= k;
            p.half *= k;
            p.w *= k;
            p.h *= k;
        }
        self.text_size *= k;
        let (left, top, _, _) = self.bounds();
        let (dx, dy) = (origin_x - left, origin_y - top);
        for p in &mut self.nodes {
            p.x += dx;
            p.y += dy;
        }
        self
    }
}

/// Square half-extent that fits `label` at `text` height (width usually dominates).
fn node_half(label: &str, text: f64) -> f64 {
    let chars = label.chars().count().max(1) as f64;
    let half_w = chars * text * MONO_ADVANCE * 0.5 + PAD;
    let half_h = text * 0.5 + PAD;
    // Do not cap this before the caller's fit pass. A cap makes long labels
    // escape their boxes; uniform fitting keeps the full diagram in frame
    // while preserving the label-to-box ratio.
    half_w.max(half_h).max(MIN_HALF)
}

/// Width and height a `label` needs at `text` height, padded. Never wider or
/// taller than the square [`node_half`] reserves for the same label, which is
/// what keeps a rectangle drawn inside that square overlap-free.
fn label_size(label: &str, text: f64) -> (f64, f64) {
    let chars = label.chars().count().max(1) as f64;
    let width = chars * text * MONO_ADVANCE + PAD * 2.0;
    let height = text + PAD * 2.0;
    let square = node_half(label, text) * 2.0;
    (width.min(square), height.min(square))
}

/// Longest-path rank per node via Kahn's algorithm (roots = rank 0). Cycle-safe:
/// self-loops are ignored, and if back-edges leave nodes unprocessed we still
/// give every node a best-effort rank so the layout never panics or drops nodes.
fn ranks(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for &(u, v) in edges {
        if u != v && u < n && v < n {
            if let (Some(neighbors), Some(degree)) = (adj.get_mut(u), indeg.get_mut(v)) {
                neighbors.push(v);
                *degree += 1;
            }
        }
    }
    let mut rank = vec![0usize; n];
    let mut work = indeg.clone();
    let mut q: VecDeque<usize> = indeg
        .iter()
        .enumerate()
        .filter_map(|(index, &degree)| (degree == 0).then_some(index))
        .collect();
    let mut processed = 0usize;
    while let Some(u) = q.pop_front() {
        processed += 1;
        let Some(base_rank) = rank.get(u).copied() else {
            continue;
        };
        let Some(neighbors) = adj.get(u) else {
            continue;
        };
        for &v in neighbors {
            if let Some(v_rank) = rank.get_mut(v) {
                *v_rank = (*v_rank).max(base_rank + 1);
            }
            if let Some(degree) = work.get_mut(v) {
                *degree = degree.saturating_sub(1);
                if *degree == 0 {
                    q.push_back(v);
                }
            }
        }
    }
    // Cyclic remainder: one relaxation pass so cycle members still stratify.
    if processed < n {
        for _ in 0..n {
            let mut changed = false;
            for &(u, v) in edges {
                if u == v || u >= n || v >= n {
                    continue;
                }
                let Some(candidate) = rank.get(u).copied().map(|value| value + 1) else {
                    continue;
                };
                let Some(v_rank) = rank.get_mut(v) else {
                    continue;
                };
                if candidate > *v_rank {
                    *v_rank = candidate;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
    rank
}

/// Compute a clean layered layout for `labels` connected by `edges` (index pairs),
/// in natural label-derived units centred on the origin.
///
/// The result is deliberately unfitted — see the module docs. Call
/// [`Layout::fit`] for a centred world frame or [`Layout::place`] for a
/// top-left pixel space.
pub fn layout(labels: &[String], edges: &[(usize, usize)], dir: Direction) -> Layout {
    layout_with(labels, edges, dir, GAP_PRIMARY)
}

/// [`layout`], with the gap between rank layers chosen by the caller. Used by
/// [`Spec::layout`] to widen the gap when edges carry labels.
pub fn layout_with(
    labels: &[String],
    edges: &[(usize, usize)],
    dir: Direction,
    gap_primary: f64,
) -> Layout {
    let n = labels.len();
    if n == 0 {
        return Layout {
            nodes: Vec::new(),
            text_size: BASE_TEXT,
        };
    }

    let rank = ranks(n, edges);
    let max_rank = *rank.iter().max().unwrap_or(&0);

    // Group node indices into layers by rank, preserving input order within a layer.
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for (i, &r) in rank.iter().enumerate() {
        if let Some(layer) = layers.get_mut(r) {
            layer.push(i);
        }
    }

    // One barycenter sweep: order each layer by the mean cross-slot of its parents
    // in the previous layer, which markedly cuts edge crossings for typical DAGs.
    let mut slot_of = vec![0usize; n]; // provisional slot index within its layer
    for layer in &layers {
        for (s, &idx) in layer.iter().enumerate() {
            if let Some(slot) = slot_of.get_mut(idx) {
                *slot = s;
            }
        }
    }
    let mut prev = layers.first().cloned().unwrap_or_default();
    for layer in layers.iter_mut().skip(1) {
        let prev_slot = |node: usize| -> Option<usize> { prev.iter().position(|&p| p == node) };
        let mut keyed: Vec<(f64, usize)> = layer
            .iter()
            .map(|&idx| {
                let parents: Vec<f64> = edges
                    .iter()
                    .filter(|&&(_, v)| v == idx)
                    .filter_map(|&(u, _)| prev_slot(u).map(|s| s as f64))
                    .collect();
                let bary = if parents.is_empty() {
                    slot_of.get(idx).copied().unwrap_or_default() as f64
                } else {
                    parents.iter().sum::<f64>() / parents.len() as f64
                };
                (bary, idx)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        *layer = keyed.into_iter().map(|(_, idx)| idx).collect();
        for (s, &idx) in layer.iter().enumerate() {
            if let Some(slot) = slot_of.get_mut(idx) {
                *slot = s;
            }
        }
        prev = layer.clone();
    }

    let halves: Vec<f64> = labels.iter().map(|l| node_half(l, BASE_TEXT)).collect();

    // Primary axis: cumulative layer centers spaced by neighbouring layers' sizes.
    let layer_max_half: Vec<f64> = layers
        .iter()
        .map(|layer| {
            layer
                .iter()
                .filter_map(|&i| halves.get(i).copied())
                .fold(MIN_HALF, f64::max)
        })
        .collect();
    let mut prim_center = vec![0.0f64; layers.len()];
    for (index, pair) in layer_max_half.windows(2).enumerate() {
        let [previous_half, current_half] = pair else {
            continue;
        };
        let previous_center = prim_center.get(index).copied().unwrap_or_default();
        if let Some(current_center) = prim_center.get_mut(index + 1) {
            *current_center = previous_center + previous_half + gap_primary + current_half;
        }
    }
    let prim_mid = prim_center.last().copied().unwrap_or(0.0) / 2.0;

    // Cross axis: within each layer, lay siblings out symmetrically around 0.
    let mut nodes = vec![
        Placed {
            x: 0.0,
            y: 0.0,
            half: 0.0,
            w: 0.0,
            h: 0.0,
        };
        n
    ];
    for (r, layer) in layers.iter().enumerate() {
        // total cross span of this layer
        let mut cross_pos = vec![0.0f64; layer.len()];
        let mut acc = 0.0;
        let mut previous = None;
        for (&idx, position) in layer.iter().zip(&mut cross_pos) {
            if let Some(previous_idx) = previous {
                let (Some(previous_half), Some(current_half)) =
                    (halves.get(previous_idx), halves.get(idx))
                else {
                    continue;
                };
                acc += previous_half + GAP_CROSS + current_half;
            }
            *position = acc;
            previous = Some(idx);
        }
        let cross_mid = acc / 2.0;
        let primary_center = prim_center.get(r).copied().unwrap_or_default();
        for (&idx, &position) in layer.iter().zip(&cross_pos) {
            let Some(half) = halves.get(idx).copied() else {
                continue;
            };
            let primary = primary_center - prim_mid;
            let cross = position - cross_mid;
            let (x, y) = match dir {
                // roots left, flow right; cross runs top→bottom so slot 0 is highest
                Direction::Right => (primary, -cross),
                // roots top, flow down
                Direction::Down => (cross, prim_mid - primary_center),
            };
            if let Some(node) = nodes.get_mut(idx) {
                let (w, h) = labels
                    .get(idx)
                    .map(|label| label_size(label, BASE_TEXT))
                    .unwrap_or((half * 2.0, half * 2.0));
                *node = Placed { x, y, half, w, h };
            }
        }
    }

    Layout {
        nodes,
        text_size: BASE_TEXT,
    }
}

// ── The agent-facing spec ───────────────────────────────────────────────────

/// One node as the agent described it. `color` is whatever string the agent
/// wrote; resolving it is the surface's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecNode {
    pub id: String,
    pub label: String,
    pub color: Option<String>,
    pub group: Option<String>,
}

/// One edge, with endpoints already resolved to indices into [`Spec::nodes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecEdge {
    pub from: usize,
    pub to: usize,
    pub arrow: bool,
    pub label: Option<String>,
}

/// A validated diagram request.
///
/// Every surface shares this parse so they agree on what a diagram *is* and
/// report the same errors for the same mistakes. Parsing is total: by the time
/// you hold a `Spec`, ids are unique, every edge endpoint names a real node,
/// and nothing has been drawn yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub nodes: Vec<SpecNode>,
    pub edges: Vec<SpecEdge>,
    pub direction: Direction,
    pub title: Option<String>,
    /// Whether this diagram replaces what is on the surface (default) or draws
    /// over it.
    pub clear: bool,
}

impl Spec {
    /// Validate the agent's arguments completely, before any mutation.
    pub fn parse(args: &JsonValue) -> Result<Spec, String> {
        let Some(nodes_in) = args.get("nodes").and_then(|v| v.as_array()) else {
            return Err("diagram: no `nodes` given.".to_string());
        };
        if nodes_in.is_empty() {
            return Err("diagram: `nodes` was empty.".to_string());
        }

        // id (wires edges), label (shown), optional color/group. id and label
        // default to each other so a bare {label} or {id} still works.
        let mut nodes: Vec<SpecNode> = Vec::with_capacity(nodes_in.len());
        for (i, nv) in nodes_in.iter().enumerate() {
            if !nv.is_object() {
                return Err(format!("diagram node {i} must be an object"));
            }
            let label = match nv.get("label") {
                Some(value) => value
                    .as_str()
                    .ok_or_else(|| format!("diagram node {i} `label` must be a string"))?
                    .trim()
                    .to_string(),
                None => String::new(),
            };
            let id = match nv.get("id") {
                Some(value) => {
                    let id = value
                        .as_str()
                        .ok_or_else(|| format!("diagram node {i} `id` must be a string"))?
                        .trim();
                    if id.is_empty() {
                        return Err(format!("diagram node {i} `id` must not be empty"));
                    }
                    id.to_string()
                }
                None => {
                    if label.is_empty() {
                        format!("n{i}")
                    } else {
                        label.clone()
                    }
                }
            };
            if nodes.iter().any(|existing| existing.id == id) {
                return Err(format!("diagram contains duplicate node id {id:?}"));
            }
            let color = match nv.get("color") {
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| format!("diagram node {i} `color` must be a string"))?
                        .trim()
                        .to_string(),
                )
                .filter(|color| !color.is_empty()),
                None => None,
            };
            let group = match nv.get("group") {
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| format!("diagram node {i} `group` must be a string"))?
                        .trim()
                        .to_string(),
                )
                .filter(|group| !group.is_empty()),
                None => None,
            };
            let label = if label.is_empty() { id.clone() } else { label };
            nodes.push(SpecNode {
                id,
                label,
                color,
                group,
            });
        }

        // Resolve every edge before returning. A typo in structural input is an
        // error, never a partial diagram.
        let mut edges: Vec<SpecEdge> = Vec::new();
        if let Some(value) = args.get("edges") {
            let edge_values = value
                .as_array()
                .ok_or_else(|| "diagram `edges` must be an array".to_string())?;
            for (edge_index, edge) in edge_values.iter().enumerate() {
                if !edge.is_object() {
                    return Err(format!("diagram edge {edge_index} must be an object"));
                }
                let endpoint = |name: &str| -> Result<&str, String> {
                    let endpoint = edge
                        .get(name)
                        .and_then(JsonValue::as_str)
                        .ok_or_else(|| {
                            format!("diagram edge {edge_index} requires string `{name}`")
                        })?
                        .trim();
                    if endpoint.is_empty() {
                        return Err(format!(
                            "diagram edge {edge_index} `{name}` must not be empty"
                        ));
                    }
                    Ok(endpoint)
                };
                let from_id = endpoint("from")?;
                let to_id = endpoint("to")?;
                let index_of = |id: &str| nodes.iter().position(|node| node.id == id);
                let from = index_of(from_id).ok_or_else(|| {
                    format!("diagram edge {edge_index} references unknown node {from_id:?}")
                })?;
                let to = index_of(to_id).ok_or_else(|| {
                    format!("diagram edge {edge_index} references unknown node {to_id:?}")
                })?;
                if from == to {
                    return Err(format!(
                        "diagram edge {edge_index} cannot connect node {from_id:?} to itself"
                    ));
                }
                let arrow = match edge.get("arrow") {
                    Some(value) => value.as_bool().ok_or_else(|| {
                        format!("diagram edge {edge_index} `arrow` must be a boolean")
                    })?,
                    None => true,
                };
                let label = match edge.get("label") {
                    Some(value) => Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                format!("diagram edge {edge_index} `label` must be a string")
                            })?
                            .trim()
                            .to_string(),
                    )
                    .filter(|label| !label.is_empty()),
                    None => None,
                };
                edges.push(SpecEdge {
                    from,
                    to,
                    arrow,
                    label,
                });
            }
        }

        let direction = Direction::parse(
            args.get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("right"),
        );
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let clear = match args.get("clear") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "diagram `clear` must be a boolean".to_string())?,
            None => true,
        };

        Ok(Spec {
            nodes,
            edges,
            direction,
            title,
            clear,
        })
    }

    pub fn labels(&self) -> Vec<String> {
        self.nodes.iter().map(|node| node.label.clone()).collect()
    }

    pub fn edge_pairs(&self) -> Vec<(usize, usize)> {
        self.edges.iter().map(|e| (e.from, e.to)).collect()
    }

    /// Lay this spec out in natural units. Fit or place it yourself.
    ///
    /// Widens the gap between ranks when any edge is labelled, so the label
    /// has somewhere to sit that is not on top of the boxes it connects.
    pub fn layout(&self) -> Layout {
        let labelled = self.edges.iter().any(|edge| edge.label.is_some());
        layout_with(
            &self.labels(),
            &self.edge_pairs(),
            self.direction,
            if labelled {
                GAP_PRIMARY_LABELLED
            } else {
                GAP_PRIMARY
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The frame teaching-canvas fits to; used here to keep the old
    /// off-frame assertions meaningful after fitting moved to the caller.
    const FIT_EXTENT: f64 = 8.6;

    fn labels(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("n{i}")).collect()
    }

    #[test]
    fn ranks_linear_chain() {
        // 0 -> 1 -> 2 -> 3 : strictly increasing rank
        let r = ranks(4, &[(0, 1), (1, 2), (2, 3)]);
        assert_eq!(r, vec![0, 1, 2, 3]);
    }

    #[test]
    fn ranks_diamond_longest_path() {
        // 0->1, 0->2, 1->3, 2->3 : 3 is rank 2 (longest path), not 1
        let r = ranks(4, &[(0, 1), (0, 2), (1, 3), (2, 3)]);
        assert_eq!(r[0], 0);
        assert_eq!(r[3], 2);
    }

    #[test]
    fn cycle_does_not_panic_and_ranks_all() {
        // 0->1->2->0 (cycle) + a clean tail 2->3
        let r = ranks(4, &[(0, 1), (1, 2), (2, 0), (2, 3)]);
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn layout_places_every_node_and_fits_frame() {
        let l = layout(
            &labels(6),
            &[(0, 1), (0, 2), (1, 3), (2, 4), (3, 5), (4, 5)],
            Direction::Right,
        )
        .fit(FIT_EXTENT);
        assert_eq!(l.nodes.len(), 6);
        for p in &l.nodes {
            assert!(
                p.x.abs() + p.half <= FIT_EXTENT + 1e-6,
                "node ran off-frame: {p:?}"
            );
            assert!(
                p.y.abs() + p.half <= FIT_EXTENT + 1e-6,
                "node ran off-frame: {p:?}"
            );
        }
    }

    #[test]
    fn long_label_box_is_not_clipped_before_global_fit() {
        let label = "Validated input";
        let expected_half = label.chars().count() as f64 * BASE_TEXT * MONO_ADVANCE * 0.5 + PAD;
        assert!(node_half(label, BASE_TEXT) >= expected_half);

        let placed = layout(&[label.to_string()], &[], Direction::Right);
        assert!(placed.nodes[0].half >= expected_half);
    }

    #[test]
    fn layout_right_flows_along_x_by_rank() {
        // chain: each successive node should have a strictly greater x than the last.
        let l = layout(&labels(4), &[(0, 1), (1, 2), (2, 3)], Direction::Right);
        assert!(l.nodes[0].x < l.nodes[1].x);
        assert!(l.nodes[1].x < l.nodes[2].x);
        assert!(l.nodes[2].x < l.nodes[3].x);
    }

    #[test]
    fn onnx_shaped_no_overlap() {
        // input x -> matmul, weights -> matmul, matmul -> output y
        let labs: Vec<String> = ["input x", "weights", "MatMul", "output y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let l = layout(&labs, &[(0, 2), (1, 2), (2, 3)], Direction::Right);
        // pairwise: no two boxes overlap (separating axis on the square extents)
        for i in 0..l.nodes.len() {
            for j in (i + 1)..l.nodes.len() {
                let a = &l.nodes[i];
                let b = &l.nodes[j];
                let overlap_x = (a.x - b.x).abs() < (a.half + b.half) - 1e-3;
                let overlap_y = (a.y - b.y).abs() < (a.half + b.half) - 1e-3;
                assert!(!(overlap_x && overlap_y), "boxes {i} and {j} overlap");
            }
        }
    }

    // ── fit / place / bounds ────────────────────────────────────────────────

    #[test]
    fn fit_only_ever_shrinks() {
        // A single small node is already inside the frame; fitting must not
        // scale it up to fill it.
        let natural = layout(&["a".to_string()], &[], Direction::Right);
        let fitted = natural.clone().fit(FIT_EXTENT);
        assert_eq!(natural, fitted);
    }

    #[test]
    fn fit_scales_text_with_the_boxes() {
        // 40 long labels in one rank forces a shrink; the label-to-box ratio
        // has to survive it or text spills out of the nodes.
        let labs: Vec<String> = (0..40).map(|i| format!("a long node label {i}")).collect();
        let natural = layout(&labs, &[], Direction::Right);
        let fitted = natural.clone().fit(FIT_EXTENT);
        assert!(fitted.text_size < natural.text_size, "expected a shrink");
        let ratio_before = natural.text_size / natural.nodes[0].half;
        let ratio_after = fitted.text_size / fitted.nodes[0].half;
        assert!((ratio_before - ratio_after).abs() < 1e-9);
    }

    #[test]
    fn place_puts_the_top_left_bound_on_the_origin() {
        let l = layout(
            &labels(5),
            &[(0, 1), (1, 2), (0, 3), (3, 4)],
            Direction::Down,
        )
        .place(64.0, 120.0, 80.0);
        let (left, top, right, bottom) = l.bounds();
        assert!((left - 120.0).abs() < 1e-9, "left was {left}");
        assert!((top - 80.0).abs() < 1e-9, "top was {top}");
        assert!(right > left && bottom > top);
    }

    #[test]
    fn bounds_of_an_empty_layout_is_not_inverted() {
        // An inverted box turns into a negative width/height on an SVG viewBox.
        let (l, t, r, b) = layout(&[], &[], Direction::Right).bounds();
        assert_eq!((l, t, r, b), (0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn a_drawn_box_never_escapes_the_square_the_layout_reserved() {
        // This is what makes rectangles safe: spacing is computed from `half`,
        // so as long as w/h stay inside that square, no two drawn boxes touch.
        let labs: Vec<String> = ["x", "a much longer node label", "mid"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let l = layout(&labs, &[(0, 1), (1, 2)], Direction::Right);
        for p in &l.nodes {
            assert!(p.w <= p.half * 2.0 + 1e-9, "width escaped: {p:?}");
            assert!(p.h <= p.half * 2.0 + 1e-9, "height escaped: {p:?}");
            assert!(p.w > 0.0 && p.h > 0.0);
        }
    }

    #[test]
    fn a_one_line_label_gets_a_wide_box_not_a_tall_one() {
        // The defect this fixes: drawing a one-line label in its reserved
        // square leaves a mostly empty box on screen.
        let l = layout(&["atlas shapes".to_string()], &[], Direction::Right);
        let p = l.nodes[0];
        assert!(p.w > p.h * 2.0, "expected a wide box, got {p:?}");
    }

    #[test]
    fn an_edge_stops_on_the_rectangle_not_on_a_circle_around_it() {
        let wide = Placed {
            x: 0.0,
            y: 0.0,
            half: 5.0,
            w: 10.0,
            h: 2.0,
        };
        let right = Placed {
            x: 100.0,
            y: 0.0,
            half: 1.0,
            w: 2.0,
            h: 2.0,
        };
        // Straight out the side: exactly the half-width, not the half-square.
        assert_eq!(wide.edge_toward(&right), (5.0, 0.0));

        // Straight up: limited by the half-height instead.
        let above = Placed {
            x: 0.0,
            y: -100.0,
            ..right
        };
        assert_eq!(wide.edge_toward(&above), (0.0, -1.0));

        // Diagonal: on the boundary, never outside it.
        let corner = Placed {
            x: 100.0,
            y: -100.0,
            ..right
        };
        let (x, y) = wide.edge_toward(&corner);
        assert!(x <= 5.0 + 1e-9 && y >= -1.0 - 1e-9);
        assert!((x - 5.0).abs() < 1e-9 || (y + 1.0).abs() < 1e-9);
    }

    // ── Spec::parse ─────────────────────────────────────────────────────────

    #[test]
    fn parses_a_full_spec() {
        let spec = Spec::parse(&json!({
            "nodes": [
                { "id": "a", "label": "input x", "color": "#38bdf8" },
                { "id": "b", "label": "MatMul", "group": "op" },
                { "id": "c", "label": "output y" }
            ],
            "edges": [
                { "from": "a", "to": "b", "label": "x" },
                { "from": "b", "to": "c", "arrow": false }
            ],
            "direction": "down",
            "title": "a tiny graph"
        }))
        .expect("spec parses");

        assert_eq!(spec.nodes.len(), 3);
        assert_eq!(spec.nodes[0].color.as_deref(), Some("#38bdf8"));
        assert_eq!(spec.nodes[1].group.as_deref(), Some("op"));
        assert_eq!(spec.edges[0].label.as_deref(), Some("x"));
        assert!(spec.edges[0].arrow, "edges carry an arrowhead by default");
        assert!(!spec.edges[1].arrow);
        assert_eq!(spec.direction, Direction::Down);
        assert_eq!(spec.title.as_deref(), Some("a tiny graph"));
        assert!(spec.clear, "a diagram replaces the surface by default");
    }

    #[test]
    fn id_and_label_default_to_each_other() {
        let spec = Spec::parse(&json!({
            "nodes": [{ "label": "just a label" }, { "id": "just-an-id" }]
        }))
        .expect("spec parses");
        assert_eq!(spec.nodes[0].id, "just a label");
        assert_eq!(spec.nodes[1].label, "just-an-id");
    }

    #[test]
    fn an_edge_to_an_unknown_node_is_an_error() {
        // The point of validating up front: this must fail before anything is
        // drawn, not leave half a diagram on the surface.
        let error = Spec::parse(&json!({
            "nodes": [{ "id": "a" }],
            "edges": [{ "from": "a", "to": "ghost" }]
        }))
        .expect_err("an unknown endpoint does not parse");
        assert!(error.contains("ghost"), "unhelpful error: {error}");
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        Spec::parse(&json!({ "nodes": [{ "id": "a" }, { "id": "a" }] }))
            .expect_err("duplicate ids do not parse");
    }

    #[test]
    fn self_edges_are_an_error() {
        Spec::parse(&json!({
            "nodes": [{ "id": "a" }],
            "edges": [{ "from": "a", "to": "a" }]
        }))
        .expect_err("a self-edge does not parse");
    }

    #[test]
    fn empty_or_missing_nodes_are_an_error() {
        Spec::parse(&json!({ "nodes": [] })).expect_err("empty nodes do not parse");
        Spec::parse(&json!({ "title": "no nodes" })).expect_err("missing nodes do not parse");
    }

    #[test]
    fn wrong_types_are_rejected_rather_than_coerced() {
        Spec::parse(&json!({ "nodes": [{ "id": 7 }] })).expect_err("numeric id");
        Spec::parse(&json!({ "nodes": [{ "id": "a" }], "edges": {} })).expect_err("object edges");
        Spec::parse(&json!({ "nodes": [{ "id": "a" }], "clear": "yes" }))
            .expect_err("string clear");
        Spec::parse(&json!({
            "nodes": [{ "id": "a" }, { "id": "b" }],
            "edges": [{ "from": "a", "to": "b", "arrow": "yes" }]
        }))
        .expect_err("string arrow");
    }

    #[test]
    fn a_labelled_edge_gets_more_room_than_a_bare_one() {
        let spec_of = |edge: JsonValue| {
            Spec::parse(&json!({ "nodes": [{ "id": "a" }, { "id": "b" }], "edges": [edge] }))
                .expect("spec parses")
                .layout()
        };
        let bare = spec_of(json!({ "from": "a", "to": "b" }));
        let labelled = spec_of(json!({ "from": "a", "to": "b", "label": "nodes+edges" }));
        let span = |l: &Layout| (l.nodes[1].x - l.nodes[0].x).abs();
        assert!(
            span(&labelled) > span(&bare),
            "a labelled edge must not be laid out as tightly as a bare one"
        );
    }

    #[test]
    fn spec_layout_matches_a_hand_built_layout() {
        let spec = Spec::parse(&json!({
            "nodes": [{ "id": "a" }, { "id": "b" }],
            "edges": [{ "from": "a", "to": "b" }]
        }))
        .expect("spec parses");
        let direct = layout(
            &["a".to_string(), "b".to_string()],
            &[(0, 1)],
            Direction::Right,
        );
        assert_eq!(spec.layout(), direct);
    }
}
