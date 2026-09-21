//! What a card shows at this zoom, and how many labels a board may carry.
//!
//! Ported from agentviz `crates/graph_core/src/labels.rs`, HEAD `6c7d808`.
//! Two of its three passes port directly and keep their tests:
//! `budget_by_importance` (pick top-K by importance) and
//! `pick_non_overlapping_screen` (greedy screen-space de-clutter).
//!
//! Its third pass, `compute_visible_labels`, does not: it culls against a
//! column-major 4x4 view-projection matrix, which is the right cull for a
//! 3D orbit rig and dead code on a 2D canvas whose camera is a translate and
//! a scale. `cull_to_viewport` replaces it with the 2D form, keeping the same
//! 20% margin so a label just off the edge still draws instead of popping in
//! mid-pan.
//!
//! The atlas addition is `DetailLevel` and `detail_for`. The finding it
//! answers: at a whole-board fit every card rendered its title, body, and
//! source at a few pixels a line, so the cards became the picture and the
//! sketch became noise behind them.

use serde::{Deserialize, Serialize};

/// Which parts of a card render at the current zoom.
///
/// The thresholds are the design's table, and they are about legibility
/// rather than taste: below 0.45 a body line is under six pixels tall on this
/// renderer, which is texture, not text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetailLevel {
    /// Title only. The card is a labelled block in a picture.
    Title,
    /// Title and body.
    Body,
    /// Title, body, and the source reference.
    Source,
}

/// Below this, a card is its title and nothing else.
pub const BODY_SCALE: f64 = 0.45;
/// Above this, the source reference earns its line.
pub const SOURCE_SCALE: f64 = 1.0;

/// What this card shows at this scale.
///
/// Selection overrides the zoom: a card the human has picked out shows
/// everything it has, wherever the camera happens to be. Asking to see a card
/// and being shown three words of it is the surface arguing with the person.
pub fn detail_for(scale: f64, selected: bool) -> DetailLevel {
    if selected {
        return DetailLevel::Source;
    }
    if !scale.is_finite() || scale < BODY_SCALE {
        DetailLevel::Title
    } else if scale > SOURCE_SCALE {
        DetailLevel::Source
    } else {
        DetailLevel::Body
    }
}

/// Result of a budgeting pass: which labels should render.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelBudget {
    /// `visible[i]` is true iff label `i` should render this frame.
    pub visible: Vec<bool>,
    /// How many labels the budget actually emitted.
    pub emitted: usize,
}

/// Tunables for `budget_by_importance`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LabelPolicy {
    /// Fraction of labels that may render, 0.0 to 1.0.
    pub density: f64,
    /// Hard ceiling regardless of `density * n`.
    pub cap: usize,
    /// Minimum to emit when at least one is renderable. Stops a whole-board
    /// shot of a large graph from going blank.
    pub floor: usize,
    /// Breathing room added to every half-extent in the screen-space pass. A
    /// few pixels reads dramatically cleaner than mathematically not quite
    /// overlapping.
    pub padding_px: f64,
}

impl Default for LabelPolicy {
    fn default() -> Self {
        Self {
            density: 1.0,
            cap: 512,
            floor: 1,
            padding_px: 2.0,
        }
    }
}

/// Pick the top-K by importance and mark them visible.
///
/// `renderable[i] = false` excludes label `i`. An empty `renderable` means
/// everything is a candidate.
pub fn budget_by_importance(
    importance: &[f64],
    renderable: &[bool],
    policy: &LabelPolicy,
) -> LabelBudget {
    let n = importance.len();
    let mut visible = vec![false; n];
    if n == 0 {
        return LabelBudget {
            visible,
            emitted: 0,
        };
    }

    let budget = ((n as f64) * policy.density.clamp(0.0, 1.0)).ceil() as usize;
    let budget = budget.max(policy.floor).min(policy.cap).min(n);

    let mut candidates: Vec<usize> = (0..n)
        .filter(|index| renderable.get(*index).copied().unwrap_or(true))
        .collect();
    // Highest importance first. `total_cmp` rather than `partial_cmp` so ties
    // and NaN are ordered rather than arbitrary, which is what keeps a
    // re-render from shuffling which labels survive.
    candidates.sort_by(|a, b| importance[*b].total_cmp(&importance[*a]));

    let mut emitted = 0;
    for index in candidates {
        if emitted >= budget {
            break;
        }
        visible[index] = true;
        emitted += 1;
    }

    LabelBudget { visible, emitted }
}

