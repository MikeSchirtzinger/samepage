//! Where chrome sits, decided once.
//!
//! The problem this replaces is four independently positioned panels that do
//! not know about each other. `.atlas-explanation-flow` and `.atlas-changes`
//! were both written at `right: 14px; top: 62px`, and the survivor was
//! whichever rendered second. A stylesheet cannot express "these two want the
//! same corner", so the collision was patched with a third rule
//! (`.atlas-view.has-explanation-flow .atlas-changes`) that moved one of them
//! somewhere else. That is a race with a bandage, not a layout.
//!
//! Rules, and where each came from:
//!
//! 1. Four surface kinds, declared, never inferred. Source: agentviz
//!    `shells/shared/panel-registry.js:1-28`.
//! 2. One expanded flyout per zone. Opening a second collapses the first to
//!    its summary line. Source: agentviz `shells/shared/icon-strip.js:26-31`
//!    and `:73-90`, where a strip of collapsed entries carries one open panel.
//! 3. One panel per zone may hold that edge unconditionally. A zone is a flex
//!    column, so several panels can stack in it without overlapping, but two
//!    DRAWERS in one zone are two panels that both claim the edge whatever
//!    else is happening, and that is the bug this module exists for: a
//!    constructor error, not a z-index race. No upstream source.
//! 4. Explicit z order when two want one edge, and the interactive one wins.
//!    Source: agentviz `specs/viz-notes/global.md` rule 6.
//! 5. Chrome must not lie. A control is gated on data, never on mode name.
//!    Source: agentviz `specs/viz-notes/chrome-audit.md`.
//! 6. A notice never covers the canvas centre. Enforced by the zone set: every
//!    zone is an edge.
//! 7. Panel state is browser local, never CRDT. That rule lives at the call
//!    site; this module is a pure function over what the caller says is open.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How a surface behaves, declared per panel rather than guessed from its
/// shape at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SurfaceKind {
    /// Toggled open, one expanded per zone, collapses to a summary line.
    Flyout,
    /// Holds an edge for as long as its data exists. Not toggled.
    Drawer,
    /// Full overlay, blocks the canvas. None yet; declared so the fourth kind
    /// is not invented ad hoc the first time one is needed.
    Modal,
    /// A section inside another panel. Never placed on its own.
    Inline,
}

/// The six edges chrome may occupy. Every one is an edge, which is rule 6
/// expressed as a type rather than as a review comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockZone {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomCentre,
    BottomRight,
    BottomStrip,
}

impl DockZone {
    /// The value the page writes into `data-zone`, and the selector the
    /// stylesheet positions. One name, so a zone cannot be renamed on one
    /// side of the boundary.
    pub fn slug(self) -> &'static str {
        match self {
            DockZone::TopLeft => "top-left",
            DockZone::TopRight => "top-right",
            DockZone::BottomLeft => "bottom-left",
            DockZone::BottomCentre => "bottom-centre",
            DockZone::BottomRight => "bottom-right",
            DockZone::BottomStrip => "bottom-strip",
        }
    }
}

/// What a panel needs from the document before it may render at all.
///
/// Rule 5 in one type: a panel is gated on data, never on a mode name. An
/// unmet requirement is why a panel is absent or disabled, and the reason is
/// carried through to the page so the human is told rather than left guessing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataRequirement {
    /// Always renderable.
    Always,
    /// A durable explanation flow exists on the document.
    Explanation,
    /// The document carries an agent-ink layer.
    AgentInk,
    /// Something changed since this human last looked.
    Changes,
    /// A constraint is broken.
    Diagnostics,
}

/// What the document actually holds right now. The page measures these; the
/// registry never inspects a document itself, which is what keeps it a pure
/// function that both sides can run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFacts {
    #[serde(default)]
    pub has_explanation: bool,
    #[serde(default)]
    pub has_agent_ink: bool,
    #[serde(default)]
    pub change_count: u32,
    #[serde(default)]
    pub broken_constraints: u32,
    /// True while the pane is too narrow to carry an expanded panel beside the
    /// canvas. Measured, not assumed: the announcement that covered the
    /// picture it was announcing did so at 320 CSS pixels over a 683 pixel
    /// pane.
    #[serde(default)]
    pub pane_is_cramped: bool,
}

