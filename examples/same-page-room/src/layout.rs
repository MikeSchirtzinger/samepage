//! Where panes sit, and how to say it without numbers.
//!
//! The room used to store a pane's position as a place in an ordered list plus
//! a `span` of 1-3 columns, because the read-back has to describe the room in
//! words and a named token is trivially a word. That inference was wrong, and
//! it cost the surface its whole point: a person could not put a pane where
//! they wanted it, because nothing in the model could express "over there".
//!
//! The split this module draws instead:
//!
//! - **Stored** as a free rectangle. The person drags a pane anywhere on a
//!   canvas larger than the window and it stays exactly where they dropped it.
//!   Nothing here caps a width, a height, or a position.
//! - **Addressed** by pane id. An agent says "the particles pane", never a
//!   coordinate, in either direction.
//! - **Described** by relations *derived from* the rectangles at read time —
//!   `right of`, `below`, `overlaps`, `aligned` — the same move
//!   `same-page-atlas` already makes when it reports ink over cards. Geometry
//!   is an implementation detail of the sentence, not a thing the agent ever
//!   sees.
//!
//! So `describe` is the load-bearing function in this file. If a read-back ever
//! starts leaking pixels, the bug is here, not in the storage model.

use serde::{Deserialize, Serialize};

/// A pane's rectangle in canvas units. Canvas units are CSS pixels at zoom 1,
/// but nothing outside this module and the renderer is allowed to care.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Spot {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What a fresh pane gets when nobody said otherwise.
pub const DEFAULT_W: f64 = 420.0;
pub const DEFAULT_H: f64 = 280.0;

/// The smallest a pane may be dragged to. Below this the header and its byline
/// stop being readable, and a pane nobody can identify cannot be talked about.
pub const MIN_W: f64 = 200.0;
pub const MIN_H: f64 = 120.0;

/// Generous ceilings. These exist so a bad number cannot make the canvas
/// unusable, not to express a layout opinion.
pub const MAX_W: f64 = 4000.0;
pub const MAX_H: f64 = 4000.0;
pub const MAX_COORD: f64 = 20000.0;

/// The gap the host leaves when it places a pane itself.
const GUTTER: f64 = 20.0;

/// Two edges within this many units read as deliberately aligned rather than
/// coincidentally close, and get said so in the read-back.
const ALIGN_SLOP: f64 = 6.0;

/// How much of the shorter side two panes must share before they count as
/// side-by-side (or stacked) rather than merely diagonal from each other.
const FACING: f64 = 0.4;

/// Beyond this much empty space between two panes, they are not neighbours and
/// claiming a relation between them would be noise.
const NEIGHBOUR_REACH: f64 = 520.0;

impl Spot {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }.clamped()
    }

    /// Force a rectangle into the legal range. Called on everything that
    /// arrives from the browser, so a hand-edited document or a wild drag
    /// cannot produce a pane nobody can reach.
    pub fn clamped(self) -> Self {
        let w = self.w.clamp(MIN_W, MAX_W);
        let h = self.h.clamp(MIN_H, MAX_H);
        Self {
            x: sane(self.x).clamp(0.0, MAX_COORD),
            y: sane(self.y).clamp(0.0, MAX_COORD),
            w: sane(w).clamp(MIN_W, MAX_W),
            h: sane(h).clamp(MIN_H, MAX_H),
        }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn center_x(&self) -> f64 {
        self.x + self.w / 2.0
    }

    pub fn center_y(&self) -> f64 {
        self.y + self.h / 2.0
    }

    fn overlaps(&self, other: &Spot) -> bool {
        span_overlap(self.x, self.right(), other.x, other.right()) > 0.0
            && span_overlap(self.y, self.bottom(), other.y, other.bottom()) > 0.0
    }
}