/// One label's axis-aligned box in screen pixels.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LabelScreenRect {
    /// Centre in pixels. `None` means no on-screen anchor, which excludes it.
    pub anchor: Option<[f64; 2]>,
    /// Half-width and half-height in pixels.
    pub half: [f64; 2],
}

/// Cull to what the pane can show, with a 20% margin.
///
/// The 2D replacement for agentviz's projection cull. The margin is the same
/// number and exists for the same reason: a label just off the edge keeps
/// drawing so it does not pop in halfway through a pan.
pub fn cull_to_viewport(
    rects: &[LabelScreenRect],
    width: f64,
    height: f64,
    renderable: &[bool],
) -> Vec<bool> {
    let margin_x = width * 0.2;
    let margin_y = height * 0.2;
    rects
        .iter()
        .enumerate()
        .map(|(index, rect)| {
            if !renderable.get(index).copied().unwrap_or(true) {
                return false;
            }
            let Some(anchor) = rect.anchor else {
                return false;
            };
            anchor[0] >= -margin_x
                && anchor[0] <= width + margin_x
                && anchor[1] >= -margin_y
                && anchor[1] <= height + margin_y
        })
        .collect()
}

/// Greedy non-overlapping pick, highest importance first.
///
/// The standard label de-clutter pass: at a dense zoom you get fewer, more
/// readable labels, and zooming in brings the rest back as their screen rects
/// stop colliding.
pub fn pick_non_overlapping_screen(
    rects: &[LabelScreenRect],
    importance: &[f64],
    renderable: &[bool],
    padding_px: f64,
) -> LabelBudget {
    let n = rects.len();
    let mut visible = vec![false; n];
    if n == 0 {
        return LabelBudget {
            visible,
            emitted: 0,
        };
    }

    let mut candidates: Vec<usize> = (0..n)
        .filter(|index| rects[*index].anchor.is_some())
        .filter(|index| renderable.get(*index).copied().unwrap_or(true))
        .collect();
    candidates.sort_by(|a, b| {
        let left = importance.get(*a).copied().unwrap_or(0.0);
        let right = importance.get(*b).copied().unwrap_or(0.0);
        right.total_cmp(&left)
    });

    let mut kept: Vec<(f64, f64, f64, f64)> = Vec::with_capacity(candidates.len());
    let pad = padding_px.max(0.0);

    for index in candidates {
        let rect = rects[index];
        let Some(centre) = rect.anchor else { continue };
        let half_x = rect.half[0] + pad;
        let half_y = rect.half[1] + pad;
        let (min_x, min_y) = (centre[0] - half_x, centre[1] - half_y);
        let (max_x, max_y) = (centre[0] + half_x, centre[1] + half_y);

        let collides = kept.iter().any(|(kept_x0, kept_y0, kept_x1, kept_y1)| {
            !(max_x <= *kept_x0 || min_x >= *kept_x1 || max_y <= *kept_y0 || min_y >= *kept_y1)
        });

        if !collides {
            kept.push((min_x, min_y, max_x, max_y));
            visible[index] = true;
        }
    }

    let emitted = visible.iter().filter(|value| **value).count();
    LabelBudget { visible, emitted }
}

/// One card's answer for this frame: what to show, and whether to show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabelPlan {
    pub id: String,
    pub detail: DetailLevel,
    pub visible: bool,
}

