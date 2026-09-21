//! Deterministic orthogonal routes around measured leaf cards.
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
};
type Point = (f64, f64);
type Bounds = (f64, f64, f64, f64);
const CLEARANCE: f64 = 12.0;

#[derive(Clone, Copy)]
struct Visit {
    cost: f64,
    index: usize,
}
impl PartialEq for Visit {
    fn eq(&self, other: &Self) -> bool {
        self.cost.total_cmp(&other.cost) == Ordering::Equal && self.index == other.index
    }
}
impl Eq for Visit {}
impl PartialOrd for Visit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Visit {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.index.cmp(&self.index))
    }
}
fn center(b: Bounds) -> Point {
    ((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0)
}
fn inside(p: Point, b: Bounds) -> bool {
    p.0 > b.0 && p.0 < b.2 && p.1 > b.1 && p.1 < b.3
}
fn clear(a: Point, b: Point, boxes: &[Bounds]) -> bool {
    !boxes.iter().any(|r| {
        if a.0 == b.0 {
            a.0 > r.0 && a.0 < r.2 && a.1.min(b.1) < r.3 && a.1.max(b.1) > r.1
        } else {
            a.1 > r.1 && a.1 < r.3 && a.0.min(b.0) < r.2 && a.0.max(b.0) > r.0
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}
impl Side {
    fn horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
    fn point(self, b: Bounds, fraction: f64) -> Point {
        let inset = 16.0_f64.min(if self.horizontal() {
            (b.3 - b.1) / 4.0
        } else {
            (b.2 - b.0) / 4.0
        });
        let x = b.0 + inset + (b.2 - b.0 - 2.0 * inset) * fraction;
        let y = b.1 + inset + (b.3 - b.1 - 2.0 * inset) * fraction;
        match self {
            Self::Left => (b.0, y),
            Self::Right => (b.2, y),
            Self::Top => (x, b.1),
            Self::Bottom => (x, b.3),
        }
    }
    fn outside(self, p: Point) -> Point {
        match self {
            Self::Left => (p.0 - CLEARANCE, p.1),
            Self::Right => (p.0 + CLEARANCE, p.1),
            Self::Top => (p.0, p.1 - CLEARANCE),
            Self::Bottom => (p.0, p.1 + CLEARANCE),
        }
    }
}

/// Stable identities, rather than projection order, break routing ties.
pub struct Connection<'a> {
    pub id: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub from_box: Bounds,
    pub to_box: Bounds,
}

fn sides(from: Bounds, to: Bounds) -> (Side, Side) {
    let (a, b) = (center(from), center(to));
    if from == to {
        return (Side::Right, Side::Top);
    }
    if (b.0 - a.0).abs() >= (b.1 - a.1).abs() {
        if b.0 >= a.0 {
            (Side::Right, Side::Left)
        } else {
            (Side::Left, Side::Right)
        }
    } else if b.1 >= a.1 {
        (Side::Bottom, Side::Top)
    } else {
        (Side::Top, Side::Bottom)
    }
}

/// Allocate separate ports to every incident relationship, including arrows
/// entering and leaving the same side. Route in stable edge order, reserving
/// segments so independent relationships cannot be mistaken for a shared bus.
struct PortRequest {
    connection: usize,
    end: usize,
    other: f64,
    bounds: Bounds,
}

fn valid_bounds(b: Bounds) -> bool {
    [b.0, b.1, b.2, b.3].iter().all(|v| v.is_finite()) && b.2 > b.0 && b.3 > b.1
}

pub fn routes(connections: &[Connection<'_>], obstacles: &[Bounds]) -> Vec<Vec<Point>> {
    let mut ports = vec![[(0.0, 0.0); 2]; connections.len()];
    let mut counts = vec![[0; 2]; connections.len()];
    let mut groups: BTreeMap<(&str, Side), Vec<PortRequest>> = BTreeMap::new();
    for (i, e) in connections.iter().enumerate() {
        if !valid_bounds(e.from_box) || !valid_bounds(e.to_box) {
            continue;
        }
        let (a, b) = sides(e.from_box, e.to_box);
        for (end, id, side, bounds, other) in [
            (0, e.from, a, e.from_box, e.to_box),
            (1, e.to, b, e.to_box, e.from_box),
        ] {
            let c = center(other);
            groups.entry((id, side)).or_default().push(PortRequest {
                connection: i,
                end,
                other: if side.horizontal() { c.1 } else { c.0 },
                bounds,
            });
        }
    }
    for ((_, side), mut group) in groups {
        group.sort_by(|a, b| {
            a.other
                .total_cmp(&b.other)
                .then_with(|| {
                    connections[a.connection]
                        .id
                        .cmp(connections[b.connection].id)
                })
                .then(a.end.cmp(&b.end))
        });
        let count = group.len();
        for (slot, request) in group.into_iter().enumerate() {
            counts[request.connection][request.end] = count;
            ports[request.connection][request.end] =
                side.point(request.bounds, (slot + 1) as f64 / (count + 1) as f64);
        }
    }
    // When both sides carry only this relationship, align nearby ports on a
    // common axis. Small differences in card height should not create a jog.
    for (i, e) in connections.iter().enumerate() {
        if counts[i] != [1, 1] {
            continue;
        }
        let (a, b) = sides(e.from_box, e.to_box);
        if a.horizontal() != b.horizontal() {
            continue;
        }
        let [start, end] = &mut ports[i];
        let (from, to) = if a.horizontal() {
            (&mut start.1, &mut end.1)
        } else {
            (&mut start.0, &mut end.0)
        };
        if (*from - *to).abs() <= CLEARANCE {
            let aligned = (*from + *to) / 2.0;
            let (low, high) = if a.horizontal() {
                (e.from_box.1.max(e.to_box.1), e.from_box.3.min(e.to_box.3))
            } else {
                (e.from_box.0.max(e.to_box.0), e.from_box.2.min(e.to_box.2))
            };
            if aligned > low && aligned < high {
                *from = aligned;
                *to = aligned;
            }
        }
    }
    let mut order: Vec<_> = (0..connections.len()).collect();
    order.sort_by_key(|&i| connections[i].id);
    let mut result = vec![Vec::new(); connections.len()];
    let mut reserved = Vec::new();
    for i in order {
        let e = &connections[i];
        if !valid_bounds(e.from_box) || !valid_bounds(e.to_box) {
            continue;
        }
        let (a, b) = sides(e.from_box, e.to_box);
        let route = between(ports[i][0], ports[i][1], a, b, obstacles, &reserved);
        reserved.extend(route.windows(2).map(|p| (p[0], p[1])));
        result[i] = route;
    }
    result
}

/// Empty when measured cards overlap or no unobstructed route exists.
pub fn orthogonal(from: Bounds, to: Bounds, obstacles: &[Bounds]) -> Vec<Point> {
    routes(
        &[Connection {
            id: "edge",
            from: "from",
            to: "to",
            from_box: from,
            to_box: to,
        }],
        obstacles,
    )
    .pop()
    .unwrap_or_default()
}

const LANE_GAP: f64 = 14.0;
const BEND_COST: f64 = 28.0;
const CROSSING_COST: f64 = 90.0;

/// Positive-length collinear sharing is a false junction, even if its two
/// arrows happen to point in the same direction.
pub fn shared_segment(a: Point, b: Point, c: Point, d: Point) -> bool {
    if a.0 == b.0 && c.0 == d.0 && a.0 == c.0 {
        a.1.max(b.1).min(c.1.max(d.1)) - a.1.min(b.1).max(c.1.min(d.1)) > 0.01
    } else if a.1 == b.1 && c.1 == d.1 && a.1 == c.1 {
        a.0.max(b.0).min(c.0.max(d.0)) - a.0.min(b.0).max(c.0.min(d.0)) > 0.01
    } else {
        false
    }
}
fn crosses(a: Point, b: Point, c: Point, d: Point) -> bool {
    if a.0 == b.0 && c.1 == d.1 {
        a.0 >= c.0.min(d.0) && a.0 <= c.0.max(d.0) && c.1 >= a.1.min(b.1) && c.1 <= a.1.max(b.1)
    } else if a.1 == b.1 && c.0 == d.0 {
        crosses(c, d, a, b)
    } else {
        false
    }
}

fn between(
    start: Point,
    end: Point,
    from_side: Side,
    to_side: Side,
    obstacles: &[Bounds],
    reserved: &[(Point, Point)],
) -> Vec<Point> {
    if ![start.0, start.1, end.0, end.1]
        .iter()
        .all(|v| v.is_finite())
        || obstacles.iter().any(|b| {
            ![b.0, b.1, b.2, b.3].iter().all(|v| v.is_finite()) || b.2 <= b.0 || b.3 <= b.1
        })
    {
        return Vec::new();
    }
    let outer_start = from_side.outside(start);
    let outer_end = to_side.outside(end);
    let boxes: Vec<_> = obstacles
        .iter()
        .map(|b| {
            (
                b.0 - CLEARANCE,
                b.1 - CLEARANCE,
                b.2 + CLEARANCE,
                b.3 + CLEARANCE,
            )
        })
        .collect();
    if boxes
        .iter()
        .any(|b| inside(outer_start, *b) || inside(outer_end, *b))
    {
        return Vec::new();
    }
    let mut xs = vec![outer_start.0, outer_end.0];
    let mut ys = vec![outer_start.1, outer_end.1];
    for b in &boxes {
        xs.extend([b.0, b.2]);
        ys.extend([b.1, b.3]);
    }
    for (a, b) in reserved {
        for p in [a, b] {
            xs.extend([p.0 - LANE_GAP, p.0, p.0 + LANE_GAP]);
            ys.extend([p.1 - LANE_GAP, p.1, p.1 + LANE_GAP]);
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    ys.sort_by(f64::total_cmp);
    ys.dedup();
    // Bound work for very large imported diagrams. An unavailable route remains
    // explicit, rather than blocking interaction or drawing through a card.
    let count = xs.len().saturating_mul(ys.len());
    if count > 65_536 {
        return Vec::new();
    }
    let point = |index: usize| (xs[index % xs.len()], ys[index / xs.len()]);
    let index = |p: Point| {
        ys.iter()
            .position(|y| *y == p.1)
            .zip(xs.iter().position(|x| *x == p.0))
            .map(|(y, x)| y * xs.len() + x)
    };
    let (Some(first), Some(last)) = (index(outer_start), index(outer_end)) else {
        return Vec::new();
    };
    let heuristic = |index: usize| {
        let p = point(index);
        (p.0 - outer_end.0).abs() + (p.1 - outer_end.1).abs()
    };
    // Track arrival orientation so a shorter zigzag is not preferred to a
    // slightly longer, readable route with fewer turns.
    let initial = first * 2 + usize::from(!from_side.horizontal());
    let terminal_axis = usize::from(!to_side.horizontal());
    let mut costs = vec![f64::INFINITY; count * 2];
    let mut previous = vec![None; count * 2];
    let mut queue = BinaryHeap::new();
    costs[initial] = 0.0;
    queue.push(Visit {
        cost: heuristic(first),
        index: initial,
    });
    let mut finish = None;
    while let Some(Visit { cost, index: state }) = queue.pop() {
        let current = state / 2;
        if cost > costs[state] + heuristic(current) {
            continue;
        }
        if current == last {
            finish = Some(state);
            break;
        }
        let (x, y) = (current % xs.len(), current / xs.len());
        let neighbors = [
            x.checked_sub(1).map(|x| y * xs.len() + x),
            (x + 1 < xs.len()).then_some(current + 1),
            y.checked_sub(1).map(|y| y * xs.len() + x),
            (y + 1 < ys.len()).then_some(current + xs.len()),
        ];
        for next in neighbors.into_iter().flatten() {
            let (a, b) = (point(current), point(next));
            if !clear(a, b, &boxes) || reserved.iter().any(|&(c, d)| shared_segment(a, b, c, d)) {
                continue;
            }
            let axis = usize::from(a.0 == b.0);
            let next_state = next * 2 + axis;
            let bends =
                usize::from(axis != state % 2) + usize::from(next == last && axis != terminal_axis);
            let crossings = reserved
                .iter()
                .filter(|&&(c, d)| crosses(a, b, c, d))
                .count();
            let next_cost = costs[state]
                + (b.0 - a.0).abs()
                + (b.1 - a.1).abs()
                + bends as f64 * BEND_COST
                + crossings as f64 * CROSSING_COST;
            if next_cost < costs[next_state] {
                costs[next_state] = next_cost;
                previous[next_state] = Some(state);
                queue.push(Visit {
                    cost: next_cost + heuristic(next),
                    index: next_state,
                });
            }
        }
    }
    let Some(mut cursor) = finish else {
        return Vec::new();
    };
    let mut route = vec![end, outer_end];
    while cursor != initial {
        let Some(parent) = previous[cursor] else {
            return Vec::new();
        };
        cursor = parent;
        route.push(point(cursor / 2));
    }
    route.push(start);
    route.reverse();
    route.dedup();
    let mut simplified: Vec<Point> = Vec::new();
    for p in route {
        while simplified.len() >= 2 {
            let a = simplified[simplified.len() - 2];
            let b = simplified[simplified.len() - 1];
            if (a.0 == b.0 && b.0 == p.0) || (a.1 == b.1 && b.1 == p.1) {
                simplified.pop();
            } else {
                break;
            }
        }
        simplified.push(p);
    }
    simplified
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared_count(paths: &[Vec<Point>]) -> usize {
        paths
            .iter()
            .enumerate()
            .map(|(i, a)| {
                paths
                    .iter()
                    .skip(i + 1)
                    .map(|b| {
                        a.windows(2)
                            .flat_map(|x| {
                                b.windows(2)
                                    .map(move |y| shared_segment(x[0], x[1], y[0], y[1]))
                            })
                            .filter(|v| *v)
                            .count()
                    })
                    .sum::<usize>()
            })
            .sum()
    }

    #[test]
    fn rearranged_code_map_has_separate_ports_and_no_false_junctions() {
        // Captured measured geometry from the shared code map after its cards
        // were rearranged. This is a regression fixture, not runtime evidence.
        let boxes = [
            (614.0, -205.0, 894.0, -56.0),
            (1018.0, -203.0, 1298.0, -56.0),
            (141.0, -100.0, 421.0, 30.0),
            (142.0, 219.0, 422.0, 368.0),
            (616.0, 219.0, 896.0, 366.0),
        ];
        let names = ["host", "surface", "canvas", "web", "core"];
        let pairs = [(0, 2), (0, 1), (0, 4), (4, 2), (3, 2), (3, 4)];
        let ids = ["a", "b", "c", "d", "e", "f"];
        let edges: Vec<_> = pairs
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| Connection {
                id: ids[i],
                from: names[a],
                to: names[b],
                from_box: boxes[a],
                to_box: boxes[b],
            })
            .collect();
        let independent: Vec<_> = edges
            .iter()
            .map(|e| orthogonal(e.from_box, e.to_box, &boxes))
            .collect();
        assert!(
            shared_count(&independent) > 0,
            "The fixture must expose the old independent routing defect"
        );
        let joint = routes(&edges, &boxes);
        assert_eq!(
            shared_count(&joint),
            0,
            "Distinct relationships must not merge into a shared segment"
        );
        let mut ports: BTreeMap<&str, Vec<Point>> = BTreeMap::new();
        for (edge, path) in edges.iter().zip(&joint) {
            assert!(path.len() >= 2, "{} must remain routable", edge.id);
            ports.entry(edge.from).or_default().push(path[0]);
            ports
                .entry(edge.to)
                .or_default()
                .push(*path.last().unwrap());
            for pair in path.windows(2) {
                assert!(pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1);
                assert!(clear(pair[0], pair[1], &boxes));
            }
        }
        for points in ports.values() {
            for (i, a) in points.iter().enumerate() {
                for b in points.iter().skip(i + 1) {
                    assert!(
                        (a.0 - b.0).hypot(a.1 - b.1) >= 12.0,
                        "Ports must remain distinguishable"
                    );
                }
            }
        }
        let reversed: Vec<_> = edges.into_iter().rev().collect();
        let back = routes(&reversed, &boxes);
        assert_eq!(
            joint,
            back.into_iter().rev().collect::<Vec<_>>(),
            "Projection order must not change routing"
        );
    }

    #[test]
    fn a_self_link_uses_two_distinct_ports_and_stays_outside_the_card() {
        let b = (0.0, 0.0, 280.0, 150.0);
        let route = routes(
            &[Connection {
                id: "self",
                from: "n",
                to: "n",
                from_box: b,
                to_box: b,
            }],
            &[b],
        )
        .remove(0);
        assert!(route.len() >= 4);
        assert_ne!(route.first(), route.last());
        assert!(route.windows(2).all(|p| clear(p[0], p[1], &[b])));
    }

    #[test]
    fn near_alignment_never_moves_a_port_off_a_short_card() {
        let a = (0.0, 0.0, 120.0, 2.0);
        let b = (300.0, 12.0, 420.0, 14.0);
        let path = orthogonal(a, b, &[a, b]);
        assert_eq!(path.first(), Some(&(120.0, 1.0)));
        assert_eq!(path.last(), Some(&(300.0, 13.0)));
    }

    #[test]
    fn near_alignment_removes_tiny_jogs_without_moving_cards() {
        let a = (0.0, 0.0, 280.0, 150.0);
        let b = (500.0, 2.0, 780.0, 152.0);
        assert_eq!(
            orthogonal(a, b, &[a, b]),
            vec![(280.0, 76.0), (500.0, 76.0)]
        );
    }

    #[test]
    fn aligned_cards_keep_a_straight_route() {
        let a = (0.0, 0.0, 280.0, 150.0);
        let b = (500.0, 0.0, 780.0, 150.0);
        assert_eq!(
            orthogonal(a, b, &[a, b]),
            vec![(280.0, 75.0), (500.0, 75.0)]
        );
    }

    #[test]
    fn detours_around_measured_cards_and_is_deterministic() {
        let a = (0.0, 0.0, 100.0, 80.0);
        let b = (400.0, 0.0, 500.0, 80.0);
        let blocker = (180.0, -30.0, 280.0, 100.0);
        let boxes = [a, b, blocker];
        let route = orthogonal(a, b, &boxes);
        assert!(route.len() >= 4);
        assert_eq!(route.first(), Some(&(100.0, 40.0)));
        assert_eq!(route.last(), Some(&(400.0, 40.0)));
        for pair in route.windows(2) {
            assert!(pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1);
            assert!(clear(pair[0], pair[1], &[blocker]));
        }
        assert_eq!(route, orthogonal(a, b, &boxes));
    }
    #[test]
    fn moved_and_resized_cards_rebind_endpoints() {
        let a = (0.0, 0.0, 200.0, 120.0);
        let b = (450.0, 250.0, 650.0, 350.0);
        let route = orthogonal(a, b, &[a, b]);
        assert_eq!(route.first(), Some(&(200.0, 60.0)));
        assert_eq!(route.last(), Some(&(450.0, 300.0)));
    }
    #[test]
    fn inverted_endpoint_boxes_are_rejected_without_an_obstacle_list() {
        let valid = (0.0, 0.0, 100.0, 100.0);
        let invalid = (200.0, 0.0, 100.0, 100.0);
        assert!(orthogonal(valid, invalid, &[]).is_empty());
        assert!(orthogonal(invalid, valid, &[]).is_empty());
    }

    #[test]
    fn overlapping_and_invalid_boxes_are_explicitly_unroutable() {
        let a = (0.0, 0.0, 100.0, 100.0);
        let b = (50.0, 0.0, 150.0, 100.0);
        assert!(orthogonal(a, b, &[a, b]).is_empty());
        assert!(orthogonal(a, (f64::NAN, 0.0, 5.0, 5.0), &[]).is_empty());
    }
}