/// NaN and infinity are the two ways a JSON number ruins a layout silently.
fn sane(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn span_overlap(a_low: f64, a_high: f64, b_low: f64, b_high: f64) -> f64 {
    (a_high.min(b_high) - a_low.max(b_low)).max(0.0)
}

// ---------------------------------------------------------------------------
// Migrating a room that was written before panes had rectangles
// ---------------------------------------------------------------------------

/// Rebuild the old masonry flow as rectangles, so a document written under the
/// column model opens looking like it did before.
///
/// This is deliberately a *reconstruction*, not a promise of pixel fidelity:
/// the old layout's row heights came from measuring rendered content in the
/// browser, which the server has never seen. Panes land in the right columns in
/// the right order at sensible sizes, and from that moment on the person can
/// drag them anywhere, which is the point of the change.
pub fn migrate_flow(panes: &[(u8, &str)], columns: u8) -> Vec<Spot> {
    let columns = columns.clamp(1, 3) as usize;
    let width = DEFAULT_W;
    let mut bottoms = vec![0.0_f64; columns];
    let mut out = Vec::with_capacity(panes.len());

    for (span, height) in panes {
        let span = (*span as usize).clamp(1, columns);
        // Find the leftmost run of `span` columns whose deepest bottom is
        // shallowest — the same rule the CSS masonry was applying.
        let mut best = 0usize;
        let mut best_depth = f64::MAX;
        for start in 0..=(columns - span) {
            let depth = bottoms[start..start + span]
                .iter()
                .copied()
                .fold(0.0_f64, f64::max);
            if depth < best_depth - 0.5 {
                best_depth = depth;
                best = start;
            }
        }
        let spot = Spot::new(
            best as f64 * (width + GUTTER),
            best_depth,
            span as f64 * width + (span as f64 - 1.0) * GUTTER,
            height_px(height),
        );
        for bottom in bottoms.iter_mut().skip(best).take(span) {
            *bottom = spot.bottom() + GUTTER;
        }
        out.push(spot);
    }
    out
}

/// What the four old height names were worth on screen, near enough.
fn height_px(height: &str) -> f64 {
    match height {
        "short" => 200.0,
        "tall" => 460.0,
        "full" => 720.0,
        _ => DEFAULT_H,
    }
}

// ---------------------------------------------------------------------------
// Placing a pane the way an agent is allowed to ask for it
// ---------------------------------------------------------------------------

/// A placement an agent may request. Every variant names a pane or a side of
/// the canvas; none of them carries a coordinate. This enum *is* the boundary —
/// if a pixel ever needs to reach the host from the agent side, it belongs in
/// the human-audience action instead.
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    Start,
    End,
    RightOf(String),
    LeftOf(String),
    Below(String),
    Above(String),
    Near(String),
}

impl Placement {
    /// Parse `"right of: particles"`, `"below: catalog"`, `"start"`, `"end"`.
    ///
    /// The separator is forgiving (`:` optional, spaces and hyphens equivalent)
    /// because this string is written by a language model mid-sentence, and a
    /// refusal over punctuation teaches it nothing worth learning. `before` and
    /// `after` are kept as synonyms for `above`/`below` so calls written
    /// against the ordered-list model still land somewhere sensible.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let lowered = raw.trim().to_ascii_lowercase();
        let normalized = lowered.replace(['-', '_'], " ");
        let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        match compact.as_str() {
            "start" | "first" | "top" => return Ok(Placement::Start),
            "end" | "last" | "bottom" => return Ok(Placement::End),
            _ => {}
        }
        let (keyword, target) = compact
            .split_once(':')
            .map(|(key, rest)| (key.trim().to_string(), rest.trim().to_string()))
            .or_else(|| {
                // No colon: take the longest keyword that prefixes the string.
                for keyword in [
                    "right of", "left of", "below", "above", "under", "over", "beside", "near",
                    "before", "after",
                ] {
                    if let Some(rest) = compact.strip_prefix(keyword) {
                        return Some((keyword.to_string(), rest.trim().to_string()));
                    }
                }
                None
            })
            .ok_or_else(|| {
                format!(
                    "place {raw:?} is not something I can resolve. Use start, end, or a \
                     relation to another pane: \"right of: <pane id>\", \"left of: <id>\", \
                     \"below: <id>\", \"above: <id>\", \"near: <id>\"."
                )
            })?;