/// One card as the page describes it.
#[derive(Clone, Debug, Deserialize)]
pub struct LabelCandidate {
    pub id: String,
    /// Screen rectangle, already projected. `None` anchor means off screen.
    #[serde(flatten)]
    pub rect: LabelScreenRect,
    #[serde(default)]
    pub importance: f64,
    #[serde(default)]
    pub selected: bool,
}

/// The whole pass: cull, budget, de-clutter, and say what each card shows.
pub fn plan(
    cards: &[LabelCandidate],
    scale: f64,
    width: f64,
    height: f64,
    policy: &LabelPolicy,
) -> Vec<LabelPlan> {
    let rects: Vec<LabelScreenRect> = cards.iter().map(|card| card.rect).collect();
    let importance: Vec<f64> = cards
        .iter()
        // A selected card is the most important thing on the board by
        // definition: it is what the human just pointed at.
        .map(|card| {
            if card.selected {
                f64::INFINITY
            } else {
                card.importance
            }
        })
        .collect();
    let on_screen = cull_to_viewport(&rects, width, height, &[]);
    let budgeted = budget_by_importance(&importance, &on_screen, policy);
    let picked =
        pick_non_overlapping_screen(&rects, &importance, &budgeted.visible, policy.padding_px);
    cards
        .iter()
        .enumerate()
        .map(|(index, card)| LabelPlan {
            id: card.id.clone(),
            detail: detail_for(scale, card.selected),
            // A selected card is never de-cluttered away. Hiding the thing
            // someone just clicked is the surface arguing with them.
            visible: card.selected || picked.visible[index],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the atlas's own rule ────────────────────────────────────────

    #[test]
    fn a_board_zoomed_out_is_titles() {
        assert_eq!(detail_for(0.2, false), DetailLevel::Title);
        assert_eq!(detail_for(0.44, false), DetailLevel::Title);
    }

    #[test]
    fn a_readable_zoom_adds_the_body() {
        assert_eq!(detail_for(BODY_SCALE, false), DetailLevel::Body);
        assert_eq!(detail_for(0.81, false), DetailLevel::Body);
        assert_eq!(detail_for(SOURCE_SCALE, false), DetailLevel::Body);
    }

    #[test]
    fn past_full_size_the_source_earns_its_line() {
        assert_eq!(detail_for(1.01, false), DetailLevel::Source);
        assert_eq!(detail_for(2.4, false), DetailLevel::Source);
    }

    #[test]
    fn a_selected_card_shows_everything_at_any_zoom() {
        assert_eq!(detail_for(0.1, true), DetailLevel::Source);
        assert_eq!(detail_for(2.4, true), DetailLevel::Source);
    }

    #[test]
    fn a_broken_scale_falls_back_to_the_title() {
        assert_eq!(detail_for(f64::NAN, false), DetailLevel::Title);
    }

    // ── ported from agentviz, same assertions ───────────────────────

    #[test]
    fn density_zero_still_emits_floor() {
        let importance = vec![0.1, 0.9, 0.5];
        let policy = LabelPolicy {
            density: 0.0,
            cap: 100,
            floor: 1,
            padding_px: 0.0,
        };
        let budget = budget_by_importance(&importance, &[], &policy);
        assert_eq!(budget.emitted, 1);
        assert!(budget.visible[1], "the floor keeps the most important one");
    }

    #[test]
    fn density_one_emits_all() {
        let importance = vec![0.1, 0.9, 0.5];
        let policy = LabelPolicy {
            density: 1.0,
            cap: 100,
            floor: 1,
            padding_px: 0.0,
        };
        let budget = budget_by_importance(&importance, &[], &policy);
        assert_eq!(budget.emitted, 3);
        assert!(budget.visible.iter().all(|value| *value));
    }

    #[test]
    fn cap_overrides_density() {
        let importance = vec![1.0; 10];
        let policy = LabelPolicy {
            density: 1.0,
            cap: 3,
            floor: 1,
            padding_px: 0.0,
        };
        assert_eq!(budget_by_importance(&importance, &[], &policy).emitted, 3);
    }

    #[test]
    fn renderable_filter_skips_excluded_nodes() {
        let importance = vec![0.9, 0.8, 0.7, 0.6];
        let renderable = vec![false, true, true, true];
        let policy = LabelPolicy {
            density: 1.0,
            cap: 100,
            floor: 1,
            padding_px: 0.0,
        };
        let budget = budget_by_importance(&importance, &renderable, &policy);
        assert!(
            !budget.visible[0],
            "excluded despite the highest importance"
        );
        assert!(budget.visible[1] && budget.visible[2] && budget.visible[3]);
        assert_eq!(budget.emitted, 3);
    }

    #[test]
    fn picks_top_k_by_importance() {
        let importance = vec![0.1, 0.5, 0.9, 0.3, 0.7];
        let policy = LabelPolicy {
            density: 0.5,
            cap: 100,
            floor: 1,
            padding_px: 0.0,
        };
        // 0.5 * 5 = 2.5, ceil 3.
        let budget = budget_by_importance(&importance, &[], &policy);
        assert_eq!(budget.emitted, 3);
        assert!(budget.visible[2] && budget.visible[4] && budget.visible[1]);
        assert!(!budget.visible[0] && !budget.visible[3]);
    }

    #[test]
    fn screen_pick_drops_lower_importance_overlap() {
        let rects = vec![
            LabelScreenRect {
                anchor: Some([100.0, 100.0]),
                half: [20.0, 8.0],
            },
            LabelScreenRect {
                anchor: Some([100.0, 100.0]),
                half: [20.0, 8.0],
            },
        ];
        let budget = pick_non_overlapping_screen(&rects, &[0.3, 0.9], &[], 0.0);
        assert_eq!(budget.emitted, 1);
        assert!(budget.visible[1] && !budget.visible[0]);
    }

    #[test]
    fn screen_pick_keeps_separated_labels() {
        let rects = vec![
            LabelScreenRect {
                anchor: Some([10.0, 10.0]),
                half: [20.0, 8.0],
            },
            LabelScreenRect {
                anchor: Some([500.0, 500.0]),
                half: [20.0, 8.0],
            },
        ];
        let budget = pick_non_overlapping_screen(&rects, &[0.5, 0.5], &[], 0.0);
        assert_eq!(budget.emitted, 2);
    }

    #[test]
    fn screen_pick_padding_pushes_neighbors_apart() {
        let rects = vec![
            LabelScreenRect {
                anchor: Some([0.0, 0.0]),
                half: [10.0, 5.0],
            },
            LabelScreenRect {
                anchor: Some([20.0, 0.0]),
                half: [10.0, 5.0],
            },
        ];
        let importance = vec![0.9, 0.5];
        assert_eq!(
            pick_non_overlapping_screen(&rects, &importance, &[], 0.0).emitted,
            2,
            "touching but not overlapping"
        );
        let padded = pick_non_overlapping_screen(&rects, &importance, &[], 2.0);
        assert_eq!(padded.emitted, 1);
        assert!(padded.visible[0] && !padded.visible[1]);
    }

    #[test]
    fn screen_pick_skips_offscreen_anchors() {
        let rects = vec![
            LabelScreenRect {
                anchor: None,
                half: [20.0, 8.0],
            },
            LabelScreenRect {
                anchor: Some([100.0, 100.0]),
                half: [20.0, 8.0],
            },
        ];
        let budget = pick_non_overlapping_screen(&rects, &[0.9, 0.1], &[], 0.0);
        assert_eq!(budget.emitted, 1);
        assert!(budget.visible[1] && !budget.visible[0]);
    }

    #[test]
    fn screen_pick_respects_renderable_mask() {
        let rects = vec![
            LabelScreenRect {
                anchor: Some([0.0, 0.0]),
                half: [5.0, 5.0],
            },
            LabelScreenRect {
                anchor: Some([100.0, 100.0]),
                half: [5.0, 5.0],
            },
        ];
        let budget = pick_non_overlapping_screen(&rects, &[0.9, 0.5], &[false, true], 0.0);
        assert_eq!(budget.emitted, 1);
        assert!(budget.visible[1] && !budget.visible[0]);
    }

    // ── the 2D replacement for the projection cull ──────────────────

    #[test]
    fn the_viewport_cull_keeps_a_margin_past_the_edge() {
        let rects = vec![
            LabelScreenRect {
                anchor: Some([500.0, 400.0]),
                half: [10.0, 5.0],
            },
            // Just outside the pane, inside the 20% margin: still drawn, so it
            // does not pop in halfway through a pan.
            LabelScreenRect {
                anchor: Some([1100.0, 400.0]),
                half: [10.0, 5.0],
            },
            LabelScreenRect {
                anchor: Some([4000.0, 400.0]),
                half: [10.0, 5.0],
            },
            LabelScreenRect {
                anchor: None,
                half: [10.0, 5.0],
            },
        ];
        assert_eq!(
            cull_to_viewport(&rects, 1000.0, 800.0, &[]),
            vec![true, true, false, false]
        );
    }

    // ── the whole pass ──────────────────────────────────────────────

    #[test]
    fn the_plan_says_what_each_card_shows_and_whether_it_shows() {
        let card = |id: &str, x: f64, importance: f64, selected: bool| LabelCandidate {
            id: id.to_string(),
            rect: LabelScreenRect {
                anchor: Some([x, 100.0]),
                half: [40.0, 10.0],
            },
            importance,
            selected,
        };
        let cards = vec![
            card("a", 100.0, 0.9, false),
            // Right on top of "a": one of them has to go.
            card("b", 100.0, 0.1, false),
            card("c", 600.0, 0.5, false),
        ];
        let plan_at_board = plan(&cards, 0.3, 1000.0, 800.0, &LabelPolicy::default());
        assert_eq!(plan_at_board[0].detail, DetailLevel::Title);
        assert!(plan_at_board[0].visible);
        assert!(!plan_at_board[1].visible, "the overlapped one yields");
        assert!(plan_at_board[2].visible);

        let plan_close = plan(&cards, 1.2, 1000.0, 800.0, &LabelPolicy::default());
        assert_eq!(plan_close[0].detail, DetailLevel::Source);
    }

    #[test]
    fn a_selected_card_survives_the_declutter_it_would_otherwise_lose() {
        let cards = vec![
            LabelCandidate {
                id: "loud".to_string(),
                rect: LabelScreenRect {
                    anchor: Some([100.0, 100.0]),
                    half: [40.0, 10.0],
                },
                importance: 0.99,
                selected: false,
            },
            LabelCandidate {
                id: "picked".to_string(),
                rect: LabelScreenRect {
                    anchor: Some([100.0, 100.0]),
                    half: [40.0, 10.0],
                },
                importance: 0.0,
                selected: true,
            },
        ];
        let plan = plan(&cards, 0.2, 1000.0, 800.0, &LabelPolicy::default());
        assert!(
            plan[1].visible,
            "hiding the card someone just clicked is the surface arguing with them"
        );
        assert_eq!(plan[1].detail, DetailLevel::Source);
    }

    #[test]
    fn an_empty_board_plans_nothing_rather_than_panicking() {
        assert!(plan(&[], 1.0, 1000.0, 800.0, &LabelPolicy::default()).is_empty());
        assert_eq!(
            budget_by_importance(&[], &[], &LabelPolicy::default()).emitted,
            0
        );
        assert_eq!(pick_non_overlapping_screen(&[], &[], &[], 2.0).emitted, 0);
    }
}