impl DocumentFacts {
    fn satisfies(&self, requirement: DataRequirement) -> Result<(), &'static str> {
        match requirement {
            DataRequirement::Always => Ok(()),
            DataRequirement::Explanation => self
                .has_explanation
                .then_some(())
                .ok_or("no explanation flow on this document"),
            DataRequirement::AgentInk => self
                .has_agent_ink
                .then_some(())
                .ok_or("this document has no agent-ink layer"),
            DataRequirement::Changes => (self.change_count > 0)
                .then_some(())
                .ok_or("nothing has changed since you looked"),
            DataRequirement::Diagnostics => (self.broken_constraints > 0)
                .then_some(())
                .ok_or("no constraint is broken"),
        }
    }
}

/// One registered surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelEntry {
    pub id: String,
    pub title: String,
    pub kind: SurfaceKind,
    pub zone: DockZone,
    pub requires: DataRequirement,
    /// Higher wins the zone. Rule 4: the interactive surface beats the notice.
    pub z: u8,
}

/// What the page does with a panel this frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum PanelState {
    /// Rendered in full, holding its zone.
    Expanded,
    /// Rendered as its summary line only, because something else holds the
    /// zone. `behind` names what won, so the page can say so.
    Collapsed { behind: String },
    /// Rendered, but inert, with a reason a person can read.
    Disabled { reason: String },
    /// Not rendered. A control that cannot work is not drawn.
    Absent { reason: String },
}

/// A placed panel: its zone, its stacking order, and what to do with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedPanel {
    pub id: String,
    pub title: String,
    pub zone: String,
    pub z: u8,
    /// Position within the zone's strip, 0 for the expanded panel.
    pub slot: u8,
    #[serde(flatten)]
    pub state: PanelState,
}