        if target.is_empty() {
            return Err(format!(
                "place {raw:?} names a direction but no pane. Say which pane to sit {keyword} of."
            ));
        }
        let target = target.replace(' ', "-");
        match keyword.as_str() {
            "right of" | "right" | "beside" => Ok(Placement::RightOf(target)),
            "left of" | "left" => Ok(Placement::LeftOf(target)),
            "below" | "under" | "after" => Ok(Placement::Below(target)),
            "above" | "over" | "before" => Ok(Placement::Above(target)),
            "near" | "by" | "next to" => Ok(Placement::Near(target)),
            other => Err(format!(
                "place {raw:?} starts with {other:?}, which is not a direction I know. \
                 Use right of, left of, below, above or near."
            )),
        }
    }
}

/// One pane as the placer and the describer see it: an id, the name a person
/// would actually say out loud, and a rectangle.
pub struct Sited<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub spot: Spot,
}

/// Turn a relational request into a rectangle that does not sit on top of
/// anything else.
///
/// Resolution is deliberately two-stage: aim where the agent asked, then slide
/// down until the space is free. An agent that says "below the catalog" means
/// "in the reading position after the catalog", not "at these coordinates" —
/// so honouring the intent and then avoiding a collision is more faithful than
/// refusing because the obvious spot was taken.
pub fn resolve(
    placement: Option<&Placement>,
    others: &[Sited<'_>],
    size: (f64, f64),
) -> Result<Spot, String> {
    let (w, h) = size;
    let find = |id: &str| -> Result<&Sited<'_>, String> {
        others
            .iter()
            .find(|pane| pane.id == id)
            .ok_or_else(|| format!("place names pane {id}, which is not in the room"))
    };

    let aim = match placement {
        None | Some(Placement::End) => {
            // Under everything, at the left edge — where a new pane appears if
            // nobody said where to put it.
            let bottom = others
                .iter()
                .map(|pane| pane.spot.bottom())
                .fold(0.0_f64, f64::max);
            let left = others
                .iter()
                .map(|pane| pane.spot.x)
                .fold(f64::MAX, f64::min);
            Spot::new(
                if others.is_empty() { 0.0 } else { left },
                if others.is_empty() { 0.0 } else { bottom + GUTTER },
                w,
                h,
            )
        }
        Some(Placement::Start) => {
            let left = others
                .iter()
                .map(|pane| pane.spot.x)
                .fold(f64::MAX, f64::min);
            let top = others
                .iter()
                .map(|pane| pane.spot.y)
                .fold(f64::MAX, f64::min);
            if others.is_empty() {
                Spot::new(0.0, 0.0, w, h)
            } else {
                // Above the topmost pane rather than on it.
                Spot::new(left, (top - h - GUTTER).max(0.0), w, h)
            }
        }
        Some(Placement::RightOf(id)) => {
            let anchor = find(id)?;
            Spot::new(anchor.spot.right() + GUTTER, anchor.spot.y, w, h)
        }
        Some(Placement::LeftOf(id)) => {
            let anchor = find(id)?;
            Spot::new(
                (anchor.spot.x - w - GUTTER).max(0.0),
                anchor.spot.y,
                w,
                h,
            )
        }
        Some(Placement::Below(id)) => {
            let anchor = find(id)?;
            Spot::new(anchor.spot.x, anchor.spot.bottom() + GUTTER, w, h)
        }
        Some(Placement::Above(id)) => {
            let anchor = find(id)?;
            Spot::new(
                anchor.spot.x,
                (anchor.spot.y - h - GUTTER).max(0.0),
                w,
                h,
            )
        }
        Some(Placement::Near(id)) => {
            let anchor = find(id)?;
            Spot::new(anchor.spot.right() + GUTTER, anchor.spot.y, w, h)
        }
    };

    Ok(settle(aim, others))
}

/// Find the free rectangle closest to where the placement asked for.
///
/// The obvious implementation — slide down until nothing overlaps — is wrong in
/// a way that took a live test to see: asked to sit below a pane at the top of
/// a busy column, it slid past every pane in that column and landed at the
/// bottom of the room. It had honoured "do not overlap" and quietly discarded
/// "below that one", which is the only part the agent actually said.
///
/// So search outward instead. Candidate positions are the aim itself plus the
/// edges of everything already placed, and the winner is whichever free spot
/// sits nearest the aim — which keeps "below the particles pane" beside the
/// particles pane even when the space directly under it is taken.
fn settle(aim: Spot, others: &[Sited<'_>]) -> Spot {
    let free = |spot: &Spot| !others.iter().any(|pane| pane.spot.overlaps(spot));
    if free(&aim) {
        return aim;
    }

    let mut xs = vec![aim.x];
    let mut ys = vec![aim.y];
    for pane in others {
        xs.push(pane.spot.right() + GUTTER);
        xs.push((pane.spot.x - aim.w - GUTTER).max(0.0));
        ys.push(pane.spot.bottom() + GUTTER);
        ys.push((pane.spot.y - aim.h - GUTTER).max(0.0));
    }

    let mut best: Option<(f64, Spot)> = None;
    for &x in &xs {
        for &y in &ys {
            let candidate = Spot::new(x, y, aim.w, aim.h);
            if !free(&candidate) {
                continue;
            }
            // Distance from the aim, biased so that sliding sideways is
            // cheaper than sliding down: "below X" that ends up beside X is
            // still recognisably below-ish, whereas one that ends up a screen
            // further down is not anywhere the agent asked for.
            let cost = (candidate.x - aim.x).abs() + 1.4 * (candidate.y - aim.y).abs();
            if best.is_none_or(|(best_cost, _)| cost < best_cost) {
                best = Some((cost, candidate));
            }
        }
    }

    // Nothing free anywhere in the grid of candidates: fall back to underneath
    // everything, which always exists because the canvas grows downward.
    best.map(|(_, spot)| spot).unwrap_or_else(|| {
        let bottom = others
            .iter()
            .map(|pane| pane.spot.bottom())
            .fold(0.0_f64, f64::max);
        Spot::new(aim.x, bottom + GUTTER, aim.w, aim.h)
    })
}

// ---------------------------------------------------------------------------
// Saying where things are, in words
// ---------------------------------------------------------------------------

/// Which way one pane sits from another, once they are close enough and
/// square-on enough for the relation to be worth stating.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Right,
    Left,
    Below,
    Above,
}

impl Side {
    fn word(self) -> &'static str {
        match self {
            Side::Right => "right of",
            Side::Left => "left of",
            Side::Below => "below",
            Side::Above => "above",
        }
    }
}