impl PlacedPanel {
    pub fn is_rendered(&self) -> bool {
        !matches!(self.state, PanelState::Absent { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub panels: Vec<PlacedPanel>,
}

impl Layout {
    pub fn get(&self, id: &str) -> Option<&PlacedPanel> {
        self.panels.iter().find(|panel| panel.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelError {
    DuplicateId(String),
    /// Two entries want one zone and at least one of them cannot collapse.
    /// This is the `.atlas-explanation-flow` / `.atlas-changes` collision,
    /// caught at construction instead of at render.
    ZoneContested {
        zone: &'static str,
        first: String,
        second: String,
    },
}

impl std::fmt::Display for PanelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PanelError::DuplicateId(id) => write!(formatter, "two panels registered as {id:?}"),
            PanelError::ZoneContested {
                zone,
                first,
                second,
            } => write!(
                formatter,
                "{first:?} and {second:?} both hold the {zone} edge \
                 unconditionally. Pick a different zone, or make one of them a \
                 flyout so it can collapse; do not let z-index decide."
            ),
        }
    }
}

impl std::error::Error for PanelError {}

#[derive(Debug)]
pub struct PanelRegistry {
    entries: Vec<PanelEntry>,
}

impl PanelRegistry {
    /// Build a registry, refusing a layout that would race at render time.
    pub fn new(entries: Vec<PanelEntry>) -> Result<Self, PanelError> {
        let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
        for entry in &entries {
            if seen.insert(entry.id.as_str(), ()).is_some() {
                return Err(PanelError::DuplicateId(entry.id.clone()));
            }
        }
        for (index, entry) in entries.iter().enumerate() {
            for other in &entries[index + 1..] {
                if entry.zone != other.zone {
                    continue;
                }
                // Flyouts collapse and inlines are placed by their
                // container, so several of either stack in a zone's column
                // without either one covering the other. Two unconditional
                // holders of one edge is the case with no answer.
                let unconditional = |kind| matches!(kind, SurfaceKind::Drawer | SurfaceKind::Modal);
                if !unconditional(entry.kind) || !unconditional(other.kind) {
                    continue;
                }
                return Err(PanelError::ZoneContested {
                    zone: entry.zone.slug(),
                    first: entry.id.clone(),
                    second: other.id.clone(),
                });
            }
        }
        Ok(Self { entries })
    }

    /// The atlas's own chrome. One list, so "what panels are there" has an
    /// answer that is not "grep the stylesheet".
    pub fn atlas() -> Self {
        let entry = |id: &str, title: &str, kind, zone, requires, z| PanelEntry {
            id: id.to_string(),
            title: title.to_string(),
            kind,
            zone,
            requires,
            z,
        };
        Self::new(vec![
            entry(
                "agent-ink",
                "Agent ink",
                SurfaceKind::Flyout,
                DockZone::TopLeft,
                DataRequirement::AgentInk,
                6,
            ),
            // The walkthrough beats the notice for the same reason agentviz's
            // time bar beats its hotkey help: one is the thing being done, the
            // other is a remark about it.
            entry(
                "explanation-flow",
                "Explanation",
                SurfaceKind::Flyout,
                DockZone::TopRight,
                DataRequirement::Explanation,
                11,
            ),
            entry(
                "changes",
                "Changes since you looked",
                SurfaceKind::Flyout,
                DockZone::TopRight,
                DataRequirement::Changes,
                7,
            ),
            // The runtime's own presence tray. It is `position: fixed` to the
            // window by the surface crate, which is how it ended up sitting on
            // top of the broken-constraint strip. The zone takes it over.
            entry(
                "presence",
                "Who is here",
                SurfaceKind::Drawer,
                DockZone::BottomLeft,
                DataRequirement::Always,
                6,
            ),
            entry(
                "tool-toast",
                "Armed tool",
                SurfaceKind::Inline,
                DockZone::BottomLeft,
                DataRequirement::Always,
                8,
            ),
            entry(
                "gesture-card",
                "Canvas basics",
                SurfaceKind::Drawer,
                DockZone::BottomCentre,
                DataRequirement::Always,
                7,
            ),
            // A flyout rather than a drawer, so it shares the corner with the
            // zoom controls by collapsing rather than by outranking them, and
            // so a person who does not want a small copy of the board in
            // their canvas can put it away.
            entry(
                "minimap",
                "Board",
                SurfaceKind::Flyout,
                DockZone::BottomRight,
                DataRequirement::Always,
                4,
            ),
            entry(
                "zoom-controls",
                "Zoom",
                SurfaceKind::Drawer,
                DockZone::BottomRight,
                DataRequirement::Always,
                5,
            ),
            entry(
                "diagnostics",
                "Broken constraint",
                SurfaceKind::Drawer,
                DockZone::BottomStrip,
                DataRequirement::Diagnostics,
                9,
            ),
        ])
        .expect("the atlas registry is a literal; a contested zone here is a build error")
    }

    pub fn entries(&self) -> &[PanelEntry] {
        &self.entries
    }

    /// Place every registered panel for this frame.
    ///
    /// `open` is what the human has toggled on, browser-local state the core
    /// never owns. A panel that is open but whose data is gone is absent, not
    /// open: chrome must not lie.
    ///
    /// `focus` is the panel the human last reached for. Without it, z order
    /// alone decides the zone forever, and clicking the collapsed notice would
    /// do nothing at all: the walkthrough would win the edge again the same
    /// frame. z is the DEFAULT, focus is the human overriding it, and that
    /// asymmetry is the point. An agent starting a walkthrough does not steal
    /// an edge the human has just chosen, for the same reason a reveal does
    /// not take a camera the human is holding.
    pub fn layout_focused(
        &self,
        open: &[String],
        facts: &DocumentFacts,
        focus: Option<&str>,
    ) -> Layout {
        let mut placed: Vec<PlacedPanel> = Vec::with_capacity(self.entries.len());
        // Winner per zone: the highest-z renderable entry that wants to be
        // expanded. Decided before anything is placed, so a collapsed panel
        // can name what beat it.
        let mut winner: BTreeMap<DockZone, (&PanelEntry, u8)> = BTreeMap::new();
        for entry in &self.entries {
            // Only flyouts contest a zone. A drawer holds its edge for as long
            // as its data exists and never collapses, which is what makes it a
            // drawer; it stacks in the zone's column below whichever flyout is
            // open rather than fighting it.
            if entry.kind != SurfaceKind::Flyout || facts.satisfies(entry.requires).is_err() {
                continue;
            }
            if !open.iter().any(|id| id == &entry.id) {
                continue;
            }
            // A focused panel outranks anything z could say, which is why it
            // is scored as u8::MAX rather than compared separately.
            let rank = if focus == Some(entry.id.as_str()) {
                u8::MAX
            } else {
                entry.z
            };
            match winner.get(&entry.zone) {
                Some((_, held)) if *held >= rank => {}
                _ => {
                    winner.insert(entry.zone, (entry, rank));
                }
            }
        }

        let mut next_slot: BTreeMap<DockZone, u8> = BTreeMap::new();
        for entry in &self.entries {
            // A narrow canvas cannot keep independent left and right corners
            // from overlapping. Presence joins the zoom column and gets a slot.
            let zone = if facts.pane_is_cramped && entry.id == "presence" {
                DockZone::BottomRight
            } else {
                entry.zone
            };
            let state = match facts.satisfies(entry.requires) {
                Err(reason) => PanelState::Absent {
                    reason: reason.to_string(),
                },
                Ok(()) => match entry.kind {
                    // An inline section is placed by whatever contains it, a
                    // toast is not a panel anyone toggles, and a drawer holds
                    // its edge unconditionally. All three are simply rendered
                    // where their zone says.
                    SurfaceKind::Inline | SurfaceKind::Modal | SurfaceKind::Drawer => {
                        PanelState::Expanded
                    }
                    _ => match winner.get(&entry.zone) {
                        Some((held_by, _)) if held_by.id == entry.id => {
                            // Rule 6, measured rather than assumed: on a pane
                            // too narrow to carry a panel beside the canvas, an
                            // expanded flyout would be the canvas.
                            if facts.pane_is_cramped && entry.kind == SurfaceKind::Flyout {
                                PanelState::Collapsed {
                                    behind: "the canvas".to_string(),
                                }
                            } else {
                                PanelState::Expanded
                            }
                        }
                        Some((held_by, _)) => PanelState::Collapsed {
                            behind: held_by.id.clone(),
                        },
                        None => PanelState::Collapsed {
                            behind: String::new(),
                        },
                    },
                },
            };
            // Slot is the position in the zone's column. The flyout holding
            // the zone takes the top of it; everything else follows in
            // registry order, which is why a zone can carry a minimap above
            // its zoom controls without either one covering the other.
            let holds_the_zone =
                matches!(state, PanelState::Expanded) && entry.kind == SurfaceKind::Flyout;
            let slot = if holds_the_zone {
                0
            } else {
                let counter = next_slot.entry(zone).or_insert(1);
                let slot = *counter;
                if !matches!(state, PanelState::Absent { .. }) {
                    *counter += 1;
                }
                slot
            };
            placed.push(PlacedPanel {
                id: entry.id.clone(),
                title: entry.title.clone(),
                zone: zone.slug().to_string(),
                z: entry.z,
                slot,
                state,
            });
        }
        Layout { panels: placed }
    }

    /// `layout_focused` with nothing focused: z order alone.
    pub fn layout(&self, open: &[String], facts: &DocumentFacts) -> Layout {
        self.layout_focused(open, facts, None)
    }
}

impl Default for PanelRegistry {
    fn default() -> Self {
        Self::atlas()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: SurfaceKind, zone: DockZone, z: u8) -> PanelEntry {
        PanelEntry {
            id: id.to_string(),
            title: id.to_string(),
            kind,
            zone,
            requires: DataRequirement::Always,
            z,
        }
    }

    fn open(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    /// The bug this module exists for: two panels written at identical
    /// coordinates, with the survivor decided by render order.
    #[test]
    fn two_panels_claiming_one_zone_is_a_construction_error() {
        let error = PanelRegistry::new(vec![
            entry("explanation", SurfaceKind::Drawer, DockZone::TopRight, 11),
            entry("changes", SurfaceKind::Drawer, DockZone::TopRight, 7),
        ])
        .expect_err("a contested zone has to be refused");
        assert!(
            matches!(error, PanelError::ZoneContested { zone, .. } if zone == "top-right"),
            "{error}"
        );
        assert!(
            error.to_string().contains("do not let z-index decide"),
            "the error has to say what to do instead: {error}"
        );
    }

    #[test]
    fn a_drawer_and_a_flyout_stack_in_one_zone_without_covering_each_other() {
        PanelRegistry::new(vec![
            entry("presence", SurfaceKind::Drawer, DockZone::BottomLeft, 6),
            entry("toast", SurfaceKind::Inline, DockZone::BottomLeft, 8),
        ])
        .expect("a zone is a column; only two unconditional holders is the error");
    }

    #[test]
    fn flyouts_may_share_a_zone_because_they_collapse() {
        PanelRegistry::new(vec![
            entry("explanation", SurfaceKind::Flyout, DockZone::TopRight, 11),
            entry("changes", SurfaceKind::Flyout, DockZone::TopRight, 7),
        ])
        .expect("a flyout strip is the one legitimate way to share a zone");
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let error = PanelRegistry::new(vec![
            entry("changes", SurfaceKind::Flyout, DockZone::TopRight, 7),
            entry("changes", SurfaceKind::Flyout, DockZone::TopLeft, 7),
        ])
        .expect_err("two panels cannot answer to one name");
        assert!(matches!(error, PanelError::DuplicateId(id) if id == "changes"));
    }

    #[test]
    fn the_atlas_registry_builds_and_covers_every_zone_once() {
        let registry = PanelRegistry::atlas();
        let mut zones: Vec<&str> = registry
            .entries()
            .iter()
            .map(|entry| entry.zone.slug())
            .collect();
        zones.sort_unstable();
        zones.dedup();
        assert_eq!(
            zones,
            [
                "bottom-centre",
                "bottom-left",
                "bottom-right",
                "bottom-strip",
                "top-left",
                "top-right",
            ]
        );
    }

    #[test]
    fn one_flyout_is_expanded_per_zone_and_the_loser_names_the_winner() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            has_explanation: true,
            change_count: 3,
            ..DocumentFacts::default()
        };
        let layout = registry.layout(&open(&["explanation-flow", "changes"]), &facts);
        assert_eq!(
            layout.get("explanation-flow").unwrap().state,
            PanelState::Expanded,
            "the walkthrough is the interactive one and holds the edge"
        );
        assert_eq!(
            layout.get("changes").unwrap().state,
            PanelState::Collapsed {
                behind: "explanation-flow".to_string()
            }
        );
        assert_eq!(layout.get("explanation-flow").unwrap().slot, 0);
        assert_eq!(layout.get("changes").unwrap().slot, 1);
    }

    /// Without this, clicking the collapsed notice does nothing: the
    /// walkthrough wins the zone again on the same frame and the panel the
    /// human just asked for closes itself.
    #[test]
    fn the_human_reaching_for_a_collapsed_panel_takes_the_edge_from_z_order() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            has_explanation: true,
            change_count: 3,
            ..DocumentFacts::default()
        };
        let layout = registry.layout_focused(
            &open(&["explanation-flow", "changes"]),
            &facts,
            Some("changes"),
        );
        assert_eq!(layout.get("changes").unwrap().state, PanelState::Expanded);
        assert_eq!(
            layout.get("explanation-flow").unwrap().state,
            PanelState::Collapsed {
                behind: "changes".to_string()
            },
            "the walkthrough yields to the panel the human just opened"
        );
    }