/// Reading order: down the page in bands, left to right within a band. Two
/// panes whose tops are within a band height of each other are "on the same
/// line" the way a person would read them, regardless of exact tops.
pub fn in_reading_order(panes: &[Sited<'_>]) -> Vec<usize> {
    reading_order(panes)
}

fn reading_order(panes: &[Sited<'_>]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..panes.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&panes[a].spot, &panes[b].spot);
        let same_band = span_overlap(a.y, a.bottom(), b.y, b.bottom())
            > FACING * a.h.min(b.h);
        if same_band {
            a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
    order
}

/// The nearest pane on `side` of `subject`, if one is close enough and facing
/// it squarely enough to be called a neighbour.
fn neighbour(subject: &Spot, side: Side, panes: &[Sited<'_>], skip: usize) -> Option<usize> {
    let mut best: Option<(f64, usize)> = None;
    for (index, other) in panes.iter().enumerate() {
        if index == skip {
            continue;
        }
        let spot = &other.spot;
        let (facing, gap, slack) = match side {
            Side::Right | Side::Left => {
                let shared = span_overlap(subject.y, subject.bottom(), spot.y, spot.bottom());
                let facing = shared > FACING * subject.h.min(spot.h);
                let gap = if side == Side::Right {
                    spot.x - subject.right()
                } else {
                    subject.x - spot.right()
                };
                (facing, gap, subject.w.min(spot.w))
            }
            Side::Below | Side::Above => {
                let shared = span_overlap(subject.x, subject.right(), spot.x, spot.right());
                let facing = shared > FACING * subject.w.min(spot.w);
                let gap = if side == Side::Below {
                    spot.y - subject.bottom()
                } else {
                    subject.y - spot.bottom()
                };
                (facing, gap, subject.h.min(spot.h))
            }
        };
        // A negative gap means they overlap. That is still a relation worth
        // stating — arguably the most worth stating — so allow it up to the
        // point where "B is right of A" stops being true of what you'd see.
        if !facing || gap < -0.9 * slack || gap > NEIGHBOUR_REACH {
            continue;
        }
        if best.is_none_or(|(best_gap, _)| gap < best_gap) {
            best = Some((gap, index));
        }
    }
    best.map(|(_, index)| index)
}

/// Edges that line up, phrased as the person's intent rather than as numbers.
fn alignment(a: &Spot, b: &Spot, side: Side) -> Option<&'static str> {
    match side {
        Side::Right | Side::Left => {
            if (a.y - b.y).abs() <= ALIGN_SLOP {
                Some("top edges aligned")
            } else if (a.bottom() - b.bottom()).abs() <= ALIGN_SLOP {
                Some("bottom edges aligned")
            } else if (a.center_y() - b.center_y()).abs() <= ALIGN_SLOP {
                Some("centred on each other")
            } else {
                None
            }
        }
        Side::Below | Side::Above => {
            if (a.x - b.x).abs() <= ALIGN_SLOP {
                Some("left edges aligned")
            } else if (a.right() - b.right()).abs() <= ALIGN_SLOP {
                Some("right edges aligned")
            } else if (a.center_x() - b.center_x()).abs() <= ALIGN_SLOP {
                Some("centred on each other")
            } else {
                None
            }
        }
    }
}

/// Coarse position on the canvas, for a pane with nothing near it to relate to.
///
/// Nine named regions is the whole vocabulary. It is vague on purpose — the
/// point is "over on the right somewhere", which is what a person would say,
/// and precision here would just be a coordinate wearing a word.
fn region(spot: &Spot, extent: &Spot) -> String {
    let column = |value: f64, low: f64, size: f64| -> &'static str {
        if size <= 1.0 {
            return "centre";
        }
        let ratio = (value - low) / size;
        if ratio < 0.34 {
            "left"
        } else if ratio < 0.67 {
            "centre"
        } else {
            "right"
        }
    };
    let row = |value: f64, low: f64, size: f64| -> &'static str {
        if size <= 1.0 {
            return "middle";
        }
        let ratio = (value - low) / size;
        if ratio < 0.34 {
            "top"
        } else if ratio < 0.67 {
            "middle"
        } else {
            "bottom"
        }
    };
    let vertical = row(spot.center_y(), extent.y, extent.h);
    let horizontal = column(spot.center_x(), extent.x, extent.w);
    if vertical == "middle" && horizontal == "centre" {
        "in the middle of the canvas".to_string()
    } else {
        format!("at the {vertical} {horizontal} of the canvas")
    }
}

/// The bounding box of everything on the canvas.
fn extent(panes: &[Sited<'_>]) -> Spot {
    let left = panes.iter().map(|p| p.spot.x).fold(f64::MAX, f64::min);
    let top = panes.iter().map(|p| p.spot.y).fold(f64::MAX, f64::min);
    let right = panes.iter().map(|p| p.spot.right()).fold(f64::MIN, f64::max);
    let bottom = panes
        .iter()
        .map(|p| p.spot.bottom())
        .fold(f64::MIN, f64::max);
    Spot {
        x: left,
        y: top,
        w: (right - left).max(1.0),
        h: (bottom - top).max(1.0),
    }
}

/// Describe the whole canvas as relations between named panes.
///
/// This is the function the surface exists to get right. Everything it emits
/// names panes by their title and says how they sit relative to each other; no
/// caller ever receives an x, a y, a width or a height. If you are tempted to
/// add one "just for debugging", add a relation instead — a number here is a
/// number in the agent's next sentence back to the person.
pub fn describe(panes: &[Sited<'_>]) -> String {
    if panes.is_empty() {
        return String::new();
    }
    if panes.len() == 1 {
        return format!(
            "\nWHERE THINGS SIT\n- “{}” is the only pane on the canvas.\n",
            panes[0].title
        );
    }

    let extent = extent(panes);
    let order = reading_order(panes);
    let mut out = String::from(
        "\nWHERE THINGS SIT — read as relations; the canvas is free-form and the person \
         can drag anything anywhere\n",
    );

    // Say each relation once, from whichever pane comes first in reading order,
    // so the list reads like a description instead of a matrix.
    let mut said: Vec<(usize, usize)> = Vec::new();
    let mut anchored = vec![false; panes.len()];

    for (rank, &index) in order.iter().enumerate() {
        let subject = &panes[index];
        let mut clauses: Vec<String> = Vec::new();

        for side in [Side::Right, Side::Below, Side::Left, Side::Above] {
            let Some(other) = neighbour(&subject.spot, side, panes, index) else {
                continue;
            };
            let pair = (index.min(other), index.max(other));
            if said.contains(&pair) {
                continue;
            }
            // Only claim the relation from the pane that reads first, so we get
            // "B is right of A" rather than both directions.
            let other_rank = order.iter().position(|&i| i == other).unwrap_or(usize::MAX);
            if other_rank < rank {
                continue;
            }
            said.push(pair);
            anchored[index] = true;
            anchored[other] = true;
            let aligned = alignment(&subject.spot, &panes[other].spot, side)
                .map(|note| format!(" ({note})"))
                .unwrap_or_default();
            let touching = if panes[other].spot.overlaps(&subject.spot) {
                " — they overlap"
            } else {
                ""
            };
            clauses.push(format!(
                "“{}” sits {} it{aligned}{touching}",
                panes[other].title,
                side.word(),
            ));
        }

        if clauses.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "- “{}”: {}.\n",
            subject.title,
            join_clauses(&clauses)
        ));
    }

    // A pane nobody is next to has no relation to state, so give it the only
    // honest thing left: roughly where it is, in words.
    for (index, pane) in panes.iter().enumerate() {
        if anchored[index] {
            continue;
        }
        out.push_str(&format!(
            "- “{}” sits on its own, {}.\n",
            pane.title,
            region(&pane.spot, &extent)
        ));
    }

    out.push_str(
        "To move a pane, name it and say where it should go — \
         put_pane/arrange_room take place: \"right of: <id>\", \"below: <id>\", \
         \"near: <id>\", start or end. The person moves panes by dragging them.\n",
    );
    out
}

/// Where one pane ended up, in one clause, for the change log.
///
/// The log is what an agent reads to find out what the person did while it was
/// thinking, so "moved it below the catalog" is the whole value and "moved it
/// to 840, 320" is worse than saying nothing — it forces the agent to either
/// parrot numbers back at the person or re-read the room to find out what
/// actually changed.
pub fn locate(subject: usize, panes: &[Sited<'_>]) -> String {
    let Some(me) = panes.get(subject) else {
        return "somewhere on the canvas".to_string();
    };
    if panes.len() == 1 {
        return "alone on the canvas".to_string();
    }
    let mut best: Option<(f64, Side, usize)> = None;
    for side in [Side::Right, Side::Left, Side::Below, Side::Above] {
        let Some(other) = neighbour(&me.spot, side, panes, subject) else {
            continue;
        };
        let spot = &panes[other].spot;
        let gap = match side {
            Side::Right => spot.x - me.spot.right(),
            Side::Left => me.spot.x - spot.right(),
            Side::Below => spot.y - me.spot.bottom(),
            Side::Above => me.spot.y - spot.bottom(),
        };
        if best.is_none_or(|(best_gap, _, _)| gap < best_gap) {
            best = Some((gap, side, other));
        }
    }
    match best {
        Some((_, side, other)) => {
            // Said from the moved pane's point of view, so flip the side: if
            // the catalog is to its right, it is to the catalog's left.
            let facing = match side {
                Side::Right => "left of",
                Side::Left => "right of",
                Side::Below => "above",
                Side::Above => "below",
            };
            let aligned = alignment(&me.spot, &panes[other].spot, side)
                .map(|note| format!(", {note}"))
                .unwrap_or_default();
            let touching = if me.spot.overlaps(&panes[other].spot) {
                ", overlapping it"
            } else {
                ""
            };
            format!("{facing} “{}”{aligned}{touching}", panes[other].title)
        }
        None => format!("on its own, {}", region(&me.spot, &extent(panes))),
    }
}

fn join_clauses(clauses: &[String]) -> String {
    match clauses {
        [] => String::new(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first}; {}", rest.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sited<'a>(id: &'a str, title: &'a str, x: f64, y: f64, w: f64, h: f64) -> Sited<'a> {
        Sited {
            id,
            title,
            spot: Spot::new(x, y, w, h),
        }
    }

    #[test]
    fn placement_parses_the_shapes_a_model_actually_writes() {
        assert_eq!(
            Placement::parse("right of: particles").expect("parses"),
            Placement::RightOf("particles".to_string())
        );
        assert_eq!(
            Placement::parse("  RIGHT OF particles ").expect("parses"),
            Placement::RightOf("particles".to_string())
        );
        assert_eq!(
            Placement::parse("below:catalog").expect("parses"),
            Placement::Below("catalog".to_string())
        );
        // The ordered-list vocabulary still lands somewhere sensible.
        assert_eq!(
            Placement::parse("after: start").expect("parses"),
            Placement::Below("start".to_string())
        );
        assert_eq!(Placement::parse("end").expect("parses"), Placement::End);
    }

    #[test]
    fn placement_refuses_a_direction_with_no_pane() {
        let error = Placement::parse("right of:").expect_err("refused");
        assert!(error.contains("no pane"), "{error}");
    }

    #[test]
    fn a_placed_pane_never_lands_on_another_one() {
        let anchor = sited("a", "A", 0.0, 0.0, 400.0, 300.0);
        let blocker = sited("b", "B", 420.0, 0.0, 400.0, 300.0);
        let others = vec![anchor, blocker];
        let spot = resolve(
            Some(&Placement::RightOf("a".to_string())),
            &others,
            (400.0, 300.0),
        )
        .expect("resolves");
        // Asked to sit right of A, where B already is. It stays on the side it
        // was told to be on and goes further out, rather than dropping into
        // another row — "right of A" is still true of where it landed.
        assert!(
            spot.x >= others[0].spot.right(),
            "expected it to stay right of A, got x={}",
            spot.x
        );
        for other in &others {
            assert!(!other.spot.overlaps(&spot), "landed on {}", other.id);
        }
    }

    #[test]
    fn a_blocked_placement_stays_near_what_it_named() {
        // The bug this pins, caught live: "below: particles" with a full column
        // underneath used to slide all the way past every pane in that column
        // and land at the bottom of the room — technically not overlapping, and
        // nowhere near what was asked for.
        let mut column = vec![sited("particles", "Particles", 0.0, 0.0, 420.0, 200.0)];
        let owned: Vec<(String, f64)> = (0..6)
            .map(|index| (format!("stack{index}"), 220.0 + index as f64 * 300.0))
            .collect();
        for (id, y) in &owned {
            column.push(Sited {
                id,
                title: id,
                spot: Spot::new(0.0, *y, 860.0, 280.0),
            });
        }

        let spot = resolve(
            Some(&Placement::Below("particles".to_string())),
            &column,
            (420.0, 280.0),
        )
        .expect("resolves");

        let particles = column[0].spot;
        assert!(
            spot.y < particles.bottom() + 400.0,
            "asked to sit below the particles pane, it ended up {} below it",
            spot.y - particles.bottom()
        );
        for other in &column {
            assert!(!other.spot.overlaps(&spot), "landed on {}", other.id);
        }
    }

    #[test]
    fn the_read_back_never_contains_a_coordinate() {
        let panes = vec![
            sited("particles", "Particles", 0.0, 0.0, 400.0, 300.0),
            sited("catalog", "Primitives catalog", 420.0, 0.0, 400.0, 300.0),
            sited("start", "Start here", 0.0, 320.0, 400.0, 300.0),
            sited("lonely", "Off on its own", 2400.0, 1800.0, 400.0, 300.0),
        ];
        let text = describe(&panes);
        assert!(text.contains("“Primitives catalog” sits right of it"), "{text}");
        assert!(text.contains("“Start here” sits below it"), "{text}");
        assert!(text.contains("top edges aligned"), "{text}");
        assert!(text.contains("left edges aligned"), "{text}");
        assert!(text.contains("“Off on its own” sits on its own"), "{text}");
        // The actual guarantee: no bare numbers anywhere in the description.
        for token in text.split_whitespace() {
            let cleaned = token.trim_matches(|c: char| !c.is_ascii_digit());
            assert!(
                cleaned.is_empty(),
                "read-back leaked a number ({token:?}) — describe() must speak in relations:\n{text}"
            );
        }
    }

    #[test]
    fn overlapping_panes_are_reported_as_overlapping() {
        let panes = vec![
            sited("a", "First", 0.0, 0.0, 400.0, 300.0),
            sited("b", "Second", 380.0, 0.0, 400.0, 300.0),
        ];
        let text = describe(&panes);
        assert!(text.contains("they overlap"), "{text}");
    }

    #[test]
    fn migration_rebuilds_the_old_two_column_flow() {
        // Three span-1 panes in two columns: third sits under the shallower one.
        let spots = migrate_flow(&[(1, "auto"), (1, "auto"), (1, "short")], 2);
        assert_eq!(spots.len(), 3);
        assert!(spots[0].x < spots[1].x, "first two share a row");
        assert!((spots[0].y - spots[1].y).abs() < 1.0, "first two share a row");
        assert!(spots[2].y > spots[0].y, "third wraps below");
        assert!((spots[2].x - spots[0].x).abs() < 1.0, "third lands in column one");
    }

    #[test]
    fn a_wide_pane_still_spans_its_columns_after_migration() {
        let spots = migrate_flow(&[(2, "auto")], 2);
        assert!(
            spots[0].w > DEFAULT_W * 1.9,
            "a span-2 pane should stay about twice as wide, got {}",
            spots[0].w
        );
    }

    #[test]
    fn a_rectangle_from_the_browser_is_clamped_not_trusted() {
        let wild = Spot::new(-500.0, f64::NAN, 10.0, 99999.0);
        assert_eq!(wild.x, 0.0);
        assert_eq!(wild.y, 0.0);
        assert_eq!(wild.w, MIN_W);
        assert_eq!(wild.h, MAX_H);
    }
}