    #[test]
    fn focusing_a_panel_whose_data_is_gone_changes_nothing() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            has_explanation: true,
            ..DocumentFacts::default()
        };
        let layout = registry.layout_focused(&open(&["explanation-flow"]), &facts, Some("changes"));
        assert_eq!(
            layout.get("explanation-flow").unwrap().state,
            PanelState::Expanded
        );
        assert!(matches!(
            layout.get("changes").unwrap().state,
            PanelState::Absent { .. }
        ));
    }

    #[test]
    fn the_notice_takes_the_edge_when_no_walkthrough_wants_it() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            change_count: 2,
            ..DocumentFacts::default()
        };
        let layout = registry.layout(&open(&["changes"]), &facts);
        assert_eq!(layout.get("changes").unwrap().state, PanelState::Expanded);
        assert!(matches!(
            layout.get("explanation-flow").unwrap().state,
            PanelState::Absent { .. }
        ));
    }

    #[test]
    fn a_panel_whose_data_is_gone_is_absent_with_a_reason() {
        let registry = PanelRegistry::atlas();
        // Open, and asking for a panel this document cannot support.
        let layout = registry.layout(&open(&["agent-ink"]), &DocumentFacts::default());
        let PanelState::Absent { reason } = &layout.get("agent-ink").unwrap().state else {
            panic!("an open panel with no data must not render");
        };
        assert_eq!(reason, "this document has no agent-ink layer");
    }

    #[test]
    fn a_closed_flyout_is_collapsed_rather_than_gone() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            has_agent_ink: true,
            ..DocumentFacts::default()
        };
        let layout = registry.layout(&[], &facts);
        assert!(
            matches!(
                layout.get("agent-ink").unwrap().state,
                PanelState::Collapsed { .. }
            ),
            "the summary line is how a closed flyout stays discoverable"
        );
    }

    #[test]
    fn a_cramped_pane_keeps_every_flyout_collapsed() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            change_count: 5,
            pane_is_cramped: true,
            ..DocumentFacts::default()
        };
        let layout = registry.layout(&open(&["changes"]), &facts);
        assert_eq!(
            layout.get("changes").unwrap().state,
            PanelState::Collapsed {
                behind: "the canvas".to_string()
            },
            "an announcement must not become the canvas it is announcing"
        );
    }

    #[test]
    fn narrow_presence_and_zoom_share_a_column_without_sharing_a_slot() {
        let registry = PanelRegistry::atlas();
        let layout = registry.layout(
            &open(&[]),
            &DocumentFacts {
                pane_is_cramped: true,
                ..DocumentFacts::default()
            },
        );
        let presence = layout.get("presence").expect("presence");
        let zoom = layout.get("zoom-controls").expect("zoom");
        assert_eq!(presence.zone, zoom.zone);
        assert_ne!(presence.slot, zoom.slot);
        assert_eq!(presence.state, PanelState::Expanded);
        assert_eq!(zoom.state, PanelState::Expanded);
    }

    #[test]
    fn the_minimap_and_the_zoom_controls_stack_in_one_corner() {
        let registry = PanelRegistry::atlas();
        let layout = registry.layout(&open(&["minimap"]), &DocumentFacts::default());
        assert_eq!(layout.get("minimap").unwrap().zone, "bottom-right");
        assert_eq!(layout.get("zoom-controls").unwrap().zone, "bottom-right");
        assert_eq!(
            layout.get("zoom-controls").unwrap().state,
            PanelState::Expanded,
            "a drawer keeps its edge; the flyout stacks in the column above it"
        );
        assert_eq!(layout.get("minimap").unwrap().state, PanelState::Expanded);
    }

    #[test]
    fn the_drawers_that_are_always_there_stay_expanded_and_alone_in_their_zones() {
        let registry = PanelRegistry::atlas();
        let layout = registry.layout(&[], &DocumentFacts::default());
        for id in ["zoom-controls", "gesture-card", "tool-toast"] {
            assert_eq!(
                layout.get(id).unwrap().state,
                PanelState::Expanded,
                "{id} is not toggled and its data is always there"
            );
        }
        // Slot 0 is whichever flyout holds the zone, so a drawer sharing a
        // zone with one sits below it rather than on top of it.
        assert_eq!(
            layout.get("zoom-controls").unwrap().slot,
            2,
            "the collapsed minimap chip sits between the corner and the controls"
        );
        assert_eq!(layout.get("gesture-card").unwrap().slot, 1);
        assert!(matches!(
            layout.get("diagnostics").unwrap().state,
            PanelState::Absent { .. }
        ));
    }

    #[test]
    fn a_broken_constraint_is_what_puts_the_strip_on_screen() {
        let registry = PanelRegistry::atlas();
        let facts = DocumentFacts {
            broken_constraints: 1,
            ..DocumentFacts::default()
        };
        let layout = registry.layout(&[], &facts);
        assert_eq!(
            layout.get("diagnostics").unwrap().state,
            PanelState::Expanded
        );
    }

    #[test]
    fn every_zone_is_an_edge() {
        // Rule 6 as a type check rather than a review comment: there is no
        // centre zone to dock a notice into.
        for zone in [
            DockZone::TopLeft,
            DockZone::TopRight,
            DockZone::BottomLeft,
            DockZone::BottomCentre,
            DockZone::BottomRight,
            DockZone::BottomStrip,
        ] {
            let slug = zone.slug();
            assert!(
                slug.starts_with("top-") || slug.starts_with("bottom-"),
                "{slug} is not on an edge"
            );
        }
    }
}
