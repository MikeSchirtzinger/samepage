//! The shared atlas model.
//!
//! One CRDT document ([`ag_ui_canvas::scene::Scene`]), one projection, one
//! mutation vocabulary — compiled twice. The host links this crate natively so
//! the agent's actions write through it; `same-page-atlas-web` links the exact
//! same code to `wasm32-unknown-unknown` so the human's pointer writes through
//! it too. Neither side owns a private shape of the artifact, which is the
//! whole point: "on the same page" has to be true at the type level before it
//! can be true on screen.
//!
//! Layout in the CRDT: `Scene`'s root `objects` map, one nested map per
//! object, flat scalar properties. Property writes are per-key
//! last-write-wins, so a human dragging `x`/`y` and an agent writing `note` on
//! the SAME node concurrently both survive the merge — see `convergence` in
//! this crate's tests.

use ag_ui_canvas::ids::ObjectId;
use ag_ui_canvas::scene::{Author, PropValue, Scene};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest as _, Sha256};
use yrs::{Any, Map, Out, Transact};

#[path = "generated/segmentation_sources.rs"]
mod segmentation_sources;
pub use segmentation_sources::{
    segment_source_by_id, segment_source_by_identity, SegmentSourceContract,
    DEFAULT_SEGMENT_SOURCE_ID, SEGMENTATION_CONTRACT_SHA256, SEGMENT_SOURCES,
    SEGMENT_SOURCE_HEIGHT, SEGMENT_SOURCE_SHA256, SEGMENT_SOURCE_URL, SEGMENT_SOURCE_WIDTH,
};

pub mod agent_ink;
pub mod anchor;
pub mod claims;
pub use claims::{claim, claim_verdict, revise_claim, withdraw_claim, Claim, ClaimDigest};
pub mod architecture;
pub mod camera;
pub mod context;
pub mod decision;
pub mod excalidraw;
pub mod labels;
pub mod panels;
mod routing;
pub mod validation;

#[cfg(test)]
mod state_machine_tests;

pub use agent_ink::{create_variable, Relation, SolveState, VarState, Variable, VAR_STATES};

/// Object kinds. A single flat object space keeps the CRDT simple; the `kind`
/// property is what the projection dispatches on.
pub const KIND_NODE: &str = "node";
pub const KIND_EDGE: &str = "edge";
pub const KIND_MARK: &str = "mark";
pub const KIND_SHAPE: &str = "shape";
pub const KIND_CONSTRAINT: &str = "constraint";
pub const KIND_VARIABLE: &str = "variable";
pub const KIND_EXPLANATION_FLOW: &str = "explanation-flow";
pub const KIND_CLAIM: &str = "claim";

/// Flat document schema shared by the native host and browser replica.
/// Version 3 adds persisted agent-ink variables and their projection fields.
/// Version 4 adds semantic image segments. Version 5 binds model-authored
/// semantics to an explicit multimodal-agent receipt. Version 6 adds
/// materialized segment parts and one human-authored animation scalar.
/// Version 7 adds a bounded root/part hierarchy and a normalized 2D pivot.
/// Version 8 adds a validated, target-specific 2D keyframe motion program.
/// Version 9 makes that hierarchy recursive within a small fixed depth.
/// Version 10 adds one durable, event-driven explanation flow over stable
/// Atlas targets. The flow is example-local during Phase 2.
pub const DOC_SCHEMA_VERSION: u32 = 11;
/// Deepest containment the node register will store.
pub const HIERARCHY_MAX_DEPTH: usize = 6;
/// Root plus three nested part tiers. This is enough for body -> foot -> claw
/// while keeping the mask workflow a small hierarchy rather than a scene-graph
/// editor.
pub const SEGMENT_MAX_DEPTH: usize = 3;

/// The bounded source catalog and real MobileSAM artifacts.
///
/// The source catalog is generated from the checked segmentation contract.
/// It remains a closed list of repository assets. A caller cannot turn the
/// segment path into an arbitrary URL fetch or silently select another model.
pub const SEGMENT_MODEL_ID: &str = "mobilesam-vit-t";
pub const SEGMENT_ENCODER_SHA256: &str =
    "d80de6055095e7ba5551e6c15aaa8981b05dca77d6ae77ee82e51e2746a87e3d";
pub const SEGMENT_DECODER_SHA256: &str =
    "a21b65b6e1b75e2c6265b36835747a0ab9169ec1ed725139a78ce90297f95126";
pub const SEGMENT_MASK_ENCODING: &str = "mask-alpha-rle-u32le-base64-v1";
/// Legacy shortcuts retained only so existing version 7 documents still render.
pub const SEGMENT_ANIMATIONS: &[&str] = &["none", "pulse", "rock", "tap-parts"];
pub const SEGMENT_MOTION_SCHEMA: &str = "atlas-segment-motion-v1";
pub const SEGMENT_CHILD_COMPOSITING: &[&str] = &["cutout", "overlay"];
pub const SEGMENT_SEMANTIC_RECEIPT_SCHEMA: &str = "same-page-atlas-multimodal-semantics-v1";
pub const EXPLANATION_FLOW_SCHEMA: &str = "atlas-explanation-flow-v1";
pub const EXPLANATION_ACTION_KINDS: &[&str] = &["point", "reveal", "replay-motion", "trace-path"];
pub const EXPLANATION_ADVANCE_MODES: &[&str] = &["continue", "choice", "agent", "terminal"];
pub const EXPLANATION_STATUSES: &[&str] = &["active", "paused", "completed", "stopped"];
const MAX_SEGMENT_MASK_SIDE: u32 = 512;
const MAX_SEGMENT_MASK_BYTES: usize = 1_000_000;
const MAX_EXPLANATION_BEATS: usize = 32;
const MAX_EXPLANATION_EVIDENCE: usize = 16;
const MAX_EXPLANATION_ACTIONS: usize = 16;
const MAX_EXPLANATION_TRANSITIONS: usize = 8;

pub const CONSTRAINT_OPS: &[&str] = &["group", "sequence", "attaches", "voids", "labels"];
pub const CONSTRAINT_AXES: &[&str] = &["x", "y"];
pub const CONSTRAINT_SALIENCES: &[&str] = &["fore", "back"];

/// What a shape *is*. Closed list, same reasoning as [`TONES`]: the browser
/// draws each form differently and [`Atlas::describe`] interprets each one
/// differently, so an unrecognised form would land as an invisible object
/// rather than a rejected write.
///
/// The vocabulary is deliberately Excalidraw's, minus the parts that only mean
/// something with a full editor behind them (image, frame, embeddable). `ink`
/// is its `freedraw`. Keeping the names and the geometry conventions aligned
/// is what makes [`excalidraw`] a field rename rather than a translation.
pub const FORMS: &[&str] = &[
    "ink", "rect", "ellipse", "diamond", "line", "arrow", "text", "frame",
];

/// How a stroke is dashed. Excalidraw's `strokeStyle`, the same three words.
pub const STROKE_STYLES: &[&str] = &["solid", "dashed", "dotted"];

/// Whether a box's corners (or a connector's joins) are rounded. Excalidraw
/// stores this as `roundness: null | { type }`; we keep the two words a person
/// would use.
pub const ROUNDNESS: &[&str] = &["sharp", "round"];

/// Stroke width, in world pixels. Excalidraw's UI offers 1, 2, and 4; the
/// range here is wider so an imported value survives, and clamped so a
/// stroke cannot be authored so thick it is a fill.
pub const STROKE_WIDTH_RANGE: (f64, f64) = (0.5, 12.0);
pub const DEFAULT_STROKE_WIDTH: f64 = 2.0;

/// Opacity as Excalidraw stores it: 0 (invisible) to 100 (opaque).
pub const OPACITY_RANGE: (f64, f64) = (0.0, 100.0);

/// Font size, in world pixels, for a `text` and for a caption. Excalidraw's UI
/// offers 16, 20, 28, and 36. Zero means "not set": the renderer uses its own
/// default and the export writes Excalidraw's, so a document authored before
/// this field existed keeps drawing exactly as it did.
pub const FONT_SIZE_RANGE: (f64, f64) = (6.0, 120.0);

/// Most groups one shape may belong to. Excalidraw nests groups; eight deep
/// is more than any hand-made drawing reaches.
pub const MAX_GROUPS: usize = 8;

/// What a [`Reading`] may say a shape does.
pub const RELATIONS: &[&str] = &[
    "encloses",
    "crosses-out",
    "points-at",
    "starts-at",
    "connects",
    "says",
    "free",
];

/// Stroke and fill palette. A closed list for the same reason tones are: the
/// renderer maps each name to real CSS, and a model-authored colour string
/// would be both a styling escape hatch and an injection surface.
pub const INKS: &[&str] = &["slate", "blue", "green", "amber", "red", "violet"];

/// Largest magnitude accepted for authored world coordinates.
pub const WORLD_LIMIT: f64 = 20_000.0;

/// What each ink name is, as real colour.
///
/// Here rather than in the stylesheet because two other things need the same
/// answer: `.excalidraw` export writes `strokeColor` as hex, and import has to
/// map an arbitrary hex back onto this closed list. A palette the renderer
/// knew privately would mean an exported drawing came out a different colour
/// from the one on screen — so the browser reads this table through
/// `ink_palette()` instead of keeping its own copy.
pub const INK_HEX: &[(&str, &str)] = &[
    ("slate", "#93a1bd"),
    ("blue", "#6ea8ff"),
    ("green", "#4ade80"),
    ("amber", "#fbbf24"),
    ("red", "#f87171"),
    ("violet", "#a78bfa"),
];

/// Hex for one ink name; `slate` for anything unrecognised.
pub fn ink_hex(name: &str) -> &'static str {
    INK_HEX
        .iter()
        .find(|(ink, _)| *ink == name)
        .map(|(_, hex)| *hex)
        .unwrap_or("#93a1bd")
}

/// The closest ink name to an arbitrary `#rrggbb`.
///
/// Import needs this and export does not: our palette is closed and
/// Excalidraw's is not, so a document from anywhere else arrives with colours
/// that have no name here. Snapping to the nearest one keeps the drawing
/// recognisable instead of flattening every stroke to the default.
pub fn nearest_ink(hex: &str) -> &'static str {
    let parse = |text: &str| -> Option<(f64, f64, f64)> {
        let text = text.trim().trim_start_matches('#');
        let full = match text.len() {
            3 => text.chars().flat_map(|c| [c, c]).collect::<String>(),
            6 | 8 => text[..6].to_string(),
            _ => return None,
        };
        let channel = |at: usize| u8::from_str_radix(&full[at..at + 2], 16).ok();
        Some((channel(0)? as f64, channel(2)? as f64, channel(4)? as f64))
    };
    let Some(want) = parse(hex) else {
        return "slate";
    };
    INK_HEX
        .iter()
        .filter_map(|(name, candidate)| {
            let (r, g, b) = parse(candidate)?;
            let distance = (r - want.0).powi(2) + (g - want.1).powi(2) + (b - want.2).powi(2);
            Some((*name, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(name, _)| name)
        .unwrap_or("slate")
}

/// How a line or arrow ends.
pub const HEADS: &[&str] = &["none", "triangle", "dot"];

/// Points per shape. A human scribbling for a few seconds produces a few
/// hundred; past this the stroke is simplified at the writer rather than
/// silently truncated.
pub const MAX_POINTS: usize = 400;

/// Territory kinds a node can claim. Deliberately a closed list: the browser
/// renders each one differently, and an open vocabulary would silently degrade
/// to "unstyled" instead of failing the write.
pub const TONES: &[&str] = &[
    "crate", "example", "doc", "spec", "concept", "question", "risk",
];

/// Where a claim stands between the two of us. `open` is the default: nothing
/// is "agreed" until somebody says so.
pub const STATUSES: &[&str] = &["open", "agreed", "disputed", "done"];

/// How the agent knows a claim. `verified` needs a source range, `inferred`
/// may carry one, `assumed` and `unknown` refuse one so the label cannot lie.
pub const BASES: &[&str] = &["verified", "inferred", "assumed", "unknown"];

/// What the human said about a claim. Only the human writes this.
pub const VERDICTS: &[&str] = &["open", "accepted", "rejected"];

/// A node that needs more claims than this is two nodes.
pub const MAX_CLAIMS_PER_NODE: usize = 24;

/// Characters in a full commit hash, the only form a claim may pin.
pub const REVISION_LENGTH: usize = 40;

// ── the style register ────────────────────────────────────────────────────
//
// Four closed vocabularies, each answering a different question about a card,
// and each deliberately separate from the others. Form carries meaning on this
// surface: weight says how much something matters, enclosure says what owns
// it, colour says what family it is in, and shape says what kind of thing it
// is. Collapsing any two of those into one field is how a picture stops being
// readable, because then the reader cannot tell an important thing from a
// different KIND of thing.
//
// What the meaning IS stays the agent's to choose and to say out loud. The
// model enforces the vocabulary, never the interpretation: nothing here
// decides that amber means risk.

/// How much a card is claiming to matter, as weight rather than as size.
///
/// Distinct from [`SIZES`] on purpose. "This is the important one" and "this
/// one needs to be big enough to hold a paragraph" are different claims, and a
/// single field would make a long note look like an argument.
pub const EMPHASES: &[&str] = &["normal", "strong", "muted"];

/// How large a card is drawn, as an importance class rather than a width.
///
/// The authored claim, not the realised geometry: `w` is what the layout
/// actually gave it. Keeping the claim means a relayout can honour the
/// intent instead of re-deriving it from pixels it is about to overwrite.
pub const SIZES: &[&str] = &["normal", "primary", "hero"];

/// What a card IS, as a form a reader can tell apart from across the room.
///
/// This is the string `atlas_diagram` used to take as a "compatibility hint"
/// and throw away. Every value here is now stored on the node and reaches the
/// renderer, which is the whole of what "made honest" means: a tool that
/// accepts a field and discards it teaches the agent that the field works.
pub const NODE_KINDS: &[&str] = &["rect", "ellipse", "diamond"];

/// Typed diagram vocabulary accepted by the Atlas tool contract.
///
/// The built kinds come first, followed by the reserved kinds. The order lets
/// projection accept only the built prefix while every caller still validates
/// against one closed vocabulary.
pub const DIAGRAM_KINDS: &[&str] = &["hierarchy", "state_machine", "data_flow", "user_flow"];

const K_STATE_INITIAL: &str = "state_initial";
const K_STATE_TERMINAL: &str = "state_terminal";
const K_TRANSITION_EVENT: &str = "event";
const K_TRANSITION_GUARD: &str = "guard";
const K_TRANSITION_ORDER: &str = "transition_order";

/// What every card is before anyone says otherwise. Named rather than spelled
/// out at each use, because the read-back's rule for whether a style is worth
/// printing is exactly "it is not this".
pub const DEFAULT_EMPHASIS: &str = "normal";
pub const DEFAULT_SIZE: &str = "normal";
pub const DEFAULT_KIND: &str = "rect";

/// Human annotation glyphs. `?` = I don't follow, `!` = I think this is wrong,
/// `*` = this matters to me.
pub const GLYPHS: &[&str] = &["?", "!", "*"];

pub const MAX_OBJECTS: usize = 300;
pub const MAX_LABEL: usize = 140;
pub const MAX_NOTE: usize = 4_000;
pub const MAX_PATH: usize = 240;
pub const MAX_TEXT: usize = 1_200;

const GRID_ORIGIN: (f64, f64) = (48.0, 64.0);
const GRID_STEP: (f64, f64) = (300.0, 168.0);
const GRID_COLUMNS: usize = 4;
const DEFAULT_WIDTH: f64 = 232.0;

/// The content inset used by the named frame a container renders as.
///
/// These are model geometry, not decoration: a parent reports this exact box
/// through `Node::bounds`, while the browser draws the same box around the
/// rendered children. A source-contract test in the host pins both values to
/// the renderer so the two languages cannot drift silently.
pub const CONTAINER_PAD: f64 = 22.0;
pub const CONTAINER_TITLE_HEIGHT: f64 = 26.0;

/// A rejected write. Every one of these is a contract violation the caller can
/// fix and retry, which is exactly what the runtime's `Effect::Reject` wants.
pub type AtlasError = String;

// ── projection ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    /// Repository-relative source; empty when the node is a pure concept.
    pub path: String,
    /// `"12-48"` or `"12"`. Empty when the whole file is meant.
    pub lines: String,
    pub note: String,
    pub tone: String,
    pub status: String,
    pub cemented: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    /// Rendered height in world units.
    /// Nobody authors this: a node is as tall as its own text makes it, and
    /// only the browser that laid it out knows by how much. The renderer
    /// writes what it measured back through [`measure_node`]; until it does,
    /// this is [`estimate_height`]'s guess. `measured` says which one you are
    /// looking at, because "I calculated your overlap from a guess" and "I
    /// measured it" are different claims and the agent should not confuse them.
    pub h: f64,
    pub measured: bool,
    pub created_by: String,
    /// Who wrote to this node LAST. Per-key LWW means this is a coarse signal,
    /// not an audit log — but it is the signal that makes "the human moved my
    /// node" legible to the agent on its next read.
    pub touched_by: String,
    /// The node that contains this one. Empty when it sits at the top level.
    ///
    /// First-class containment rather than a `group` constraint or an inferred
    /// spatial ENCLOSES, because those two answer different questions. A
    /// constraint is a claim *about* a set that the set does not carry, and a
    /// spatial reading is geometry the human can break by dragging one card
    /// two hundred pixels. "This subsystem owns these three parts" survives
    /// both, so it lives on the node.
    ///
    /// [`read`] guarantees this is a tree: a parent naming a node that is not
    /// on the atlas, and any cycle two replicas produce by reparenting
    /// concurrently, are healed to the top level at projection time. Every
    /// reader of this field can therefore walk upwards without a visit set and
    /// without the risk of looping forever.
    pub parent: String,
    /// What typed diagram this container represents. Empty for an ordinary
    /// node, otherwise `hierarchy` or `state_machine`.
    pub diagram_kind: String,
    /// Whether this state is the machine's entry point.
    pub state_initial: bool,
    /// Whether this state ends the machine.
    pub state_terminal: bool,
    /// Agent-ink variable whose numeric value selects this machine's current
    /// state by stable child order. Empty when the machine is not bound.
    pub state_variable: String,
    /// The bound variable value immediately before its latest direct write.
    /// This lets both replicas describe the transition that produced the
    /// current state without keeping a replica-local opinion.
    pub state_previous_value: Option<f64>,
    /// Category colour, one of [`INKS`], empty when the card belongs to no
    /// family. Stored as the authored NAME rather than as hex, so the
    /// read-back can hand the label back to whoever chose it. A model that
    /// wrote `#fbbf24` and read back `#fbbf24` learns nothing about what it
    /// said; one that reads back `amber` can see its own taxonomy.
    pub color: String,
    /// One of [`EMPHASES`].
    pub emphasis: String,
    /// One of [`SIZES`].
    pub size: String,
    /// One of [`NODE_KINDS`].
    pub kind: String,
}

impl Node {
    /// `(left, top, right, bottom)` in world units.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.x + self.w, self.y + self.h)
    }

    fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The style words this card is actually saying something with.
    ///
    /// A defaulted style is not a claim. Printing `emphasis=normal kind=rect`
    /// on every one of forty cards would bury the one card that IS emphasised
    /// under thirty-nine restatements of the default, which is the same
    /// failure the drawn register avoids by refusing to report coordinates.
    /// So each field appears only where it differs from what every card
    /// already is, and a card that chose nothing prints nothing.
    pub fn style_words(&self) -> Vec<String> {
        let mut said = Vec::new();
        if !self.diagram_kind.is_empty() {
            said.push(format!("diagram_kind={}", self.diagram_kind));
        }
        if self.kind != DEFAULT_KIND {
            said.push(format!("kind={}", self.kind));
        }
        if self.emphasis != DEFAULT_EMPHASIS {
            said.push(format!("emphasis={}", self.emphasis));
        }
        if self.size != DEFAULT_SIZE {
            said.push(format!("size={}", self.size));
        }
        if !self.color.is_empty() {
            said.push(format!("color={}", self.color));
        }
        said
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    pub label: String,
    /// Required event name for a state-machine transition. Empty for an
    /// ordinary link.
    pub event: String,
    /// Optional condition for a state-machine transition.
    pub guard: String,
    /// Persisted creation order for byte-stable state-machine read-back.
    pub transition_order: u64,
    pub created_by: String,
    pub touched_by: String,
}

impl Edge {
    fn is_transition(&self) -> bool {
        !self.event.is_empty()
    }
}

/// The JSON form accepted by the core for one state machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StateMachineSpec {
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub direction: String,
    pub nodes: Vec<StateMachineSpecNode>,
    pub edges: Vec<StateMachineSpecEdge>,
}

/// One state in [`StateMachineSpec`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StateMachineSpecNode {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shape: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub emphasis: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub size: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub initial: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lines: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// One transition in [`StateMachineSpec`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StateMachineSpecEdge {
    pub from: String,
    pub to: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guard: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone)]
struct StateMachineView {
    id: String,
    title: String,
    states: Vec<StateView>,
    transitions: Vec<TransitionView>,
    current_state: Option<String>,
}

#[derive(Debug, Clone)]
struct StateView {
    id: String,
    label: String,
    initial: bool,
    terminal: bool,
}

#[derive(Debug, Clone)]
struct TransitionView {
    id: String,
    from: String,
    from_label: String,
    to: String,
    to_label: String,
    to_machine: String,
    to_machine_label: String,
    event: String,
    guard: String,
    order: u64,
}

impl StateMachineSpec {
    /// Parse and validate one state-machine JSON document.
    pub fn parse(input: &str) -> Result<Self, AtlasError> {
        let spec: Self = serde_json::from_str(input)
            .map_err(|error| format!("invalid state machine JSON: {error}"))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Export a validated state-machine document without losing its order.
    pub fn export(&self) -> Result<String, AtlasError> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| format!("could not export state machine: {error}"))
    }

    /// Describe this JSON through the same grammar used by [`Atlas::describe`].
    pub fn describe(&self) -> Result<String, AtlasError> {
        self.validate()?;
        let view = self.view();
        let mut out = view.describe();
        let problems = view.problems();
        if !problems.is_empty() {
            out.push_str(&format!("\nPROBLEMS ({})\n", problems.len()));
            for problem in problems {
                out.push_str(&format!(
                    "- {} [{}]\n",
                    problem.detail,
                    problem.ids.join(", ")
                ));
            }
        }
        Ok(out)
    }

    fn validate(&self) -> Result<(), AtlasError> {
        if self.kind != "state_machine" {
            return Err(format!(
                "state machine JSON kind must be \"state_machine\", got {:?}",
                self.kind
            ));
        }
        require_exact_text(&self.title, MAX_LABEL, "state machine title")?;
        if self.title.is_empty() {
            return Err("a state machine needs a title".to_string());
        }

        let mut ids = std::collections::BTreeSet::new();
        for node in &self.nodes {
            require_exact_text(&node.id, MAX_LABEL, "state id")?;
            require_exact_text(&node.label, MAX_LABEL, "state label")?;
            if node.id.is_empty() || node.label.is_empty() {
                return Err("every state needs a non-empty id and label".to_string());
            }
            if !ids.insert(node.id.as_str()) {
                return Err(format!("duplicate state id {:?}", node.id));
            }
            if !node.shape.is_empty() {
                one_of(&node.shape, NODE_KINDS, "state shape")?;
            }
            if !node.color.is_empty() {
                one_of(&node.color, INKS, "state color")?;
            }
            if !node.emphasis.is_empty() {
                one_of(&node.emphasis, EMPHASES, "state emphasis")?;
            }
            if !node.size.is_empty() {
                one_of(&node.size, SIZES, "state size")?;
            }
            require_exact_text(&node.path, MAX_PATH, "state path")?;
            require_exact_text(&node.lines, MAX_LABEL, "state lines")?;
            require_exact_text(&node.note, MAX_NOTE, "state note")?;
        }

        for (index, edge) in self.edges.iter().enumerate() {
            require_exact_text(&edge.event, MAX_LABEL, "transition event")?;
            require_exact_text(&edge.guard, MAX_TEXT, "transition guard")?;
            require_exact_text(&edge.label, MAX_LABEL, "transition label")?;
            if edge.event.is_empty() {
                return Err(format!(
                    "diagram edge {} is inside the state machine {:?} and carries no `event`; a transition is named by the event that fires it",
                    index + 1,
                    self.title
                ));
            }
            if !edge.label.is_empty() {
                return Err(format!(
                    "diagram edge {} carries both `label` and `event`; a transition must use `event`",
                    index + 1
                ));
            }
            for endpoint in [&edge.from, &edge.to] {
                if !ids.contains(endpoint.as_str()) {
                    return Err(format!(
                        "diagram edge {} names unknown state {:?}",
                        index + 1,
                        endpoint
                    ));
                }
            }
        }
        Ok(())
    }

    fn view(&self) -> StateMachineView {
        let label_of = |id: &str| {
            self.nodes
                .iter()
                .find(|node| node.id == id)
                .map_or_else(|| format!("<missing {id}>"), |node| node.label.clone())
        };
        StateMachineView {
            id: "fixture".to_string(),
            title: self.title.clone(),
            states: self
                .nodes
                .iter()
                .map(|node| StateView {
                    id: node.id.clone(),
                    label: node.label.clone(),
                    initial: node.initial,
                    terminal: node.terminal,
                })
                .collect(),
            transitions: self
                .edges
                .iter()
                .enumerate()
                .map(|(index, edge)| TransitionView {
                    id: format!("edge-{}", index + 1),
                    from: edge.from.clone(),
                    from_label: label_of(&edge.from),
                    to: edge.to.clone(),
                    to_label: label_of(&edge.to),
                    to_machine: "fixture".to_string(),
                    to_machine_label: self.title.clone(),
                    event: edge.event.clone(),
                    guard: edge.guard.clone(),
                    order: index as u64 + 1,
                })
                .collect(),
            current_state: None,
        }
    }
}

impl StateMachineView {
    fn describe(&self) -> String {
        let cyclic = self.is_cyclic();
        let cycle_note = if cyclic { ", cyclic" } else { "" };
        let mut out = format!(
            "\nSTATE MACHINE {:?} ({} states, {} transitions{cycle_note})\n",
            self.title,
            self.states.len(),
            self.transitions.len()
        );
        if let Some(current_state) = &self.current_state {
            out.push_str(current_state);
            out.push('\n');
        }
        for state in &self.states {
            let mut roles = Vec::new();
            if state.initial {
                roles.push("INITIAL");
            }
            if state.terminal {
                roles.push("TERMINAL");
            }
            let suffix = if roles.is_empty() {
                String::new()
            } else {
                format!(" {}", roles.join(" "))
            };
            out.push_str(&format!("- state {:?}{suffix}\n", state.label));
        }
        let mut transitions = self.transitions.iter().collect::<Vec<_>>();
        transitions.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        for transition in transitions {
            out.push_str(&format!("- {}\n", transition.sentence()));
        }
        out
    }

    fn problems(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let initials = self
            .states
            .iter()
            .filter(|state| state.initial)
            .collect::<Vec<_>>();
        match initials.as_slice() {
            [] => problems.push(Problem {
                code: "missing_initial_state",
                ids: vec![self.id.clone()],
                detail: format!("state machine {:?}: no state is marked initial", self.title),
            }),
            [first, second, ..] => problems.push(Problem {
                code: "multiple_initial_states",
                ids: vec![self.id.clone(), first.id.clone(), second.id.clone()],
                detail: format!(
                    "state machine {:?}: {:?} and {:?} are both marked initial",
                    self.title, first.label, second.label
                ),
            }),
            [_] => {}
        }

        if let Some(initial) = initials.first() {
            let reachable = self.reachable_from(&initial.id);
            for state in self
                .states
                .iter()
                .filter(|state| !reachable.contains(&state.id))
            {
                problems.push(Problem { code: "unreachable_state",
                    ids: vec![self.id.clone(), state.id.clone()],
                    detail: format!(
                        "state machine {:?}: state {:?} is unreachable; no transition reaches it from {:?}",
                        self.title, state.label, initial.label
                    ),
                });
            }
        }

        for state in self.states.iter().filter(|state| state.terminal) {
            for transition in self
                .transitions
                .iter()
                .filter(|transition| transition.from == state.id)
            {
                problems.push(Problem { code: "terminal_transition",
                    ids: vec![self.id.clone(), state.id.clone(), transition.id.clone()],
                    detail: format!(
                        "state machine {:?}: terminal state {:?} has an outgoing transition on {:?}; a terminal state ends the machine",
                        self.title, state.label, transition.event
                    ),
                });
            }
        }

        let mut unguarded: std::collections::BTreeMap<(&str, &str), Vec<&TransitionView>> =
            std::collections::BTreeMap::new();
        for transition in self
            .transitions
            .iter()
            .filter(|transition| transition.guard.is_empty())
        {
            unguarded
                .entry((&transition.from, &transition.event))
                .or_default()
                .push(transition);
        }
        for ((from, event), transitions) in unguarded {
            if transitions.len() < 2 {
                continue;
            }
            let label = self
                .states
                .iter()
                .find(|state| state.id == from)
                .map_or(from, |state| state.label.as_str());
            let mut ids = vec![self.id.clone(), from.to_string()];
            ids.extend(transitions.iter().map(|transition| transition.id.clone()));
            problems.push(Problem { code: "ambiguous_transition",
                ids,
                detail: format!(
                    "state machine {:?}: state {:?} has two transitions on {:?} and neither carries a guard; which one fires is ambiguous",
                    self.title, label, event
                ),
            });
        }

        for transition in self
            .transitions
            .iter()
            .filter(|transition| transition.to_machine != self.id)
        {
            let destination = if transition.to_machine.is_empty() {
                "outside any state machine".to_string()
            } else {
                format!("into state machine {:?}", transition.to_machine_label)
            };
            problems.push(Problem {
                code: "cross_machine_transition",
                ids: vec![self.id.clone(), transition.id.clone()],
                detail: format!(
                    "state machine {:?}: transition {} crosses {destination}",
                    self.title,
                    transition.sentence()
                ),
            });
        }
        problems
    }

    fn reachable_from(&self, initial: &str) -> std::collections::BTreeSet<String> {
        let mut reachable = std::collections::BTreeSet::from([initial.to_string()]);
        let mut pending = std::collections::VecDeque::from([initial.to_string()]);
        while let Some(from) = pending.pop_front() {
            for transition in self
                .transitions
                .iter()
                .filter(|transition| transition.from == from && transition.to_machine == self.id)
            {
                if reachable.insert(transition.to.clone()) {
                    pending.push_back(transition.to.clone());
                }
            }
        }
        reachable
    }

    fn is_cyclic(&self) -> bool {
        self.states.iter().any(|origin| {
            let mut visited = std::collections::BTreeSet::new();
            let mut pending = self
                .transitions
                .iter()
                .filter(|transition| {
                    transition.from == origin.id && transition.to_machine == self.id
                })
                .map(|transition| transition.to.clone())
                .collect::<Vec<_>>();
            while let Some(at) = pending.pop() {
                if at == origin.id {
                    return true;
                }
                if !visited.insert(at.clone()) {
                    continue;
                }
                pending.extend(
                    self.transitions
                        .iter()
                        .filter(|transition| {
                            transition.from == at && transition.to_machine == self.id
                        })
                        .map(|transition| transition.to.clone()),
                );
            }
            false
        })
    }
}

impl TransitionView {
    fn sentence(&self) -> String {
        let guard = if self.guard.is_empty() {
            String::new()
        } else {
            format!(" when {}", self.guard)
        };
        format!(
            "from {:?} on {}{guard} -> {:?}",
            self.from_label, self.event, self.to_label
        )
    }
}

fn require_exact_text(value: &str, limit: usize, field: &str) -> Result<(), AtlasError> {
    let checked = text(value, limit, field)?;
    if checked != value {
        return Err(format!("{field} must not have surrounding whitespace"));
    }
    Ok(())
}

/// The agent-ink bridge from a numeric variable to a typed state machine.
pub mod living_ink {
    use ag_ui_canvas::scene::{Author, PropValue, Scene};
    use yrs::{Any, Map, Out, Transact};

    use super::{agent_ink, read, Atlas, AtlasError, Node, MAX_LABEL};

    pub(super) const K_STATE_VARIABLE: &str = "state_variable";
    pub(super) const K_STATE_PREVIOUS_VALUE: &str = "state_previous_value";

    /// Bind one numeric agent-ink variable to one state machine.
    ///
    /// State order is the core node register's stable order. If `state` is
    /// present, its exact visible name is resolved to that order and written
    /// into a pinned or scrubbing variable in the same CRDT transaction as
    /// the binding. The returned sentence is the sentence the state-machine
    /// read-back uses for the resulting current state.
    pub fn bind_state(
        scene: &Scene,
        machine: &str,
        variable: &str,
        state: Option<&str>,
        author: &Author,
    ) -> Result<String, AtlasError> {
        let atlas = read(scene)?;
        let machine = resolve_machine(&atlas, machine)?;
        let machine_id = machine.id.clone();
        let machine_label = machine.label.clone();
        let states = atlas.children_of(&machine_id);
        if states.is_empty() {
            return Err(format!(
                "state machine {machine_label:?} has no states to bind"
            ));
        }

        let variable_name = exact_text(variable, "variable name")?;
        let variable = atlas
            .variables
            .iter()
            .find(|candidate| candidate.name == variable_name)
            .ok_or_else(|| format!("no agent-ink variable named {variable_name:?}"))?;
        let previous_index = checked_index(variable.value, states.len(), &variable_name)?;

        let requested_index = state
            .map(|state| resolve_state(&states, state, &machine_label))
            .transpose()?;
        if requested_index.is_some()
            && !matches!(
                variable.state,
                agent_ink::VarState::Pinned | agent_ink::VarState::Scrubbing
            )
        {
            return Err(format!(
                "variable {variable_name:?} is {} and cannot be set by state name; use a pinned or scrubbing variable",
                variable.state.as_str()
            ));
        }

        let existing = string_prop(scene, &machine_id, K_STATE_VARIABLE)?;
        if existing
            .as_deref()
            .is_some_and(|bound| bound != variable_name)
        {
            return Err(format!(
                "state machine {machine_label:?} is already bound to variable {:?}; a second variable is refused",
                existing.unwrap_or_default()
            ));
        }
        if existing.as_deref() == Some(variable_name.as_str()) && requested_index.is_none() {
            return read_back(scene, &machine_id);
        }

        let doc = scene.doc();
        let objects = doc.get_or_insert_map("objects");
        let mut txn = doc
            .try_transact_mut()
            .map_err(|error| format!("atlas is busy: {error}"))?;
        let machine_map = match objects.get(&txn, machine_id.as_str()) {
            Some(Out::YMap(map)) => map,
            _ => {
                return Err(format!(
                    "state machine {machine_label:?} disappeared before its binding could be stored"
                ));
            }
        };
        let variable_map = if requested_index.is_some() {
            let mut found = None;
            for (_, value) in objects.iter(&txn) {
                let Out::YMap(map) = value else { continue };
                let kind = match map.get(&txn, "kind") {
                    Some(Out::Any(Any::String(value))) => value.to_string(),
                    _ => String::new(),
                };
                let name = match map.get(&txn, "name") {
                    Some(Out::Any(Any::String(value))) => value.to_string(),
                    _ => String::new(),
                };
                if kind == super::KIND_VARIABLE && name == variable_name {
                    found = Some(map);
                    break;
                }
            }
            Some(found.ok_or_else(|| {
                format!("variable {variable_name:?} disappeared before the state could be set")
            })?)
        } else {
            None
        };

        machine_map.insert(&mut txn, K_STATE_VARIABLE, Any::from(variable_name.clone()));
        machine_map.insert(
            &mut txn,
            K_STATE_PREVIOUS_VALUE,
            Any::from(previous_index as f64),
        );
        machine_map.insert(
            &mut txn,
            "touched_by",
            Any::from(author.as_str().to_string()),
        );
        if let (Some(index), Some(map)) = (requested_index, variable_map) {
            map.insert(&mut txn, "value", Any::from(index as f64));
            map.insert(
                &mut txn,
                "touched_by",
                Any::from(author.as_str().to_string()),
            );
        }
        drop(txn);

        read_back(scene, &machine_id)
    }

    /// Finish the core variable-set path after its ordinary ownership checks.
    /// Every bound machine is validated first. The variable and each
    /// machine's previous value then land in one CRDT transaction, so neither
    /// replica can observe a current state without its transition origin.
    pub(crate) fn write_variable_value(
        scene: &Scene,
        atlas: &Atlas,
        name: &str,
        previous: f64,
        value: f64,
        author: &Author,
    ) -> Result<(), AtlasError> {
        let machines = atlas
            .nodes
            .iter()
            .filter(|node| node.diagram_kind == "state_machine" && node.state_variable == name)
            .collect::<Vec<_>>();
        for machine in &machines {
            checked_index(value, atlas.children_of(&machine.id).len(), name)?;
        }

        let doc = scene.doc();
        let objects = doc.get_or_insert_map("objects");
        let mut txn = doc
            .try_transact_mut()
            .map_err(|error| format!("atlas is busy: {error}"))?;
        let variable_map = objects
            .iter(&txn)
            .find_map(|(_, value)| {
                let Out::YMap(map) = value else { return None };
                let kind = match map.get(&txn, "kind") {
                    Some(Out::Any(Any::String(value))) => value.to_string(),
                    _ => String::new(),
                };
                let stored_name = match map.get(&txn, "name") {
                    Some(Out::Any(Any::String(value))) => value.to_string(),
                    _ => String::new(),
                };
                (kind == super::KIND_VARIABLE && stored_name == name).then_some(map)
            })
            .ok_or_else(|| format!("variable {name:?} has no persisted object"))?;
        let machine_maps = machines
            .iter()
            .map(|machine| match objects.get(&txn, machine.id.as_str()) {
                Some(Out::YMap(map)) => Ok(map),
                _ => Err(format!(
                    "state machine {:?} disappeared before the variable could be set",
                    machine.label
                )),
            })
            .collect::<Result<Vec<_>, AtlasError>>()?;

        for machine_map in machine_maps {
            machine_map.insert(&mut txn, K_STATE_PREVIOUS_VALUE, Any::from(previous));
            machine_map.insert(
                &mut txn,
                "touched_by",
                Any::from(author.as_str().to_string()),
            );
        }
        variable_map.insert(&mut txn, "value", Any::from(value));
        variable_map.insert(
            &mut txn,
            "touched_by",
            Any::from(author.as_str().to_string()),
        );
        Ok(())
    }

    /// Read the current state of one bound machine in domain terms.
    pub fn read_back(scene: &Scene, machine: &str) -> Result<String, AtlasError> {
        let atlas = read(scene)?;
        let machine = resolve_machine(&atlas, machine)?;
        current_state_sentence(&atlas, machine).ok_or_else(|| {
            format!(
                "state machine {:?} has no agent-ink state binding",
                machine.label
            )
        })
    }

    /// The canonical current-state line projected into the shared machine
    /// description. `None` means this machine has no binding, while every
    /// bound machine returns exactly one line, including unresolved bindings.
    pub(super) fn current_state_sentence(atlas: &Atlas, machine: &Node) -> Option<String> {
        let variable_name = machine.state_variable.as_str();
        if variable_name.is_empty() {
            return None;
        }
        let Some(variable) = atlas
            .variables
            .iter()
            .find(|candidate| candidate.name == variable_name)
        else {
            return Some(format!(
                "CURRENT STATE unresolved: bound variable {variable_name:?} is missing"
            ));
        };
        let states = atlas.children_of(&machine.id);
        let Ok(current_index) = checked_index(variable.value, states.len(), variable_name) else {
            return Some(format!(
                "CURRENT STATE unresolved: bound variable {variable_name:?} reads {} and the machine has {} states",
                variable.value,
                states.len()
            ));
        };
        let current = states[current_index];

        let previous_step = previous_step_value(atlas, variable_name).ok().flatten();
        let previous_value = previous_step.or(machine.state_previous_value);
        let Some(previous_index) =
            previous_value.and_then(|value| checked_index(value, states.len(), variable_name).ok())
        else {
            return Some(format!("CURRENT STATE {:?}", current.label));
        };
        let previous = states[previous_index];
        let transitions = atlas
            .edges
            .iter()
            .filter(|edge| {
                !edge.event.is_empty() && edge.from == previous.id && edge.to == current.id
            })
            .collect::<Vec<_>>();

        if previous.id == current.id && previous_step.is_none() && transitions.is_empty() {
            return Some(format!("CURRENT STATE {:?}", current.label));
        }
        let route = match transitions.as_slice() {
            [transition] => format!(", via {}", transition.event),
            [] => String::new(),
            many => {
                let event = &many[0].event;
                if many.iter().all(|transition| transition.event == *event) {
                    format!(", {} transitions on {event} connect them", many.len())
                } else {
                    format!(", {} transitions connect them", many.len())
                }
            }
        };
        Some(format!(
            "CURRENT STATE {:?} (was {:?}{route})",
            current.label, previous.label
        ))
    }

    fn resolve_machine<'a>(atlas: &'a Atlas, value: &str) -> Result<&'a Node, AtlasError> {
        let value = exact_text(value, "state machine")?;
        if let Some(node) = atlas.node(&value) {
            return if node.diagram_kind == "state_machine" {
                Ok(node)
            } else {
                Err(format!("node {value:?} is not a state_machine container"))
            };
        }
        let matches = atlas
            .nodes
            .iter()
            .filter(|node| node.diagram_kind == "state_machine" && node.label == value)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [machine] => Ok(*machine),
            [] => Err(format!("no state machine named {value:?}")),
            _ => Err(format!(
                "state machine name {value:?} is ambiguous; it matches {} containers",
                matches.len()
            )),
        }
    }

    fn resolve_state(states: &[&Node], value: &str, machine: &str) -> Result<usize, AtlasError> {
        let value = exact_text(value, "state name")?;
        let matches = states
            .iter()
            .enumerate()
            .filter(|(_, state)| state.label == value)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => Ok(*index),
            [] => Err(format!(
                "state {value:?} is not in machine {machine:?}; its states are {}",
                state_names(states)
            )),
            _ => Err(format!(
                "state name {value:?} is ambiguous in machine {machine:?}; its states are {}",
                state_names(states)
            )),
        }
    }

    fn state_names(states: &[&Node]) -> String {
        states
            .iter()
            .map(|state| format!("{:?}", state.label))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn checked_index(value: f64, count: usize, variable: &str) -> Result<usize, AtlasError> {
        if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= count as f64 {
            return Err(format!(
                "bound variable {variable:?} must read a non-negative integer from 0 to {}; got {value}",
                count.saturating_sub(1)
            ));
        }
        Ok(value as usize)
    }

    fn previous_step_value(atlas: &Atlas, variable: &str) -> Result<Option<f64>, AtlasError> {
        let Some(index_variable) = agent_ink::step_index_variable(atlas)? else {
            return Ok(None);
        };
        if !index_variable.value.is_finite()
            || index_variable.value <= 0.0
            || index_variable.value.fract() != 0.0
            || index_variable.value > f64::from(u32::MAX)
        {
            return Ok(None);
        }
        let prior_index = index_variable.value as u32 - 1;
        Ok(atlas
            .agent_ink_steps
            .iter()
            .find(|step| step.index == prior_index)
            .and_then(|step| step.values.iter().find(|value| value.name == variable))
            .map(|value| value.value))
    }

    fn exact_text(value: &str, field: &str) -> Result<String, AtlasError> {
        let checked = super::text(value, MAX_LABEL, field)?;
        if checked.is_empty() {
            return Err(format!("{field} cannot be empty"));
        }
        if checked != value {
            return Err(format!("{field} must not have surrounding whitespace"));
        }
        Ok(checked)
    }

    fn string_prop(scene: &Scene, id: &str, key: &str) -> Result<Option<String>, AtlasError> {
        match scene
            .get_prop(&ag_ui_canvas::ids::ObjectId::from(id.to_string()), key)
            .map_err(|error| format!("could not read {key} from {id}: {error}"))?
        {
            Some(PropValue::Str(value)) if !value.is_empty() => Ok(Some(value)),
            Some(PropValue::Str(_)) | None => Ok(None),
            Some(_) => Err(format!("stored {key} on {id:?} is not text")),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{
            agent_ink, place_node, set_node_diagram_kind, set_state_roles, transition, NodePatch,
        };

        fn machine(scene: &mut Scene) -> (String, String, String) {
            let container = place_node(
                scene,
                &NodePatch {
                    label: Some("request".to_string()),
                    x: Some(40.0),
                    y: Some(40.0),
                    ..Default::default()
                },
                &Author::Agent,
            )
            .expect("place machine");
            let idle = place_node(
                scene,
                &NodePatch {
                    label: Some("Idle".to_string()),
                    parent: Some(container.clone()),
                    x: Some(100.0),
                    y: Some(120.0),
                    ..Default::default()
                },
                &Author::Agent,
            )
            .expect("place Idle");
            let pending = place_node(
                scene,
                &NodePatch {
                    label: Some("Pending".to_string()),
                    parent: Some(container.clone()),
                    x: Some(500.0),
                    y: Some(120.0),
                    ..Default::default()
                },
                &Author::Agent,
            )
            .expect("place Pending");
            set_node_diagram_kind(scene, &container, "state_machine", &Author::Agent)
                .expect("mark state machine");
            set_state_roles(scene, &idle, Some(true), Some(false), &Author::Agent)
                .expect("mark Idle initial");
            set_state_roles(scene, &pending, Some(false), Some(false), &Author::Agent)
                .expect("mark Pending");
            transition(scene, &idle, &pending, "Submit", None, &Author::Agent)
                .expect("create Submit transition");
            (container, idle, pending)
        }

        fn four_state_machine(scene: &mut Scene) -> String {
            let (machine, _, _) = machine(scene);
            for (label, x, terminal) in [("Complete", 900.0, true), ("Failed", 1300.0, true)] {
                let state = place_node(
                    scene,
                    &NodePatch {
                        label: Some(label.to_string()),
                        parent: Some(machine.clone()),
                        x: Some(x),
                        y: Some(120.0),
                        ..Default::default()
                    },
                    &Author::Agent,
                )
                .expect("place state");
                set_state_roles(scene, &state, Some(false), Some(terminal), &Author::Agent)
                    .expect("mark state");
            }
            machine
        }

        fn force_variable_value(scene: &Scene, name: &str, value: f64) {
            let doc = scene.doc();
            let objects = doc.get_or_insert_map("objects");
            let mut txn = doc.try_transact_mut().expect("open corruption transaction");
            let variable = objects
                .iter(&txn)
                .find_map(|(_, candidate)| {
                    let Out::YMap(map) = candidate else {
                        return None;
                    };
                    let stored_name = match map.get(&txn, "name") {
                        Some(Out::Any(Any::String(value))) => value.to_string(),
                        _ => String::new(),
                    };
                    (stored_name == name).then_some(map)
                })
                .expect("find persisted variable");
            variable.insert(&mut txn, "value", Any::from(value));
        }

        #[test]
        fn binding_a_variable_to_a_machine_succeeds() {
            let mut scene = Scene::new();
            let (machine, _, _) = machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "scrubbing",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");

            let sentence =
                bind_state(&scene, "request", "cursor", None, &Author::Agent).expect("bind cursor");
            assert_eq!(sentence, "CURRENT STATE \"Idle\"");
            assert_eq!(
                string_prop(&scene, &machine, K_STATE_VARIABLE).expect("read binding"),
                Some("cursor".to_string())
            );

            let encoded = scene.encode_full().expect("encode binding");
            let reloaded = Scene::from_state(&encoded).expect("reload binding");
            assert_eq!(
                read_back(&reloaded, &machine).expect("read reloaded binding"),
                "CURRENT STATE \"Idle\""
            );
        }

        #[test]
        fn stepping_it_changes_the_current_state_and_the_read_back_sentence() {
            let mut scene = Scene::new();
            let (machine, _, _) = machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");
            bind_state(&scene, &machine, "cursor", None, &Author::Agent).expect("bind cursor");

            agent_ink::set_variable_value(&scene, "cursor", 1.0, &Author::Agent)
                .expect("step cursor");
            assert_eq!(
                read_back(&scene, &machine).expect("read Pending"),
                "CURRENT STATE \"Pending\" (was \"Idle\", via Submit)"
            );
        }

        #[test]
        fn an_unknown_state_is_refused() {
            let mut scene = Scene::new();
            let (machine, _, _) = machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");

            let error = bind_state(&scene, &machine, "cursor", Some("Missing"), &Author::Agent)
                .expect_err("unknown state must be refused");
            assert!(error.contains("Missing"), "{error}");
            assert!(error.contains("\"Idle\", \"Pending\""), "{error}");
            assert_eq!(
                string_prop(&scene, &machine, K_STATE_VARIABLE).expect("read binding"),
                None,
                "a refused state must not store a binding"
            );
        }

        #[test]
        fn a_step_in_learning_mode_re_derives_the_current_state_from_the_index() {
            let mut scene = Scene::new();
            let (machine, _, _) = machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "step",
                0.0,
                "scrubbing",
                None,
                None,
                &Author::Agent,
            )
            .expect("create step index");
            agent_ink::create_variable(
                &mut scene,
                "machine state",
                0.0,
                "free",
                None,
                None,
                &Author::Agent,
            )
            .expect("create machine state");
            agent_ink::create_relation(
                &mut scene,
                "state follows step",
                "linear",
                &["machine state", "step"],
                Some(1.0),
                Some(0.0),
                None,
                &Author::Agent,
            )
            .expect("relate state to step");
            bind_state(&scene, &machine, "machine state", None, &Author::Agent)
                .expect("bind derived state");

            agent_ink::solve(&mut scene, &Author::Agent).expect("solve step zero");
            agent_ink::record_current_step(&mut scene, "step", &Author::Agent)
                .expect("record step zero");
            agent_ink::advance_step(&mut scene, "step", &Author::Agent)
                .expect("advance learning step");

            let atlas = read(&scene).expect("read advanced state");
            assert_eq!(
                atlas
                    .variables
                    .iter()
                    .find(|variable| variable.name == "machine state")
                    .expect("machine state")
                    .value,
                1.0
            );
            assert_eq!(
                read_back(&scene, &machine).expect("read advanced binding"),
                "CURRENT STATE \"Pending\" (was \"Idle\", via Submit)"
            );
        }

        #[test]
        fn full_atlas_read_after_bind_and_step_contains_current_state_with_was_and_via() {
            let mut scene = Scene::new();
            let (machine, _, _) = machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");
            bind_state(&scene, &machine, "cursor", None, &Author::Agent).expect("bind cursor");
            agent_ink::set_variable_value(&scene, "cursor", 1.0, &Author::Agent)
                .expect("step cursor");

            let atlas_read = read(&scene).expect("read atlas").describe();
            let expected = "CURRENT STATE \"Pending\" (was \"Idle\", via Submit)";
            assert!(atlas_read.contains(expected), "{atlas_read}");
            assert_eq!(
                atlas_read.matches("CURRENT STATE").count(),
                1,
                "{atlas_read}"
            );
        }

        #[test]
        fn out_of_range_set_is_refused_and_read_back_stays_on_last_valid_state() {
            let mut scene = Scene::new();
            let machine = four_state_machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");
            bind_state(&scene, &machine, "cursor", None, &Author::Agent).expect("bind cursor");

            let error = agent_ink::set_variable_value(&scene, "cursor", 9.0, &Author::Agent)
                .expect_err("out-of-range state must be refused");
            assert_eq!(
                error,
                "bound variable \"cursor\" must read a non-negative integer from 0 to 3; got 9"
            );
            let atlas = read(&scene).expect("read after refusal");
            assert_eq!(
                atlas
                    .variables
                    .iter()
                    .find(|variable| variable.name == "cursor")
                    .expect("cursor")
                    .value,
                0.0
            );
            assert!(
                atlas.describe().contains("CURRENT STATE \"Idle\""),
                "{}",
                atlas.describe()
            );
        }

        #[test]
        fn non_integer_set_is_refused() {
            let mut scene = Scene::new();
            let machine = four_state_machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");
            bind_state(&scene, &machine, "cursor", None, &Author::Agent).expect("bind cursor");

            let error = agent_ink::set_variable_value(&scene, "cursor", 1.5, &Author::Agent)
                .expect_err("fractional state must be refused");
            assert_eq!(
                error,
                "bound variable \"cursor\" must read a non-negative integer from 0 to 3; got 1.5"
            );
        }

        #[test]
        fn invalid_persisted_values_have_the_exact_unresolved_current_state_sentence() {
            let mut scene = Scene::new();
            let machine = four_state_machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");
            bind_state(&scene, &machine, "cursor", None, &Author::Agent).expect("bind cursor");

            for (value, shown) in [(9.0, "9"), (1.5, "1.5")] {
                force_variable_value(&scene, "cursor", value);
                let atlas_read = read(&scene).expect("read invalid binding").describe();
                let expected = format!(
                    "CURRENT STATE unresolved: bound variable \"cursor\" reads {shown} and the machine has 4 states"
                );
                assert!(atlas_read.contains(&expected), "{atlas_read}");
                assert_eq!(
                    atlas_read.matches("CURRENT STATE").count(),
                    1,
                    "{atlas_read}"
                );
            }
        }

        #[test]
        fn a_state_change_appears_in_changed_since_your_last_read_as_a_relation_change() {
            let mut scene = Scene::new();
            let (machine, _, _) = machine(&mut scene);
            agent_ink::create_variable(
                &mut scene,
                "cursor",
                0.0,
                "pinned",
                None,
                None,
                &Author::Agent,
            )
            .expect("create cursor");
            bind_state(&scene, &machine, "cursor", None, &Author::Agent).expect("bind cursor");
            let before = read(&scene).expect("read before step").digest();

            agent_ink::set_variable_value(&scene, "cursor", 1.0, &Author::Human)
                .expect("step cursor");
            let after = read(&scene).expect("read after step").digest();
            let delta = crate::describe_changes_for(&before, &after, "agent");
            assert!(delta.contains("CHANGED SINCE YOUR LAST READ"), "{delta}");
            assert!(
                delta.contains(&format!(
                    "- [{machine}] state machine \"request\" current-state relation changed to CURRENT STATE \"Pending\" (was \"Idle\", via Submit); it was CURRENT STATE \"Idle\""
                )),
                "{delta}"
            );
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Mark {
    pub id: String,
    pub target: String,
    pub glyph: String,
    pub text: String,
    /// The agent's reply. Empty until answered.
    pub answer: String,
    /// Immutable contexts observed when the human asked. Empty for older marks.
    pub context_ids: Vec<String>,
    pub created_by: String,
}

/// A declared structural relation between existing atlas objects.
///
/// Constraints deliberately carry no geometry or numeric solver state. Their
/// validity is a structural graph check over these string-valued properties.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Deserialize)]
pub struct Constraint {
    pub id: String,
    pub op: String,
    pub members: Vec<String>,
    pub axis: Option<String>,
    pub text: Option<String>,
    pub salience: String,
    /// `sat` unless one of the declared members no longer exists.
    pub status: String,
    pub created_by: String,
}

impl Constraint {
    fn statement(&self) -> String {
        match self.op.as_str() {
            "sequence" => format!(
                "{} sequence[{}]: {}",
                self.id,
                self.axis.as_deref().unwrap_or("x"),
                self.members.join(" -> ")
            ),
            "group" => format!("{} group: {}", self.id, self.members.join(", ")),
            "attaches" => format!("{} attaches: {}", self.id, self.members.join(" <-> ")),
            "voids" => format!("{} voids: {}", self.id, self.members.join(", ")),
            "labels" => format!(
                "{} labels: {} {:?}",
                self.id,
                self.members.join(", "),
                self.text.as_deref().unwrap_or("")
            ),
            _ => format!("{} {}: {}", self.id, self.op, self.members.join(", ")),
        }
    }

    fn description(&self) -> String {
        format!("{} ({})", self.statement(), self.status.to_uppercase())
    }
}

/// Exact document-level result for the union of same-axis sequence relations.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Deserialize)]
pub struct ConstraintState {
    pub status: String,
    pub core: Vec<String>,
}

impl Default for ConstraintState {
    fn default() -> Self {
        Self {
            status: "sat".to_string(),
            core: Vec::new(),
        }
    }
}

/// Something drawn rather than stated: a pen stroke, a box, an arrow.
///
/// The whole reason this kind exists is that some things a human means are
/// faster to draw than to type — "these three belong together" is one gesture
/// and a paragraph. The cost is that a drawing is opaque: 200 coordinate pairs
/// say nothing to a reader who is not looking at the screen. [`readings`] is
/// what pays that cost back, turning each shape into the claim it is making
/// about the nodes underneath it.
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
    /// Browser-derived appearance attached to a host-minted semantic object.
    /// Empty on every ordinary drawn shape. The model never supplies `id`,
    /// `created_by`, `touched_by`, or the world transform.
    pub segment_status: String,
    pub segment_tags: Vec<String>,
    pub segment_ocr: Vec<String>,
    pub segment_semantics_source: String,
    pub segment_semantics_receipt: JsonValue,
    pub segment_source_url: String,
    pub segment_source_sha256: String,
    pub segment_source_width: u32,
    pub segment_source_height: u32,
    pub segment_prompt_box: Vec<f64>,
    pub segment_occlusion_box: Vec<f64>,
    pub segment_generation: u32,
    pub segment_model_id: String,
    pub segment_encoder_sha256: String,
    pub segment_decoder_sha256: String,
    pub segment_predicted_iou: f64,
    pub segment_mask_encoding: String,
    pub segment_mask_width: u32,
    pub segment_mask_height: u32,
    pub segment_mask_runs: String,
    pub segment_mask_source_box: Vec<f64>,
    pub segment_wing_prompts: JsonValue,
    pub segment_parts: JsonValue,
    /// Stable id of the segment this mask is directly attached to. Empty for
    /// a root. Hierarchy depth is bounded by [`SEGMENT_MAX_DEPTH`].
    pub segment_parent_id: String,
    pub segment_flap: f64,
    pub segment_animation: String,
    /// Validated root-owned or self-owned 2D keyframe motion. `null` means no
    /// custom motion. Legacy preset names remain separate for version 7 state.
    pub segment_motion: JsonValue,
    /// Normalized transform origin inside this segment's own mask box.
    pub segment_pivot_x: f64,
    pub segment_pivot_y: f64,
    pub segment_receipt: JsonValue,
}

/// How much of a card falls inside a shape's region.
///
/// The distinction exists because the honest answer is sometimes neither. A
/// loop drawn three-quarters of the way around a card is a real gesture with
/// an unclear meaning, and resolving it silently — in either direction — is
/// the read-back inventing an intent the human did not express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Outside,
    Partial,
    Inside,
}

/// One browser pane projected into the atlas's world coordinate system.
///
/// This is an observation, not document state. The browser sends it to the
/// host's session-only attention slot; it is never written into the CRDT.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl ViewportRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Result<Self, AtlasError> {
        if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
            return Err("viewport values must be finite numbers".to_string());
        }
        if w <= 0.0 || h <= 0.0 {
            return Err("viewport width and height must be greater than zero".to_string());
        }
        Ok(Self { x, y, w, h })
    }

    /// `(left, top, right, bottom)` in world units.
    pub fn bounds(self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.x + self.w, self.y + self.h)
    }
}

/// Nodes related to the visible pane, in stable atlas order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewportReading {
    pub inside: Vec<String>,
    pub partial: Vec<String>,
}

/// One authored observation the learner should be able to verify on the page.
///
/// Evidence is separate from the spoken or written cue so an explanation
/// cannot satisfy its contract with fluent prose over an unrelated picture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationEvidence {
    pub target_id: String,
    pub detail: String,
}

/// One visual operation bound to a stable Atlas target.
///
/// `cue_span` is a character range in the beat cue. The event-clock renderer
/// executes actions when the beat begins. A later browser speech clock may use
/// this range to align the same action with spoken progress without changing
/// the flow state or the visual vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationVisualAction {
    pub kind: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cue_span: Option<[u32; 2]>,
}

/// One named way out of a beat.
///
/// Phrases and target ids are resolver hints, not a second hidden answer
/// checker. The agent combines the learner's utterance with live selection,
/// chooses one transition, and the shared core validates the exact id and
/// optimistic revision before advancing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationTransition {
    pub id: String,
    pub label: String,
    /// Render this transition as a learner-facing control whose click is the
    /// answer. Agent-resolved grading branches omit it and continue to use a
    /// free response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default)]
    pub target_ids: Vec<String>,
    #[serde(default)]
    pub phrases: Vec<String>,
}

/// How the human or agent may advance one beat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationAdvance {
    pub mode: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub transitions: Vec<ExplanationTransition>,
}

/// One finite explanatory unit: why it exists, what is said, what must be
/// observable, what the page does, and which transitions are valid next.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationBeat {
    pub id: String,
    pub title: String,
    pub intent: String,
    pub cue: String,
    #[serde(default)]
    pub evidence: Vec<ExplanationEvidence>,
    #[serde(default)]
    pub actions: Vec<ExplanationVisualAction>,
    pub advance: ExplanationAdvance,
}

/// An authored, topic-independent explanation graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationFlowDefinition {
    pub schema: String,
    pub id: String,
    pub title: String,
    pub goal: String,
    /// Semantic subjects this exploration is about, not architectural parents.
    #[serde(default)]
    pub about: Vec<String>,
    /// Explicit visual objects owned by this exploration, separate from the blueprint.
    #[serde(default)]
    pub scene_ids: Vec<String>,
    pub start: String,
    pub beats: Vec<ExplanationBeat>,
}

/// The independently mutable cursor over an explanation definition.
///
/// It is stored as one JSON scalar in the CRDT. Concurrent peers therefore
/// resolve to one coherent state rather than a current beat from one writer
/// and a transition record from another.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplanationFlowState {
    pub current_beat: String,
    pub status: String,
    pub revision: u32,
    /// Revision at which the current beat was entered.
    ///
    /// Pause, resume, and stop change the cursor revision but preserve this
    /// value, so browser visual actions do not replay for a control event.
    /// Advance and restart set it to the new revision.
    pub entered_revision: u32,
    #[serde(default)]
    pub last_transition: String,
    #[serde(default)]
    pub last_response: String,
    #[serde(default)]
    pub last_selected_target_ids: Vec<String>,
    pub touched_by: String,
}

/// The projected shared explanation object.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExplanationFlow {
    pub object_id: String,
    pub definition: ExplanationFlowDefinition,
    pub state: ExplanationFlowState,
    pub created_by: String,
}

/// Fraction of a card that has to be inside a region before it is grouped.
const ENCLOSE_FRACTION: f64 = 0.75;
/// Below this it is a near miss, not an ambiguity.
const GRAZE_FRACTION: f64 = 0.15;
/// Samples per axis when measuring how much of a card a region covers.
const COVERAGE_GRID: usize = 4;

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
    pub fn style_note(&self, atlas: &Atlas) -> String {
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

    fn is_box(&self) -> bool {
        matches!(self.form.as_str(), "rect" | "ellipse" | "diamond" | "frame")
    }

    /// Whether this form is authored with `w`/`h` rather than points.
    pub fn is_sized(&self) -> bool {
        self.is_box() || self.form == "text" || self.form == "segment"
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

    /// Forms that paint on top of the cards rather than behind them. A box
    /// behind a card is the grouping look working as intended; a stroke lying
    /// across one is covering up what it is drawn on.
    fn is_overlay(&self) -> bool {
        !self.is_box() && self.form != "segment"
    }

    /// Whether this shape bounds a region — something can be *inside* it.
    ///
    /// A box form always does. An ink stroke does only if the human brought it
    /// back around near where they started; an open squiggle encircles nothing,
    /// and reporting that it did would be the read-back inventing intent.
    fn is_closed(&self) -> bool {
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

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Atlas {
    pub proposals: Vec<architecture::Proposal>,
    #[serde(default)]
    pub context: context::ContextState,
    pub decision_drafts: Vec<decision::Draft>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub marks: Vec<Mark>,
    /// What the agent says it understands about a node, each with a basis
    /// and the human's verdict. See `claims`.
    #[serde(default)]
    pub claims: Vec<Claim>,
    pub shapes: Vec<Shape>,
    pub constraints: Vec<Constraint>,
    /// Compatibility projection for older clients. New readers use explanations.
    pub explanation: Option<ExplanationFlow>,
    #[serde(default)]
    pub explanations: Vec<ExplanationFlow>,
    pub constraint_state: ConstraintState,
    pub variables: Vec<Variable>,
    /// B2 populates numeric relations through the agent-ink projection hook.
    pub relations: Vec<Relation>,
    /// B2 replaces this placeholder with the projected numeric solve result.
    pub solve_state: SolveState,
    /// M0 adds the document-level LEARNING/ALIGNMENT mode. `None` means no
    /// mode object has ever been projected, which callers treat exactly as
    /// `Some(AgentInkMode::Learning)` — a document that never set a mode
    /// keeps behaving the way agent-ink always has.
    pub agent_ink_mode: Option<agent_ink::AgentInkMode>,
    /// Present once [`agent_ink::promote_to_alignment`] has run: a document
    /// whose mode did not start as whatever `agent_ink_mode` currently
    /// reports carries a record of what it was promoted from and by whom.
    /// `None` for a document that has always been in its current mode.
    pub agent_ink_promotion: Option<agent_ink::PromotionRecord>,
    /// The highest-indexed stored step whose solve converged, if any. `None`
    /// for a document that has never recorded a converged step. `describe`
    /// reads this to tell a value that matches what was actually agreed
    /// from one that is merely current — see
    /// `agent_ink::finalize_committed_step`'s doc comment for why this is
    /// computed here rather than through the ordinary object-projection
    /// dispatch.
    pub agent_ink_committed: Option<agent_ink::StoredStep>,
    /// Every stored agent-ink step, collected in the same projection pass
    /// as everything else above. `agent_ink::finalize_committed_step` is
    /// the only reader; kept out of the JSON snapshot (`#[serde(skip)]`)
    /// because nothing else needs a duplicate of what `agent_ink::stored_step`
    /// already answers, index by index, straight from the scene.
    #[serde(skip)]
    pub agent_ink_steps: Vec<agent_ink::StoredStep>,
}

impl Atlas {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn shape(&self, id: &str) -> Option<&Shape> {
        self.shapes.iter().find(|shape| shape.id == id)
    }

    pub fn claim(&self, id: &str) -> Option<&Claim> {
        self.claims.iter().find(|claim| claim.id == id)
    }

    /// Every claim about one node, withdrawn ones included, in id order.
    pub fn claims_of(&self, node_id: &str) -> Vec<&Claim> {
        self.claims
            .iter()
            .filter(|claim| claim.about == node_id)
            .collect()
    }

    /// At least one live claim the human has not accepted.
    pub fn under_challenge(&self, node_id: &str) -> bool {
        self.claims_of(node_id)
            .iter()
            .any(|claim| !claim.withdrawn && claim.verdict != "accepted")
    }

    /// Has live claims, and the human accepted every one of them.
    pub fn settled(&self, node_id: &str) -> bool {
        let live: Vec<&Claim> = self
            .claims_of(node_id)
            .into_iter()
            .filter(|claim| !claim.withdrawn)
            .collect();
        !live.is_empty() && live.iter().all(|claim| claim.verdict == "accepted")
    }

    pub fn constraint(&self, id: &str) -> Option<&Constraint> {
        self.constraints
            .iter()
            .find(|constraint| constraint.id == id)
    }

    pub fn explanation_beat(&self) -> Option<&ExplanationBeat> {
        let flow = self.explanation.as_ref()?;
        flow.definition
            .beats
            .iter()
            .find(|beat| beat.id == flow.state.current_beat)
    }

    /// Direct children of `id`, in the atlas's own stable order.
    pub fn children_of(&self, id: &str) -> Vec<&Node> {
        self.nodes.iter().filter(|node| node.parent == id).collect()
    }

    /// Nodes with no parent, in the atlas's own stable order.
    pub fn roots(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|node| node.parent.is_empty())
            .collect()
    }

    /// Whether anything is inside this node.
    pub fn is_container(&self, id: &str) -> bool {
        self.nodes.iter().any(|node| node.parent == id)
    }

    /// The state-machine container that directly owns `id`.
    pub fn state_machine_of(&self, id: &str) -> Option<&Node> {
        let parent = self.node(id)?.parent.as_str();
        self.node(parent)
            .filter(|node| node.diagram_kind == "state_machine")
    }

    fn state_machine_view(&self, machine: &Node) -> StateMachineView {
        let states = self.children_of(&machine.id);
        let transitions = self
            .edges
            .iter()
            .filter(|edge| {
                edge.is_transition()
                    && self
                        .node(&edge.from)
                        .is_some_and(|state| state.parent == machine.id)
            })
            .map(|edge| {
                let from = self.node(&edge.from);
                let to = self.node(&edge.to);
                let to_machine = to.map_or("", |state| state.parent.as_str());
                TransitionView {
                    id: edge.id.clone(),
                    from: edge.from.clone(),
                    from_label: from.map_or_else(
                        || format!("<missing {}>", edge.from),
                        |state| state.label.clone(),
                    ),
                    to: edge.to.clone(),
                    to_label: to.map_or_else(
                        || format!("<missing {}>", edge.to),
                        |state| state.label.clone(),
                    ),
                    to_machine: to_machine.to_string(),
                    to_machine_label: self
                        .node(to_machine)
                        .map_or_else(String::new, |container| container.label.clone()),
                    event: edge.event.clone(),
                    guard: edge.guard.clone(),
                    order: edge.transition_order,
                }
            })
            .collect();
        StateMachineView {
            id: machine.id.clone(),
            title: machine.label.clone(),
            states: states
                .into_iter()
                .map(|state| StateView {
                    id: state.id.clone(),
                    label: state.label.clone(),
                    initial: state.state_initial,
                    terminal: state.state_terminal,
                })
                .collect(),
            transitions,
            current_state: living_ink::current_state_sentence(self, machine),
        }
    }

    fn state_machine_views(&self) -> Vec<StateMachineView> {
        self.nodes
            .iter()
            .filter(|node| node.diagram_kind == "state_machine")
            .map(|machine| self.state_machine_view(machine))
            .collect()
    }

    fn describe_state_machines(&self) -> String {
        self.state_machine_views()
            .into_iter()
            .map(|machine| machine.describe())
            .collect()
    }

    /// Every container above `id`, nearest first. Terminates because [`read`]
    /// projects containment as a tree.
    pub fn ancestors(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = self.node(id).map(|node| node.parent.clone());
        while let Some(parent) = at.filter(|parent| !parent.is_empty()) {
            if out.contains(&parent) {
                break;
            }
            at = self.node(&parent).map(|node| node.parent.clone());
            out.push(parent);
        }
        out
    }

    /// Whether `ancestor` contains `of`, at any depth.
    pub fn contains_node(&self, ancestor: &str, of: &str) -> bool {
        !ancestor.is_empty() && self.ancestors(of).iter().any(|id| id == ancestor)
    }

    /// Whether one of these two nodes is inside the other, at any depth.
    ///
    /// The read-back asks this a lot: a child sitting inside its own container
    /// is the structure working, not two cards colliding, and every layout
    /// check that treats boxes as rivals has to skip the pair.
    pub fn related_by_containment(&self, a: &str, b: &str) -> bool {
        self.contains_node(a, b) || self.contains_node(b, a)
    }

    /// How deep `id` sits. A top-level node is `0`.
    pub fn depth(&self, id: &str) -> usize {
        self.ancestors(id).len()
    }

    /// Deepest containment currently projected. A document with only
    /// top-level nodes has depth `0`.
    pub fn deepest_containment(&self) -> usize {
        self.nodes
            .iter()
            .map(|node| self.depth(&node.id))
            .max()
            .unwrap_or_default()
    }

    /// Longest path starting at `id`, including `id`, in stable node order.
    fn deepest_descendant_path(&self, id: &str) -> Vec<String> {
        let mut deepest = vec![id.to_string()];
        for child in self.children_of(id) {
            let mut candidate = vec![id.to_string()];
            candidate.extend(self.deepest_descendant_path(&child.id));
            if candidate.len() > deepest.len() {
                deepest = candidate;
            }
        }
        deepest
    }

    fn object_exists(&self, id: &str) -> bool {
        self.node(id).is_some()
            || self.shape(id).is_some()
            || self.edges.iter().any(|edge| edge.id == id)
            || self.marks.iter().any(|mark| mark.id == id)
            || self.constraint(id).is_some()
    }

    fn semantic_target_exists(&self, id: &str) -> bool {
        self.node(id).is_some() || self.shape(id).is_some() || self.constraint(id).is_some()
    }

    /// Geometry an arrow may bind to. Claims and sized drawing shapes are
    /// first-class diagram targets; strokes and other connectors are not.
    fn binding_bounds(&self, id: &str) -> Option<(f64, f64, f64, f64)> {
        self.node(id).map(Node::bounds).or_else(|| {
            self.shape(id)
                .filter(|shape| shape.is_sized())
                .map(Shape::bounds)
        })
    }

    fn label_of(&self, id: &str) -> String {
        self.node(id)
            .map(|node| node.label.clone())
            .or_else(|| {
                self.shape(id).map(|shape| {
                    if shape.label.is_empty() {
                        format!("{} {id}", shape.form)
                    } else {
                        shape.label.clone()
                    }
                })
            })
            .or_else(|| self.constraint(id).map(Constraint::statement))
            .or_else(|| self.claim(id).map(|claim| claim.text.clone()))
            .unwrap_or_else(|| format!("<missing {id}>"))
    }

    /// The agent's structured read-back of the shared surface — the text that
    /// makes a human edit impossible to miss on the next turn.
    pub fn describe(&self) -> String {
        let unanswered = self
            .marks
            .iter()
            .filter(|mark| mark.answer.is_empty())
            .count();
        let mut out = format!(
            "Shared atlas: {} nodes, {} links, {} shapes, {} constraints, {} marks ({unanswered} unanswered), explanation={}.\n",
            self.nodes.len(),
            self.edges.len(),
            self.shapes.len(),
            self.constraints.len(),
            self.marks.len(),
            self.explanation
                .as_ref()
                .map_or("none", |flow| flow.state.status.as_str()),
        );
        out.push_str(&self.context.describe());
        out.push_str(&architecture::describe(self));
        if !self.decision_drafts.is_empty() {
            out.push_str("\nDECISION DRAFTS (proposals, not human approval)\n");
            for draft in &self.decision_drafts {
                if let Ok(json) = serde_json::to_string(draft) {
                    out.push_str(&json);
                    out.push('\n');
                }
            }
        }

        if self.nodes.is_empty()
            && self.edges.is_empty()
            && self.shapes.is_empty()
            && self.constraints.is_empty()
            && self.marks.is_empty()
            && self.variables.is_empty()
            && self.relations.is_empty()
            && self.explanation.is_none()
        {
            out.push_str("\nThe atlas is empty. Nothing is on the shared page yet.\n");
            return out;
        }

        out.push_str(&format!(
            "\nNODES (deepest containment: {} of {HIERARCHY_MAX_DEPTH})\n",
            self.deepest_containment()
        ));
        for root in self.roots() {
            self.describe_node(&mut out, root, 0);
        }

        out.push_str(&self.describe_state_machines());

        if self.edges.iter().any(|edge| !edge.is_transition()) {
            out.push_str("\nLINKS\n");
            for edge in self.edges.iter().filter(|edge| !edge.is_transition()) {
                out.push_str(&format!(
                    "- [{}] {} -> {}{}{}\n",
                    edge.id,
                    self.endpoint_of(&edge.from),
                    self.endpoint_of(&edge.to),
                    if edge.label.is_empty() { "" } else { " : " },
                    edge.label,
                ));
            }
        }

        if !self.marks.is_empty() {
            out.push_str("\nMARKS (what the human flagged on the map)\n");
            for mark in &self.marks {
                let meaning = match mark.glyph.as_str() {
                    "?" => "does not follow this",
                    "!" => "thinks this is wrong",
                    _ => "says this matters",
                };
                out.push_str(&format!(
                    "- [{}] {} on \"{}\" — {} — \"{}\"\n",
                    mark.id,
                    mark.glyph,
                    self.label_of(&mark.target),
                    meaning,
                    mark.text
                ));
                for context in self
                    .context
                    .entries
                    .iter()
                    .filter(|entry| mark.context_ids.contains(&entry.id))
                {
                    out.push_str(&format!(
                        "    asked at {} scope: {} [{}]\n",
                        context.altitude.label(),
                        context.question,
                        context.id
                    ));
                }
                if mark.answer.is_empty() {
                    out.push_str("    UNANSWERED\n");
                } else {
                    out.push_str(&format!("    answered: {}\n", mark.answer));
                }
            }
        }

        out.push_str(&claims::describe(self));

        out.push_str(&self.describe_explanation());

        // B3 fills this hook with VARIABLES and solve diagnostics. Passing the
        // section into `describe_drawing` places it after CONSTRAINTS and
        // before DRAWING without another central-file edit.
        let agent_ink = agent_ink::describe(self);
        out.push_str(&self.describe_drawing(&agent_ink));
        out.push_str(&self.describe_layout());
        out
    }

    fn describe_explanation(&self) -> String {
        self.explanations
            .iter()
            .map(|flow| self.describe_explanation_flow(flow))
            .collect()
    }

    fn describe_explanation_flow(&self, flow: &ExplanationFlow) -> String {
        let Some(beat) = flow
            .definition
            .beats
            .iter()
            .find(|beat| beat.id == flow.state.current_beat)
        else {
            return format!(
                "\nEXPLANATION FLOW\n[{}] {:?} has invalid current beat {:?}.\n",
                flow.definition.id, flow.definition.title, flow.state.current_beat
            );
        };
        let position = flow
            .definition
            .beats
            .iter()
            .position(|candidate| candidate.id == beat.id)
            .map_or(0, |index| index + 1);
        let mut out = format!(
            "\nEXPLANATION FLOW\n- [{}] {:?} status={} revision={} beat={}/{} [{}] {:?}\n  GOAL: {}\n  INTENT: {}\n  CUE: {}\n",
            flow.definition.id,
            flow.definition.title,
            flow.state.status,
            flow.state.revision,
            position,
            flow.definition.beats.len(),
            beat.id,
            beat.title,
            flow.definition.goal,
            beat.intent,
            beat.cue,
        );
        if !beat.evidence.is_empty() {
            out.push_str(&format!(
                "  ABOUT: {:?}\n  SCENE: {:?}\n",
                flow.definition.about, flow.definition.scene_ids
            ));
            out.push_str("  EVIDENCE:\n");
            for evidence in &beat.evidence {
                out.push_str(&format!(
                    "  - {} on [{}] {:?}\n",
                    evidence.detail,
                    evidence.target_id,
                    self.label_of(&evidence.target_id)
                ));
            }
        }
        if !beat.actions.is_empty() {
            out.push_str("  VISUAL ACTIONS:\n");
            for action in &beat.actions {
                let span = action
                    .cue_span
                    .map(|[start, end]| format!(" cue_chars={start}..{end}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  - {} [{}] {:?}{span}\n",
                    action.kind,
                    action.target_id,
                    self.label_of(&action.target_id)
                ));
            }
        }
        out.push_str(&format!(
            "  ADVANCE: mode={}{}\n",
            beat.advance.mode,
            if beat.advance.prompt.is_empty() {
                String::new()
            } else {
                format!(" prompt={:?}", beat.advance.prompt)
            }
        ));
        for transition in &beat.advance.transitions {
            let control = transition
                .control
                .as_deref()
                .map(|control| format!(" control={control}"))
                .unwrap_or_default();
            let targets = if transition.target_ids.is_empty() {
                String::new()
            } else {
                format!(" targets=[{}]", transition.target_ids.join(", "))
            };
            let phrases = if transition.phrases.is_empty() {
                String::new()
            } else {
                format!(" phrases={:?}", transition.phrases)
            };
            out.push_str(&format!(
                "  - transition={} label={:?} next={}{}{}{}\n",
                transition.id,
                transition.label,
                transition.next.as_deref().unwrap_or("COMPLETE"),
                control,
                targets,
                phrases,
            ));
        }
        if !flow.state.last_transition.is_empty() {
            out.push_str(&format!(
                "  LAST INPUT: transition={} response={:?} selected=[{}] by={}\n",
                flow.state.last_transition,
                flow.state.last_response,
                flow.state.last_selected_target_ids.join(", "),
                flow.state.touched_by,
            ));
        }
        out
    }

    /// One node's line, then everything inside it, indented one step further.
    ///
    /// The nesting is the whole point of the section: a flat list of twelve
    /// cards and a separate note that three of them are "in" something is two
    /// facts the reader has to join by hand, and the join is exactly what a
    /// container is for. Depth is carried by indentation rather than by a
    /// `parent=` field per line, because the reader of this text is composing
    /// a picture and an indent is the shape of the answer.
    fn describe_node(&self, out: &mut String, node: &Node, depth: usize) {
        let indent = "  ".repeat(depth);
        let children = self.children_of(&node.id);
        out.push_str(&format!(
            "{indent}- [{}] \"{}\" tone={} status={}",
            node.id, node.label, node.tone, node.status,
        ));
        // Only the styles this card is actually saying something with, so a
        // page where one node is emphasised reads as one emphasised node
        // rather than as forty declarations of the default.
        let styled = node.style_words();
        if !styled.is_empty() {
            out.push_str(&format!(" {}", styled.join(" ")));
        }
        out.push_str(&format!(
            " box=({:.0},{:.0} {:.0}x{:.0}{})",
            node.x,
            node.y,
            node.w,
            node.h,
            if node.measured { "" } else { " est" },
        ));
        if !node.path.is_empty() {
            out.push_str(&format!(" source={}", node.source_ref()));
        }
        out.push_str(&format!(" created_by={}", node.created_by));
        if node.touched_by != node.created_by && !node.touched_by.is_empty() {
            out.push_str(&format!(
                " LAST-EDITED-BY={} (they changed this after you wrote it)",
                node.touched_by
            ));
        }
        if !children.is_empty() {
            out.push_str(&format!(" CONTAINS {}", children.len()));
        }
        out.push('\n');
        if !node.note.is_empty() {
            out.push_str(&format!("{indent}    note: {}\n", node.note));
        }
        for child in children {
            self.describe_node(out, child, depth + 1);
        }
    }

    /// A link endpoint, said with the container it belongs to.
    ///
    /// Without this a relation across two levels reads as two bare titles and
    /// loses the half that says which subsystem is talking to which. The
    /// container is named rather than indented because a link line has no
    /// place in the tree to sit under.
    fn endpoint_of(&self, id: &str) -> String {
        let named = format!("\"{}\"", self.label_of(id));
        match self.node(id).filter(|node| !node.parent.is_empty()) {
            Some(node) => format!("{named} (inside \"{}\")", self.label_of(&node.parent)),
            None => named,
        }
    }

    /// The drawn layer, read as claims rather than coordinates.
    ///
    /// A human circling three cards is one gesture and a whole sentence. If
    /// this section reported points, that sentence would reach the agent as
    /// noise — so every shape is named by what it is doing to the nodes it
    /// touches, and a shape doing nothing to any of them says so plainly
    /// rather than being dressed up as meaningful.
    fn describe_drawing(&self, agent_ink: &str) -> String {
        let mut out = String::new();
        if !self.constraints.is_empty() {
            out.push_str("\nCONSTRAINTS\n");
            if self.constraint_state.status == "unsat" {
                out.push_str(&format!(
                    "UNSAT minimal core: {}\n",
                    self.constraint_state.core.join(", ")
                ));
            }
            let mut constraints = self.constraints.iter().collect::<Vec<_>>();
            constraints.sort_by(|left, right| {
                let left_core = self
                    .constraint_state
                    .core
                    .iter()
                    .position(|id| id == &left.id);
                let right_core = self
                    .constraint_state
                    .core
                    .iter()
                    .position(|id| id == &right.id);
                match (left_core, right_core) {
                    (Some(left), Some(right)) => left.cmp(&right),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => left.id.cmp(&right.id),
                }
            });
            for constraint in constraints {
                out.push_str(&format!("- {}\n", constraint.description()));
            }
        }
        out.push_str(agent_ink);
        if self.shapes.is_empty() {
            return out;
        }
        out.push_str(
            "\nDRAWING (marks made on top of the map — a shape is reported by what it does to the nodes, or the drawn matter, under it)\n",
        );
        let quoted = |ids: &[String]| {
            ids.iter()
                .map(|id| format!("\"{}\"", self.label_of(id)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for reading in readings(self) {
            let Some(shape) = self.shapes.iter().find(|s| s.id == reading.shape) else {
                continue;
            };
            let who = &shape.created_by;
            if shape.form == "segment" {
                let state = if shape.segment_status.is_empty() {
                    "invalid"
                } else {
                    shape.segment_status.as_str()
                };
                let tags = if shape.segment_tags.is_empty() {
                    "none".to_string()
                } else {
                    shape.segment_tags.join(",")
                };
                let ocr = if shape.segment_ocr.is_empty() {
                    "none".to_string()
                } else {
                    shape.segment_ocr.join(" | ")
                };
                let semantics_model = shape
                    .segment_semantics_receipt
                    .get("model")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("none");
                let part_count = shape.segment_parts.as_array().map(Vec::len).unwrap_or(0);
                let motion = if shape.segment_motion.is_null() {
                    "none".to_string()
                } else {
                    match serde_json::from_value::<SegmentMotionProgram>(
                        shape.segment_motion.clone(),
                    ) {
                        Ok(program) => format!(
                            "\"{}\" compositing={} tracks={} targets={}",
                            program.label,
                            program.child_compositing,
                            program.tracks.len(),
                            program
                                .tracks
                                .iter()
                                .map(|track| track.target_ids.len())
                                .sum::<usize>()
                        ),
                        Err(_) => "invalid".to_string(),
                    }
                };
                out.push_str(&format!(
                    "- [{}] {who}'s segment \"{}\" status={state} generation={} semantics={}:{} segmenter={} mask={} {}x{} predicted_iou={:.3} parts={} parent={} flap={:.2} pivot=({:.2},{:.2}) tags={} ocr={} box=({:.0},{:.0} {:.0}x{:.0}) created_by={} LAST-EDITED-BY={} animation={} motion={}\n",
                    shape.id,
                    shape.label,
                    shape.segment_generation,
                    shape.segment_semantics_source,
                    semantics_model,
                    non_empty(shape.segment_model_id.clone(), "pending"),
                    non_empty(shape.segment_mask_encoding.clone(), "pending"),
                    shape.segment_mask_width,
                    shape.segment_mask_height,
                    shape.segment_predicted_iou,
                    part_count,
                    non_empty(shape.segment_parent_id.clone(), "root"),
                    shape.segment_flap,
                    shape.segment_pivot_x,
                    shape.segment_pivot_y,
                    tags,
                    ocr,
                    shape.x,
                    shape.y,
                    shape.w,
                    shape.h,
                    shape.created_by,
                    shape.touched_by,
                    shape.segment_animation,
                    motion,
                ));
                continue;
            }
            // A text's label IS the shape; the claim below prints it. Every
            // other form's label is a caption on top of what it does.
            let label = if shape.label.is_empty() || shape.form == "text" {
                String::new()
            } else {
                format!(" labelled \"{}\"", shape.label)
            };
            let claim = match reading.relation.as_str() {
                "encloses" => format!(
                    "ENCLOSES {} — {who} is grouping them",
                    quoted(&reading.targets)
                ),
                "crosses-out" => format!(
                    "CROSSES-OUT {} — {who} is striking it through",
                    quoted(&reading.targets)
                ),
                "points-at" => format!(
                    "POINTS-AT {} — {who} is singling it out",
                    quoted(&reading.targets)
                ),
                "starts-at" => format!(
                    "STARTS-AT {} and ends on nothing — {who} is pointing away from it, at something not on the map",
                    quoted(&reading.targets)
                ),
                "connects" => format!(
                    "CONNECTS {} -> {} — {who} is relating them",
                    quoted(&reading.targets[..1]),
                    quoted(&reading.targets[1..])
                ),
                "says" => {
                    // Targets are either all cards or all drawn matter, never
                    // mixed: ink is consulted only when no card is near. The
                    // word in the sentence keeps that distinction readable.
                    let kind = if reading.targets.iter().all(|id| self.node(id).is_some()) {
                        "nodes"
                    } else {
                        "drawings"
                    };
                    let proximity = match reading.targets.len() {
                        0 => " touching no node".to_string(),
                        1 => format!(" NEAR {}", quoted(&reading.targets)),
                        count => format!(" near {count} {kind}: {}", quoted(&reading.targets)),
                    };
                    format!("SAYS \"{}\"{proximity}", shape.label)
                }
                _ => {
                    let (l, t, r, b) = shape.bounds();
                    format!(
                        "touches no node — {:.0}x{:.0}px at ({l:.0},{t:.0})",
                        r - l,
                        b - t
                    )
                }
            };
            let style = shape.style_note(self);
            out.push_str(&format!(
                "- [{}] {who}'s {}{style}{label} {claim}",
                shape.id, shape.form
            ));
            // A card the region only half covers is named right here rather
            // than left to PROBLEMS, because the ambiguity is part of what
            // this shape means and the agent reads this line to find out.
            if !reading.partial.is_empty() {
                out.push_str(&format!(
                    " — and only partly covers {}, which reads as neither in nor out",
                    quoted(&reading.partial)
                ));
            }
            out.push('\n');
        }
        out
    }

    /// The geometry the agent has no eyes for.
    fn describe_layout(&self) -> String {
        let report = layout(self);
        let mut out = String::new();
        let Some((left, top, right, bottom)) = report.extent else {
            return out;
        };
        out.push_str(&format!(
            "\nLAYOUT\nThe drawing spans ({left:.0},{top:.0}) to ({right:.0},{bottom:.0}) — {:.0}x{:.0}px.\n",
            right - left,
            bottom - top
        ));
        if report.unmeasured > 0 {
            out.push_str(&format!(
                "{} node(s) marked `est` have a guessed height: no browser has rendered them yet, so treat their geometry as approximate.\n",
                report.unmeasured
            ));
        }

        if report.problems.is_empty() {
            out.push_str("No layout problems detected in the available geometry.\n");
            return out;
        }
        out.push_str(&format!(
            "\nPROBLEMS ({}) — computed from stored geometry; estimated heights are identified above. Inspect the named objects to repair them\n",
            report.problems.len()
        ));
        for problem in &report.problems {
            out.push_str(&format!(
                "- {} [{}]\n",
                problem.detail,
                problem.ids.join(", ")
            ));
        }
        out
    }
}

impl Node {
    /// `path:lines` when a range is pinned, bare path otherwise.
    pub fn source_ref(&self) -> String {
        if self.lines.is_empty() {
            self.path.clone()
        } else {
            format!("{}:{}", self.path, self.lines)
        }
    }
}

/// Read the whole atlas in ONE transaction.
///
/// Rough spot worth naming: `Scene::snapshot()` returns renderer-shaped
/// `ObjectSnapshot`s (x/y/color/z/text) and `get_prop` opens a fresh
/// transaction per key, so neither serves a DOM surface with domain
/// properties. `Scene::doc()` is public, so this reads the `objects` map
/// directly instead of paying 10 transactions per node.
pub fn read(scene: &Scene) -> Result<Atlas, AtlasError> {
    let doc = scene.doc();
    let objects = doc.get_or_insert_map("objects");
    let txn = doc
        .try_transact()
        .map_err(|error| format!("atlas is busy: {error}"))?;

    let mut atlas = Atlas::default();
    let mut exploration_members = Vec::new();
    for (id, value) in objects.iter(&txn) {
        let Out::YMap(map) = value else { continue };
        let string = |key: &str| -> String {
            match map.get(&txn, key) {
                Some(Out::Any(Any::String(text))) => text.to_string(),
                _ => String::new(),
            }
        };
        let optional_number = |key: &str| -> Option<f64> {
            match map.get(&txn, key) {
                Some(Out::Any(Any::Number(value))) => Some(value),
                Some(Out::Any(Any::BigInt(value))) => Some(value as f64),
                _ => None,
            }
        };
        let boolean =
            |key: &str| -> bool { matches!(map.get(&txn, key), Some(Out::Any(Any::Bool(true)))) };
        let number = |key: &str| -> f64 { optional_number(key).unwrap_or(0.0) };
        let kind = string("kind");
        let exploration_id = string("exploration_id");
        if !exploration_id.is_empty() {
            exploration_members.push((id.to_string(), exploration_id));
        }
        if agent_ink::project_object(&kind, &string, &optional_number, &mut atlas)? {
            continue;
        }
        let created_by = string("created_by");
        match kind.as_str() {
            KIND_NODE => {
                let width = if number("w") > 0.0 {
                    number("w")
                } else {
                    DEFAULT_WIDTH
                };
                let mut node = Node {
                    id: id.to_string(),
                    label: string("label"),
                    path: string("path"),
                    lines: string("lines"),
                    note: string("note"),
                    tone: non_empty(string("tone"), "concept"),
                    status: non_empty(string("status"), "open"),
                    cemented: string("cemented"),
                    x: number("x"),
                    y: number("y"),
                    w: width,
                    h: 0.0,
                    measured: number(K_HEIGHT) > 0.0,
                    touched_by: non_empty(string("touched_by"), &created_by),
                    created_by,
                    parent: string(K_PARENT),
                    diagram_kind: one_of_or(string(K_DIAGRAM_KIND), &DIAGRAM_KINDS[..2], ""),
                    state_initial: boolean(K_STATE_INITIAL),
                    state_terminal: boolean(K_STATE_TERMINAL),
                    state_variable: string(living_ink::K_STATE_VARIABLE),
                    state_previous_value: optional_number(living_ink::K_STATE_PREVIOUS_VALUE),
                    // An unrecognised style word reaching the projection means
                    // a replica wrote a vocabulary this build does not have.
                    // It falls back to the default rather than through to the
                    // renderer, because an unknown emphasis draws as nothing
                    // and would read back as a claim the picture is not making.
                    color: one_of_or(string(K_COLOR), INKS, ""),
                    emphasis: one_of_or(string(K_EMPHASIS), EMPHASES, DEFAULT_EMPHASIS),
                    size: one_of_or(string(K_SIZE), SIZES, DEFAULT_SIZE),
                    kind: one_of_or(string(K_NODE_KIND), NODE_KINDS, DEFAULT_KIND),
                };
                node.h = if node.measured {
                    number(K_HEIGHT)
                } else {
                    estimate_height(&node)
                };
                atlas.nodes.push(node);
            }
            KIND_EDGE => atlas.edges.push(Edge {
                id: id.to_string(),
                from: string("from"),
                to: string("to"),
                label: string("label"),
                event: string(K_TRANSITION_EVENT),
                guard: string(K_TRANSITION_GUARD),
                transition_order: number(K_TRANSITION_ORDER).max(0.0) as u64,
                touched_by: non_empty(string("touched_by"), &created_by),
                created_by,
            }),
            KIND_MARK => atlas.marks.push(Mark {
                id: id.to_string(),
                target: string("target"),
                glyph: string("glyph"),
                text: string("text"),
                answer: string("answer"),
                context_ids: string("context_ids")
                    .split(',')
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect(),
                created_by,
            }),
            KIND_CLAIM => atlas.claims.push(Claim {
                id: id.to_string(),
                about: string("about"),
                text: string("text"),
                basis: one_of_or(string("basis"), BASES, "unknown"),
                path: string("path"),
                lines: string("lines"),
                revision: string("revision"),
                verdict: one_of_or(string("verdict"), VERDICTS, "open"),
                withdrawn: boolean("withdrawn"),
                touched_by: non_empty(string("touched_by"), &created_by),
                created_by,
            }),
            KIND_SHAPE => {
                let form = non_empty(string("form"), "ink");
                // A text is as tall as its own words, so its height is
                // measured by the browser and guessed until one has rendered
                // it — the same deal a card's height is on, for the same
                // reason. Every other form is exactly as big as it was drawn.
                let is_text = form == "text";
                let rounded_by_default = form == "rect" || form == "frame";
                let measured = !is_text || number(K_HEIGHT) > 0.0;
                let mut shape = Shape {
                    id: id.to_string(),
                    form,
                    x: number("x"),
                    y: number("y"),
                    w: number("w"),
                    h: number(K_HEIGHT),
                    points: parse_points(&string(K_POINTS)),
                    from: string("from"),
                    to: string("to"),
                    head: non_empty(string("head"), "none"),
                    ink: non_empty(string("ink"), "slate"),
                    fill: non_empty(string("fill"), "none"),
                    label: string("label"),
                    stroke_width: {
                        let width = number("stroke_width");
                        if width > 0.0 {
                            width
                        } else {
                            DEFAULT_STROKE_WIDTH
                        }
                    },
                    stroke_style: one_of_or(string("stroke_style"), STROKE_STYLES, "solid"),
                    opacity: match optional_number("opacity") {
                        Some(opacity) => opacity.clamp(OPACITY_RANGE.0, OPACITY_RANGE.1),
                        None => OPACITY_RANGE.1,
                    },
                    roundness: one_of_or(
                        string("roundness"),
                        ROUNDNESS,
                        // A box drew with rounded corners before this field
                        // existed, so the absent value keeps that look.
                        if rounded_by_default { "round" } else { "sharp" },
                    ),
                    font_size: number("font_size").max(0.0),
                    angle: number("angle"),
                    groups: parse_groups(&string("groups")),
                    frame: string("frame"),
                    z: number("z"),
                    measured,
                    touched_by: non_empty(string("touched_by"), &created_by),
                    created_by,
                    segment_status: string("segment_status"),
                    segment_tags: parse_json_string_list(&string("segment_tags")),
                    segment_ocr: parse_json_string_list(&string("segment_ocr")),
                    segment_semantics_source: non_empty(
                        string("segment_semantics_source"),
                        "oracle",
                    ),
                    segment_semantics_receipt: serde_json::from_str(&string(
                        "segment_semantics_receipt",
                    ))
                    .unwrap_or(JsonValue::Null),
                    segment_source_url: string("segment_source_url"),
                    segment_source_sha256: string("segment_source_sha256"),
                    segment_source_width: whole_number(number("segment_source_width")),
                    segment_source_height: whole_number(number("segment_source_height")),
                    segment_prompt_box: parse_json_numbers(&string("segment_prompt_box")),
                    segment_occlusion_box: parse_json_numbers(&string("segment_occlusion_box")),
                    segment_generation: whole_number(number("segment_generation")),
                    segment_model_id: string("segment_model_id"),
                    segment_encoder_sha256: string("segment_encoder_sha256"),
                    segment_decoder_sha256: string("segment_decoder_sha256"),
                    segment_predicted_iou: number("segment_predicted_iou"),
                    segment_mask_encoding: string("segment_mask_encoding"),
                    segment_mask_width: whole_number(number("segment_mask_width")),
                    segment_mask_height: whole_number(number("segment_mask_height")),
                    segment_mask_runs: string("segment_mask_runs"),
                    segment_mask_source_box: parse_json_numbers(&string("segment_mask_source_box")),
                    segment_wing_prompts: serde_json::from_str(&string("segment_wing_prompts"))
                        .unwrap_or_else(|_| serde_json::json!({})),
                    segment_parts: serde_json::from_str(&string("segment_parts"))
                        .unwrap_or_else(|_| serde_json::json!([])),
                    segment_parent_id: string("segment_parent_id"),
                    segment_flap: number("segment_flap").clamp(-1.0, 1.0),
                    segment_animation: non_empty(string("segment_animation"), "none"),
                    segment_motion: serde_json::from_str(&string("segment_motion"))
                        .unwrap_or(JsonValue::Null),
                    segment_pivot_x: optional_number("segment_pivot_x")
                        .unwrap_or(0.5)
                        .clamp(0.0, 1.0),
                    segment_pivot_y: optional_number("segment_pivot_y")
                        .unwrap_or(0.5)
                        .clamp(0.0, 1.0),
                    segment_receipt: serde_json::from_str(&string("segment_receipt"))
                        .unwrap_or(JsonValue::Null),
                };
                if is_text {
                    if shape.w <= 0.0 {
                        shape.w = TEXT_WIDTH;
                    }
                    if !measured {
                        shape.h = estimate_text_height(&shape);
                    }
                }
                atlas.shapes.push(shape);
            }
            KIND_CONSTRAINT => {
                let op = string("op");
                let members = string("members")
                    .split(',')
                    .filter(|member| !member.is_empty())
                    .map(str::to_string)
                    .collect();
                atlas.constraints.push(Constraint {
                    id: id.to_string(),
                    axis: (op == "sequence").then(|| non_empty(string("axis"), "x")),
                    text: (op == "labels").then(|| string("text")),
                    op,
                    members,
                    salience: non_empty(string("salience"), "back"),
                    status: "sat".to_string(),
                    created_by,
                });
            }
            decision::KIND => {
                atlas.decision_drafts.push(decision::project(
                    id,
                    &string("definition"),
                    created_by,
                )?);
            }
            context::KIND => {
                context::project(&mut atlas, id, &string("definition"), created_by)?;
            }
            architecture::KIND => {
                atlas.proposals.push(architecture::project(
                    id,
                    &string("definition"),
                    created_by,
                )?);
            }
            KIND_EXPLANATION_FLOW => {
                let definition =
                    serde_json::from_str::<ExplanationFlowDefinition>(&string("definition"))
                        .map_err(|error| format!("invalid explanation definition: {error}"))?;
                let state = serde_json::from_str::<ExplanationFlowState>(&string("state"))
                    .map_err(|error| format!("invalid explanation state: {error}"))?;
                atlas.explanations.push(ExplanationFlow {
                    object_id: id.to_string(),
                    definition,
                    state,
                    created_by,
                });
            }
            _ => continue,
        }
    }

    atlas.context.finalize()?;
    atlas.proposals.sort_by(|a, b| a.id.cmp(&b.id));
    atlas.decision_drafts.sort_by(|a, b| a.id.cmp(&b.id));

    // Containment is a tree by projection, not by hope. Two things can put a
    // non-tree in the document and neither is a caller bug that [`place_node`]
    // could have refused: a parent whose node was deleted on another replica,
    // and two replicas reparenting concurrently so that per-key LWW lands
    // A-inside-B and B-inside-A. Both are healed to the top level here, once,
    // so that every walk upwards in this file terminates.
    atlas
        .explanations
        .sort_by(|a, b| a.object_id.cmp(&b.object_id));
    for (id, owner) in exploration_members {
        for flow in &mut atlas.explanations {
            if flow.definition.id == owner && !flow.definition.scene_ids.contains(&id) {
                flow.definition.scene_ids.push(id.clone());
            }
        }
    }
    let mut flow_names = std::collections::BTreeSet::new();
    let mut owned_ids = std::collections::BTreeSet::new();
    for flow in &atlas.explanations {
        if !flow_names.insert(&flow.definition.id) {
            return Err(format!(
                "conflicting exploration identity {:?}; resolve duplicate objects before continuing",
                flow.definition.id
            ));
        }
        for id in &flow.definition.scene_ids {
            if !owned_ids.insert(id) {
                return Err(format!(
                    "conflicting exploration ownership of {id:?}; resolve overlapping scopes before continuing"
                ));
            }
        }
    }
    atlas.explanation = atlas.explanations.last().cloned();
    heal_containment(&mut atlas.nodes);

    // A container is the frame around what it owns, not a hidden card parked
    // near those members. Derive deepest frames first so a parent enclosing a
    // child container includes that child's full frame. This also means a
    // later browser measurement of any leaf immediately expands every parent
    // above it on both replicas without another authoring write.
    size_containers_to_content(&mut atlas.nodes);

    // Stable order: the CRDT map iterates in hash order, which would reshuffle
    // the agent's read-back and the human's DOM on every unrelated write.
    atlas.nodes.sort_by(|a, b| {
        a.y.total_cmp(&b.y)
            .then(a.x.total_cmp(&b.x))
            .then_with(|| a.id.cmp(&b.id))
    });
    atlas.edges.sort_by(|a, b| a.id.cmp(&b.id));
    atlas.marks.sort_by(|a, b| a.id.cmp(&b.id));
    claims::heal(&mut atlas);
    atlas
        .constraints
        .sort_by(|left, right| left.id.cmp(&right.id));
    // Paint order first, id second. The renderer appends in this order, so the
    // sort IS the z-order; ties break on id so two shapes drawn in the same
    // frame do not swap places between reads.
    atlas
        .shapes
        .sort_by(|a, b| a.z.total_cmp(&b.z).then_with(|| a.id.cmp(&b.id)));
    let object_ids = atlas
        .nodes
        .iter()
        .map(|object| object.id.clone())
        .chain(atlas.edges.iter().map(|object| object.id.clone()))
        .chain(atlas.marks.iter().map(|object| object.id.clone()))
        .chain(atlas.shapes.iter().map(|object| object.id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    for constraint in &mut atlas.constraints {
        if constraint
            .members
            .iter()
            .any(|member| !object_ids.contains(member))
        {
            constraint.status = "broken".to_string();
        }
    }
    atlas.constraint_state = compute_constraint_state(&atlas.constraints);
    // Whether a `represents` binding is actually driving a shape or node's
    // geometry, or has gone dangling since it was authored, depends on the
    // whole atlas being assembled, not just the variable's own object — so
    // this runs last, after every shape and node above has been projected.
    agent_ink::finalize_bindings(&mut atlas);
    // Same reasoning, same place: a measured variable's value depends on
    // every shape and node above having already been projected, not just
    // its own object.
    agent_ink::finalize_measurements(&mut atlas);
    // Same reasoning, same place: `describe` needs to know which stored step,
    // if any, the live values still agree with, and every step object the
    // main loop above visited is now sitting in `atlas.agent_ink_steps`
    // (see the `step::KIND_STEP` arm of `agent_ink::project_object`).
    agent_ink::finalize_committed_step(&mut atlas);
    for flow in &atlas.explanations {
        validate_explanation_structure(&flow.definition)?;
        validate_explanation_state(&flow.definition, &flow.state)?;
    }
    Ok(atlas)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SequenceRelation {
    axis: String,
    from: String,
    to: String,
    constraint: String,
}

/// The CRDT key holding a node's container.
const K_PARENT: &str = "parent";
/// The CRDT key holding a container's typed diagram role.
const K_DIAGRAM_KIND: &str = "diagram_kind";

/// Style keys.
///
/// Two of the four are prefixed because the shared object space has already
/// spent the bare name, and both collisions are load-bearing rather than
/// stylistic. `kind` is the discriminator [`read`] dispatches on, so a node
/// whose style overwrote it would vanish from the projection entirely.
/// `color` is reserved as a NUMERIC field by `ag_ui_canvas::scene`, which
/// refuses the write outright, and that refusal is the good outcome: the
/// palette name is a string and packing it into a float would lose the word
/// the read-back exists to hand back. Renaming either of these back to the
/// bare form breaks a round trip that
/// `a_styled_node_survives_a_write_and_read_round_trip` will catch.
const K_COLOR: &str = "node_color";
const K_EMPHASIS: &str = "emphasis";
const K_SIZE: &str = "size";
const K_NODE_KIND: &str = "node_kind";

/// A stored word if the vocabulary still has it, the fallback otherwise.
pub(crate) fn one_of_or(value: String, allowed: &[&str], fallback: &str) -> String {
    if allowed.contains(&value.as_str()) {
        value
    } else {
        fallback.to_string()
    }
}

/// Force the projected `parent` pointers into a forest.
///
/// Two repairs, both silent by necessity — there is nobody to report a merge
/// artefact to at projection time, and the alternative to repairing it is a
/// read-back that hangs. A dangling parent becomes a top-level node, which is
/// the same disposition [`remove`] writes when it deletes a container on this
/// replica. A cycle is broken at every node on it, because there is no
/// principled way to choose which of two concurrent reparents was "the real
/// one" and picking one would invent an intent nobody expressed.
fn heal_containment(nodes: &mut [Node]) {
    let known: std::collections::BTreeSet<String> =
        nodes.iter().map(|node| node.id.clone()).collect();
    for node in nodes.iter_mut() {
        if node.parent == node.id || !known.contains(&node.parent) {
            node.parent.clear();
        }
    }
    let parents: std::collections::BTreeMap<String, String> = nodes
        .iter()
        .map(|node| (node.id.clone(), node.parent.clone()))
        .collect();
    let mut on_a_cycle = std::collections::BTreeSet::new();
    for node in nodes.iter() {
        let mut seen = vec![node.id.clone()];
        let mut at = node.parent.clone();
        while !at.is_empty() {
            if let Some(start) = seen.iter().position(|id| *id == at) {
                // Only the ring itself, not the path that walked into it: a
                // node hanging off a cycle keeps a parent that is about to
                // become top level, which is a tree again.
                on_a_cycle.extend(seen[start..].iter().cloned());
                break;
            }
            seen.push(at.clone());
            at = parents.get(&at).cloned().unwrap_or_default();
        }
    }
    for node in nodes.iter_mut() {
        if on_a_cycle.contains(&node.id) {
            node.parent.clear();
        }
    }
}

/// Replace every container's projected box with the frame around its direct
/// content. The CRDT keeps the node's last ordinary x/y/w values so an orphaned
/// container can become a card again; while it owns children, those parked
/// values are not the geometry the human sees and therefore are not the
/// geometry the read-back is allowed to report.
fn size_containers_to_content(nodes: &mut [Node]) {
    let depth = |start: usize, nodes: &[Node]| {
        let mut at = nodes[start].parent.as_str();
        let mut depth = 0usize;
        while !at.is_empty() {
            let Some(parent) = nodes.iter().find(|node| node.id == at) else {
                break;
            };
            depth += 1;
            at = parent.parent.as_str();
        }
        depth
    };
    let max_depth = (0..nodes.len())
        .map(|index| depth(index, nodes))
        .max()
        .unwrap_or_default();

    for current_depth in (0..=max_depth).rev() {
        let updates = (0..nodes.len())
            .filter(|&index| depth(index, nodes) == current_depth)
            .filter_map(|index| {
                let children = nodes
                    .iter()
                    .filter(|node| node.parent == nodes[index].id)
                    .collect::<Vec<_>>();
                if children.is_empty() {
                    return None;
                }
                let (left, top, right, bottom) = children.iter().fold(
                    (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                    |(left, top, right, bottom), child| {
                        let (cl, ct, cr, cb) = child.bounds();
                        (left.min(cl), top.min(ct), right.max(cr), bottom.max(cb))
                    },
                );
                Some((
                    index,
                    (
                        left - CONTAINER_PAD,
                        top - CONTAINER_PAD - CONTAINER_TITLE_HEIGHT,
                        right + CONTAINER_PAD,
                        bottom + CONTAINER_PAD,
                    ),
                    children.iter().all(|child| child.measured),
                ))
            })
            .collect::<Vec<_>>();

        for (index, (left, top, right, bottom), measured) in updates {
            let node = &mut nodes[index];
            node.x = left;
            node.y = top;
            node.w = right - left;
            node.h = bottom - top;
            node.measured = measured;
        }
    }
}

fn compute_constraint_state(constraints: &[Constraint]) -> ConstraintState {
    let mut relations = constraints
        .iter()
        .filter(|constraint| constraint.op == "sequence" && constraint.status == "sat")
        .flat_map(|constraint| {
            constraint
                .members
                .windows(2)
                .map(move |members| SequenceRelation {
                    axis: constraint.axis.clone().unwrap_or_else(|| "x".to_string()),
                    from: members[0].clone(),
                    to: members[1].clone(),
                    constraint: constraint.id.clone(),
                })
        })
        .collect::<Vec<_>>();
    relations.sort();

    let mut best: Option<(usize, Vec<String>)> = None;
    for relation in &relations {
        let Some(path) =
            shortest_sequence_path(&relations, &relation.axis, &relation.to, &relation.from)
        else {
            continue;
        };
        let mut core = vec![relation.constraint.clone()];
        core.extend(
            path.iter()
                .map(|index| relations[*index].constraint.clone()),
        );
        core.sort();
        core.dedup();
        let candidate = (path.len() + 1, core);
        if best.as_ref().is_none_or(|current| candidate < *current) {
            best = Some(candidate);
        }
    }

    match best {
        Some((_, core)) => ConstraintState {
            status: "unsat".to_string(),
            core,
        },
        None => ConstraintState::default(),
    }
}

fn shortest_sequence_path(
    relations: &[SequenceRelation],
    axis: &str,
    start: &str,
    goal: &str,
) -> Option<Vec<usize>> {
    let mut queue = std::collections::VecDeque::from([(start.to_string(), Vec::new())]);
    let mut visited = std::collections::BTreeSet::from([start.to_string()]);
    while let Some((at, path)) = queue.pop_front() {
        if at == goal {
            return Some(path);
        }
        for (index, relation) in relations.iter().enumerate() {
            if relation.axis != axis || relation.from != at || visited.contains(&relation.to) {
                continue;
            }
            let mut next_path = path.clone();
            next_path.push(index);
            visited.insert(relation.to.clone());
            queue.push_back((relation.to.clone(), next_path));
        }
    }
    None
}

pub(crate) fn non_empty(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

// ── mutation vocabulary ───────────────────────────────────────────────────

/// A create-or-update for one node. Absent fields are left alone on update and
/// defaulted on create, so the agent can nudge one property without restating
/// the node — and without clobbering a human edit to a field it never touched.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodePatch {
    pub id: Option<String>,
    pub label: Option<String>,
    pub path: Option<String>,
    pub lines: Option<String>,
    pub note: Option<String>,
    pub tone: Option<String>,
    pub status: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    /// The container this node goes inside. `Some("")` moves it back to the
    /// top level, which is a different request from `None` — "leave the
    /// containment alone" — and the two have to stay distinguishable or a
    /// caller nudging a label would silently empty the tree.
    pub parent: Option<String>,
    /// One of [`INKS`], or `Some("")` to take the colour off again.
    pub color: Option<String>,
    /// One of [`EMPHASES`].
    pub emphasis: Option<String>,
    /// One of [`SIZES`].
    pub size: Option<String>,
    /// One of [`NODE_KINDS`].
    pub kind: Option<String>,
}

/// One node inside a batch, optionally carrying a batch-local name.
///
/// A newly created node's id is generated by the write, so a model composing a
/// subgraph in a single call has no way to refer to a node it is creating in
/// that same call. `reference` is that handle: any name the caller picks,
/// meaningful only for the duration of one [`draw`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodeDraft {
    pub reference: Option<String>,
    pub patch: NodePatch,
}

/// One relationship inside a batch. `from`/`to` are either an existing node id
/// or a [`NodeDraft::reference`] from the same batch.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkDraft {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// Fields that may change on an existing state-machine transition.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TransitionPatch {
    pub from: Option<String>,
    pub to: Option<String>,
    pub event: Option<String>,
    pub guard: Option<String>,
}

/// What one [`draw`] actually put on the page.
#[derive(Debug, Clone, Default)]
pub struct Drawn {
    /// `(reference, id)` per node, in the order written.
    pub nodes: Vec<(Option<String>, String)>,
    pub edges: Vec<String>,
}

impl Drawn {
    fn resolve(&self, name: &str) -> Option<&str> {
        self.nodes
            .iter()
            .find_map(|(reference, id)| (reference.as_deref() == Some(name)).then_some(id.as_str()))
    }
}

/// Write a whole subgraph — many nodes and the links between them — in one
/// call.
///
/// This exists for latency, and the latency is not in the writing. Each node
/// placed through [`place_node`] costs a full model round trip: propose the
/// call, wait for the completion, dispatch, then start the next one. Drawing a
/// twelve-node map that way is twelve round trips of thinking to produce a few
/// milliseconds of mutation, and the human watches an almost-empty page for
/// all of it.
///
/// Everything that can be checked without writing is checked first — capacity
/// for the whole batch, unique references, and every link endpoint resolving
/// to either a batch reference or a node already on the atlas. Source-reference
/// verification stays with the caller, which owns filesystem access.
///
/// This is **not** a transaction. The atlas is a live CRDT with a browser peer
/// attached, so there is no snapshot to roll back to. If a write fails partway,
/// the error names exactly what already landed rather than implying nothing
/// did — the read-back is authoritative either way.
pub fn draw(
    scene: &mut Scene,
    nodes: &[NodeDraft],
    links: &[LinkDraft],
    author: &Author,
) -> Result<Drawn, AtlasError> {
    if nodes.is_empty() && links.is_empty() {
        return Err("a draw needs at least one node or link".to_string());
    }

    let atlas = read(scene)?;
    let creating = nodes.iter().filter(|node| node.patch.id.is_none()).count();
    let total = atlas.nodes.len()
        + atlas.edges.len()
        + atlas.marks.len()
        + atlas.shapes.len()
        + atlas.constraints.len()
        + atlas.variables.len()
        + atlas.relations.len()
        + atlas.explanations.len();
    if total + creating + links.len() > MAX_OBJECTS {
        return Err(format!(
            "this draw would put {} objects on an atlas that holds {MAX_OBJECTS}; split it or remove something first",
            total + creating + links.len()
        ));
    }

    let mut references: Vec<&str> = Vec::new();
    for node in nodes {
        let Some(reference) = node.reference.as_deref() else {
            continue;
        };
        if reference.trim().is_empty() {
            return Err("a node reference cannot be blank".to_string());
        }
        if references.contains(&reference) {
            return Err(format!(
                "reference {reference:?} is used by two nodes in the same draw"
            ));
        }
        references.push(reference);
    }

    // Fail before writing anything if a link points at nothing. Resolution
    // order matters: a batch reference wins over an id, so a caller cannot be
    // silently redirected to an unrelated existing node by picking a name that
    // happens to collide with one.
    for edge in links {
        for endpoint in [&edge.from, &edge.to] {
            if !references.contains(&endpoint.as_str()) && atlas.node(endpoint).is_none() {
                return Err(format!(
                    "link endpoint {endpoint:?} is neither a node on the atlas nor a reference in this draw"
                ));
            }
        }
    }

    // A container named by batch reference has to be written before whatever
    // goes inside it, because the reference only becomes an id at write time.
    // Checked here rather than discovered halfway through, so a caller that
    // ordered the batch wrongly gets told instead of getting half a tree.
    let mut written: Vec<&str> = Vec::new();
    for node in nodes {
        if let Some(parent) = node.patch.parent.as_deref() {
            let parent = parent.trim();
            if !parent.is_empty() && !written.contains(&parent) && atlas.node(parent).is_none() {
                return Err(if references.contains(&parent) {
                    format!(
                        "parent {parent:?} is created later in this draw; list a container before the nodes that go inside it"
                    )
                } else {
                    format!(
                        "parent {parent:?} is neither a node on the atlas nor a reference in this draw"
                    )
                });
            }
        }
        if let Some(reference) = node.reference.as_deref() {
            written.push(reference);
        }
    }

    let mut drawn = Drawn::default();
    for node in nodes {
        // The parent may name something this same batch is creating, so it is
        // resolved against what has already landed rather than against the
        // atlas the batch started from.
        let mut patch = node.patch.clone();
        if let Some(parent) = patch.parent.as_deref() {
            if let Some(id) = drawn.resolve(parent) {
                patch.parent = Some(id.to_string());
            }
        }
        match place_node(scene, &patch, author) {
            Ok(id) => drawn.nodes.push((node.reference.clone(), id)),
            Err(error) => return Err(partial(&drawn, error)),
        }
    }
    for edge in links {
        let from = drawn.resolve(&edge.from).unwrap_or(&edge.from).to_string();
        let to = drawn.resolve(&edge.to).unwrap_or(&edge.to).to_string();
        match link(scene, &from, &to, &edge.label, author) {
            Ok(id) => drawn.edges.push(id),
            Err(error) => return Err(partial(&drawn, error)),
        }
    }
    Ok(drawn)
}

/// Report a mid-batch failure without pretending the earlier writes were
/// undone. They are on the human's screen; saying otherwise would be a lie the
/// next read-back immediately contradicts.
fn partial(drawn: &Drawn, error: AtlasError) -> AtlasError {
    if drawn.nodes.is_empty() && drawn.edges.is_empty() {
        return error;
    }
    format!(
        "{error} — {} node(s) and {} link(s) from this draw are already on the atlas and were not undone; read it back before retrying",
        drawn.nodes.len(),
        drawn.edges.len()
    )
}

/// Validate a requested container for `id`, returning the value to store.
///
/// `id` is empty when the node is being created, which is exactly the case
/// where no cycle is reachable: nothing can be inside a node that does not
/// exist yet. On update the walk upwards from the proposed parent has to miss
/// the node itself, or the tree becomes a ring and the read-back has nothing
/// to print. The refusal names both ends, because "cycle" on its own leaves
/// the caller to work out which of its two writes was the wrong one.
fn containment(atlas: &Atlas, id: &str, label: &str, parent: &str) -> Result<String, AtlasError> {
    let parent = parent.trim();
    if parent.is_empty() {
        return Ok(String::new());
    }
    if parent == id {
        return Err(format!(
            "a node cannot contain itself; {:?} was given as its own parent",
            id
        ));
    }
    let container = atlas
        .node(parent)
        .ok_or_else(|| format!("no node {parent:?} on the atlas to put this one inside"))?;
    if !id.is_empty() && atlas.contains_node(id, parent) {
        let inside = atlas
            .node(id)
            .map(|node| node.label.clone())
            .unwrap_or_else(|| id.to_string());
        return Err(format!(
            "putting \"{inside}\" inside \"{}\" would make it its own container; \"{}\" is already inside \"{inside}\", and containment has to stay a tree",
            container.label, container.label
        ));
    }

    let mut chain = atlas.ancestors(parent);
    chain.reverse();
    chain.push(parent.to_string());
    if id.is_empty() {
        chain.push(String::new());
    } else {
        chain.extend(atlas.deepest_descendant_path(id));
    }
    let attempted_depth = chain.len().saturating_sub(1);
    if attempted_depth > HIERARCHY_MAX_DEPTH {
        let chain = chain
            .iter()
            .map(|node_id| {
                let label = if node_id.is_empty() || node_id == id {
                    label.to_string()
                } else {
                    atlas.label_of(node_id)
                };
                format!("{label:?}")
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(format!(
            "containment exceeds HIERARCHY_MAX_DEPTH {HIERARCHY_MAX_DEPTH}: attempted depth {attempted_depth} through {chain}"
        ));
    }
    Ok(parent.to_string())
}

/// Set the typed diagram role stored on a container node.
///
/// Empty clears the role. The two reserved kinds are named refusals so a
/// caller does not mistake an unbuilt diagram for a typo.
pub fn set_node_diagram_kind(
    scene: &Scene,
    id: &str,
    diagram_kind: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    if atlas.node(id).is_none() {
        return Err(format!("no node {id:?} on the atlas"));
    }
    let diagram_kind = diagram_kind.trim();
    let diagram_kind = if diagram_kind.is_empty() {
        String::new()
    } else {
        let diagram_kind = one_of(diagram_kind, DIAGRAM_KINDS, "diagram kind")?;
        if DIAGRAM_KINDS[2..].contains(&diagram_kind.as_str()) {
            return Err(format!(
                "atlas_diagram kind {diagram_kind:?} is reserved and is not built yet; this board ships hierarchy and state_machine"
            ));
        }
        diagram_kind
    };
    if !diagram_kind.is_empty() && !atlas.is_container(id) {
        return Err(format!(
            "node {id:?} has no children and is not a container; diagram_kind belongs on a container node"
        ));
    }
    if diagram_kind == "state_machine" {
        for edge in atlas.edges.iter().filter(|edge| {
            atlas
                .node(&edge.from)
                .is_some_and(|state| state.parent == id)
                && atlas.node(&edge.to).is_some_and(|state| state.parent == id)
        }) {
            if edge.event.is_empty() {
                return Err(format!(
                    "edge {:?} is inside state machine {:?} and carries no `event`; a transition is named by the event that fires it",
                    edge.id,
                    atlas.label_of(id)
                ));
            }
            if !edge.label.is_empty() {
                return Err(format!(
                    "edge {:?} carries both `label` and `event`; a transition must use `event`",
                    edge.id
                ));
            }
        }
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                (K_DIAGRAM_KIND, PropValue::Str(diagram_kind)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not set diagram kind on {id}: {error}"))
}

/// Set either state role without packing them into one LWW property.
pub fn set_state_roles(
    scene: &Scene,
    id: &str,
    initial: Option<bool>,
    terminal: Option<bool>,
    author: &Author,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    state_machine_parent(&atlas, id)?;
    let mut props = Vec::new();
    if let Some(initial) = initial {
        props.push((
            K_STATE_INITIAL,
            if initial {
                PropValue::Bool(true)
            } else {
                PropValue::Str(String::new())
            },
        ));
    }
    if let Some(terminal) = terminal {
        props.push((
            K_STATE_TERMINAL,
            if terminal {
                PropValue::Bool(true)
            } else {
                PropValue::Str(String::new())
            },
        ));
    }
    if props.is_empty() {
        return Ok(());
    }
    props.push(("touched_by", PropValue::Str(author.as_str().to_string())));
    scene
        .set_props(&ObjectId::from(id.to_string()), &props)
        .map_err(|error| format!("could not set state roles on {id}: {error}"))
}

fn state_machine_parent<'a>(atlas: &'a Atlas, id: &str) -> Result<&'a Node, AtlasError> {
    if atlas.node(id).is_none() {
        return Err(format!("no node {id:?} on the atlas"));
    }
    atlas.state_machine_of(id).ok_or_else(|| {
        format!("node {id:?} is not a direct state inside a state_machine container")
    })
}

/// Create or update a node. Returns the object id.
pub fn place_node(
    scene: &mut Scene,
    patch: &NodePatch,
    author: &Author,
) -> Result<String, AtlasError> {
    let mut props: Vec<(&str, PropValue)> = Vec::new();
    if let Some(label) = &patch.label {
        props.push(("label", PropValue::Str(text(label, MAX_LABEL, "label")?)));
    }
    if let Some(path) = &patch.path {
        props.push(("path", PropValue::Str(source_path(path)?)));
    }
    if let Some(lines) = &patch.lines {
        props.push(("lines", PropValue::Str(line_range(lines)?)));
    }
    if let Some(note) = &patch.note {
        props.push(("note", PropValue::Str(text(note, MAX_NOTE, "note")?)));
    }
    if let Some(tone) = &patch.tone {
        props.push(("tone", PropValue::Str(one_of(tone, TONES, "tone")?)));
    }
    if let Some(status) = &patch.status {
        props.push((
            "status",
            PropValue::Str(one_of(status, STATUSES, "status")?),
        ));
    }
    if let Some(x) = patch.x {
        props.push(("x", PropValue::Num(coord(x, "x")?)));
    }
    if let Some(y) = patch.y {
        props.push(("y", PropValue::Num(coord(y, "y")?)));
    }
    if let Some(w) = patch.w {
        props.push(("w", PropValue::Num(w.clamp(120.0, 640.0))));
    }
    // The style register. Each word is checked against its own closed list, so
    // a value from the wrong vocabulary is a refusal the caller can fix rather
    // than a card that silently renders as unstyled. Colour is the one that
    // also accepts empty, because "this belongs to no family" is a real thing
    // to say and there is no word for it.
    if let Some(color) = &patch.color {
        let color = color.trim();
        props.push((
            K_COLOR,
            PropValue::Str(if color.is_empty() {
                String::new()
            } else {
                one_of(color, INKS, "color")?
            }),
        ));
    }
    if let Some(emphasis) = &patch.emphasis {
        props.push((
            K_EMPHASIS,
            PropValue::Str(one_of(emphasis, EMPHASES, "emphasis")?),
        ));
    }
    if let Some(size) = &patch.size {
        props.push((K_SIZE, PropValue::Str(one_of(size, SIZES, "size")?)));
    }
    if let Some(kind) = &patch.kind {
        props.push((
            K_NODE_KIND,
            PropValue::Str(one_of(kind, NODE_KINDS, "kind")?),
        ));
    }
    props.push(("touched_by", PropValue::Str(author.as_str().to_string())));

    match &patch.id {
        Some(id) => {
            let atlas = read(scene)?;
            let node = atlas
                .node(id)
                .ok_or_else(|| format!("no node {id:?} on the atlas"))?;
            if patch.status.as_deref() == Some("agreed") {
                claims::refuse_agreed_under_challenge(&atlas, id)?;
            }
            if let Some(parent) = &patch.parent {
                let label = patch.label.as_deref().unwrap_or(&node.label);
                props.push((
                    K_PARENT,
                    PropValue::Str(containment(&atlas, id, label, parent)?),
                ));
            }
            let id = ObjectId::from(node.id.clone());
            scene
                .set_props(&id, &props)
                .map_err(|error| format!("could not update node: {error}"))?;
            Ok(id.into_string())
        }
        None => {
            let atlas = read(scene)?;
            capacity(&atlas)?;
            if patch.label.is_none() {
                return Err("a new node needs a label".to_string());
            }
            if let Some(parent) = &patch.parent {
                props.push((
                    K_PARENT,
                    PropValue::Str(containment(
                        &atlas,
                        "",
                        patch.label.as_deref().unwrap_or("<new node>"),
                        parent,
                    )?),
                ));
            }
            if patch.x.is_none() || patch.y.is_none() {
                let (x, y) = free_slot(&atlas);
                if patch.x.is_none() {
                    props.push(("x", PropValue::Num(x)));
                }
                if patch.y.is_none() {
                    props.push(("y", PropValue::Num(y)));
                }
            }
            if patch.tone.is_none() {
                props.push(("tone", PropValue::Str("concept".to_string())));
            }
            if patch.status.is_none() {
                props.push(("status", PropValue::Str("open".to_string())));
            }
            if patch.w.is_none() {
                props.push(("w", PropValue::Num(DEFAULT_WIDTH)));
            }
            let id = scene
                .create_object_with_props(KIND_NODE, author.clone(), &props)
                .map_err(|error| format!("could not create node: {error}"))?;
            Ok(id.into_string())
        }
    }
}

/// Draw a relationship between two existing nodes.
pub fn link(
    scene: &mut Scene,
    from: &str,
    to: &str,
    label: &str,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    capacity(&atlas)?;
    if from == to {
        return Err("a link needs two different nodes".to_string());
    }
    for id in [from, to] {
        if atlas.node(id).is_none() {
            return Err(format!("no node {id:?} on the atlas"));
        }
    }
    if let (Some(from_machine), Some(to_machine)) =
        (atlas.state_machine_of(from), atlas.state_machine_of(to))
    {
        if from_machine.id == to_machine.id {
            return Err(format!(
                "a link inside state machine {:?} carries no `event`; a transition is named by the event that fires it",
                from_machine.label
            ));
        }
    }
    if let Some(existing) = atlas
        .edges
        .iter()
        .find(|edge| !edge.is_transition() && edge.from == from && edge.to == to)
    {
        let id = ObjectId::from(existing.id.clone());
        scene
            .set_props(
                &id,
                &[
                    ("label", PropValue::Str(text(label, MAX_LABEL, "label")?)),
                    ("touched_by", PropValue::Str(author.as_str().to_string())),
                ],
            )
            .map_err(|error| format!("could not update link: {error}"))?;
        return Ok(id.into_string());
    }
    let id = scene
        .create_object_with_props(
            KIND_EDGE,
            author.clone(),
            &[
                ("from", PropValue::Str(from.to_string())),
                ("to", PropValue::Str(to.to_string())),
                ("label", PropValue::Str(text(label, MAX_LABEL, "label")?)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not create link: {error}"))?;
    Ok(id.into_string())
}

/// Create one state-machine transition in edge creation order.
pub fn transition(
    scene: &mut Scene,
    from: &str,
    to: &str,
    event: &str,
    guard: Option<&str>,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    capacity(&atlas)?;
    let from_machine = state_machine_parent(&atlas, from)?;
    let to_machine = state_machine_parent(&atlas, to)?;
    if from_machine.id != to_machine.id {
        return Err(format!(
            "a new transition must stay inside one state machine; {:?} is in {:?} and {:?} is in {:?}",
            atlas.label_of(from),
            from_machine.label,
            atlas.label_of(to),
            to_machine.label
        ));
    }
    let event = text(event, MAX_LABEL, "transition event")?;
    if event.is_empty() {
        return Err(format!(
            "a transition inside state machine {:?} carries no `event`; a transition is named by the event that fires it",
            from_machine.label
        ));
    }
    let guard = text(guard.unwrap_or(""), MAX_TEXT, "transition guard")?;
    let order = atlas
        .edges
        .iter()
        .filter(|edge| edge.is_transition())
        .map(|edge| edge.transition_order)
        .max()
        .unwrap_or_default()
        .checked_add(1)
        .ok_or_else(|| "transition creation order is exhausted".to_string())?;
    let id = scene
        .create_object_with_props(
            KIND_EDGE,
            author.clone(),
            &[
                ("from", PropValue::Str(from.to_string())),
                ("to", PropValue::Str(to.to_string())),
                ("label", PropValue::Str(String::new())),
                (K_TRANSITION_EVENT, PropValue::Str(event)),
                (K_TRANSITION_GUARD, PropValue::Str(guard)),
                (K_TRANSITION_ORDER, PropValue::Num(order as f64)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not create transition: {error}"))?;
    Ok(id.into_string())
}

/// Update a transition while retaining its id and creation order.
pub fn update_transition(
    scene: &Scene,
    id: &str,
    patch: &TransitionPatch,
    author: &Author,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    let edge = atlas
        .edges
        .iter()
        .find(|edge| edge.id == id)
        .ok_or_else(|| format!("no transition {id:?} on the atlas"))?;
    if !edge.is_transition() {
        return Err(format!("edge {id:?} is an ordinary link, not a transition"));
    }
    if !edge.label.is_empty() {
        return Err(format!(
            "edge {id:?} carries both `label` and `event`; a transition must use `event`"
        ));
    }

    let from = patch.from.as_deref().unwrap_or(&edge.from);
    let to = patch.to.as_deref().unwrap_or(&edge.to);
    state_machine_parent(&atlas, from)?;
    state_machine_parent(&atlas, to)?;

    let mut props = Vec::new();
    if patch.from.is_some() {
        props.push(("from", PropValue::Str(from.to_string())));
    }
    if patch.to.is_some() {
        props.push(("to", PropValue::Str(to.to_string())));
    }
    if let Some(event) = patch.event.as_deref() {
        let event = text(event, MAX_LABEL, "transition event")?;
        if event.is_empty() {
            return Err("a transition event cannot be empty".to_string());
        }
        props.push((K_TRANSITION_EVENT, PropValue::Str(event)));
    }
    if let Some(guard) = patch.guard.as_deref() {
        props.push((
            K_TRANSITION_GUARD,
            PropValue::Str(text(guard, MAX_TEXT, "transition guard")?),
        ));
    }
    if props.is_empty() {
        return Ok(());
    }
    props.push(("touched_by", PropValue::Str(author.as_str().to_string())));
    scene
        .set_props(&ObjectId::from(id.to_string()), &props)
        .map_err(|error| format!("could not update transition {id}: {error}"))
}

/// Create one exact structural relation between existing non-constraint
/// objects. Every validation finishes before the CRDT write begins.
#[allow(clippy::too_many_arguments)]
pub fn create_constraint(
    scene: &mut Scene,
    op: &str,
    members: &[String],
    axis: Option<&str>,
    label_text: Option<&str>,
    salience: Option<&str>,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    let op = one_of(op, CONSTRAINT_OPS, "constraint op")?;
    let valid_arity = match op.as_str() {
        "group" | "sequence" => members.len() >= 2,
        "attaches" => members.len() == 2,
        "voids" | "labels" => members.len() == 1,
        _ => false,
    };
    if !valid_arity {
        let required = match op.as_str() {
            "group" | "sequence" => "at least two members",
            "attaches" => "exactly two members",
            _ => "exactly one member",
        };
        return Err(format!(
            "constraint op {op:?} needs {required}; got {}",
            members.len()
        ));
    }

    let unique = members
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    if unique.len() != members.len() {
        return Err("a constraint cannot contain self or duplicate members".to_string());
    }
    for member in members {
        if atlas.constraint(member).is_some() {
            return Err(format!(
                "constraint member {member:?} names another constraint; v0 members must name non-constraint objects"
            ));
        }
        if !atlas.object_exists(member) {
            return Err(format!(
                "constraint member {member:?} does not resolve to an atlas object"
            ));
        }
    }

    let axis = if op == "sequence" {
        Some(one_of(
            axis.unwrap_or("x"),
            CONSTRAINT_AXES,
            "constraint axis",
        )?)
    } else {
        None
    };
    let label_text = if op == "labels" {
        Some(text(label_text.unwrap_or(""), MAX_TEXT, "constraint text")?)
    } else {
        None
    };
    let salience = one_of(
        salience.unwrap_or("back"),
        CONSTRAINT_SALIENCES,
        "constraint salience",
    )?;
    capacity(&atlas)?;

    let id = scene
        .create_object_with_props(
            KIND_CONSTRAINT,
            author.clone(),
            &[
                ("op", PropValue::Str(op)),
                ("members", PropValue::Str(members.join(","))),
                ("axis", PropValue::Str(axis.unwrap_or_default())),
                ("text", PropValue::Str(label_text.unwrap_or_default())),
                ("salience", PropValue::Str(salience)),
            ],
        )
        .map_err(|error| format!("could not create constraint: {error}"))?;
    Ok(id.into_string())
}

/// Flag a node or drawing shape: a question, an objection, or "this matters".
pub fn mark(
    scene: &mut Scene,
    target: &str,
    glyph: &str,
    text_body: &str,
    author: &Author,
) -> Result<String, AtlasError> {
    if !matches!(author, Author::Human) {
        return Err("only the human can create atlas marks".to_string());
    }
    let atlas = read(scene)?;
    capacity(&atlas)?;
    if atlas.node(target).is_none()
        && atlas.shape(target).is_none()
        && atlas.constraint(target).is_none()
        && atlas.claim(target).is_none()
    {
        return Err(format!(
            "no node, shape, constraint, or claim {target:?} on the atlas"
        ));
    }
    let id = scene
        .create_object_with_props(
            KIND_MARK,
            author.clone(),
            &[
                ("target", PropValue::Str(target.to_string())),
                ("glyph", PropValue::Str(one_of(glyph, GLYPHS, "glyph")?)),
                (
                    "text",
                    PropValue::Str(text(text_body, MAX_TEXT, "mark text")?),
                ),
                ("answer", PropValue::Str(String::new())),
                ("context_ids", PropValue::Str(atlas.context.heads.join(","))),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not create mark: {error}"))?;
    Ok(id.into_string())
}

/// Answer a mark in place, so the question and its resolution stay attached to
/// the thing they are about instead of scrolling away in a transcript.
pub fn answer_mark(
    scene: &Scene,
    id: &str,
    answer: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    if !atlas.marks.iter().any(|mark| mark.id == id) {
        return Err(format!("no mark {id:?} on the atlas"));
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("answer", PropValue::Str(text(answer, MAX_TEXT, "answer")?)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not answer mark: {error}"))
}

/// Remove one object, plus any edges/marks that pointed at it.
///
/// Shapes bound to a removed card or drawing shape are handled by halves,
/// deliberately. An arrow that was only ever two references has no geometry
/// once one of them is gone, so it goes too. A stroke somebody actually drew
/// keeps its points and merely loses the binding: deleting its target is not
/// consent to delete the drawing somebody put next to it.
pub fn remove(scene: &Scene, id: &str) -> Result<usize, AtlasError> {
    let object_id = ObjectId::from(id.to_string());
    if scene
        .get_prop(&object_id, "kind")
        .map_err(|e| e.to_string())?
        == Some(PropValue::Str(KIND_EXPLANATION_FLOW.to_string()))
    {
        scene.delete_object(&object_id).map_err(|e| e.to_string())?;
        return Ok(1);
    }
    let atlas = read(scene)?;
    let mut doomed = vec![id.to_string()];
    let is_node = atlas.node(id).is_some();
    let is_shape = atlas.shape(id).is_some();
    let is_constraint = atlas.constraint(id).is_some();
    let is_claim = atlas.claim(id).is_some();
    if is_node {
        doomed.extend(
            atlas
                .edges
                .iter()
                .filter(|edge| edge.from == id || edge.to == id)
                .map(|edge| edge.id.clone()),
        );
        // A node's claims are about it and nothing else, so they go with it,
        // and so does every mark the human put on one of those claims.
        for claim in atlas.claims_of(id) {
            doomed.push(claim.id.clone());
            doomed.extend(
                atlas
                    .marks
                    .iter()
                    .filter(|mark| mark.target == claim.id)
                    .map(|mark| mark.id.clone()),
            );
        }
    }
    if is_node || is_shape || is_constraint || is_claim {
        doomed.extend(
            atlas
                .marks
                .iter()
                .filter(|mark| mark.target == id)
                .map(|mark| mark.id.clone()),
        );
    } else if !atlas.edges.iter().any(|edge| edge.id == id)
        && !atlas.marks.iter().any(|mark| mark.id == id)
        && !is_constraint
    {
        return Err(format!("no object {id:?} on the atlas"));
    }

    // Deleting a container must not delete what was inside it. The children
    // move up one level, to the grandparent when there is one and to the top
    // level otherwise, and the write happens before the delete so the pointer
    // never dangles even for a moment on a replica watching this transaction.
    // The read-back reports the move as its own change, from the same digest
    // diff that reports every other reparent.
    if is_node {
        let grandparent = atlas
            .node(id)
            .map(|node| node.parent.clone())
            .unwrap_or_default();
        for child in atlas.children_of(id) {
            if doomed.contains(&child.id) {
                continue;
            }
            scene
                .set_props(
                    &ObjectId::from(child.id.clone()),
                    &[(K_PARENT, PropValue::Str(grandparent.clone()))],
                )
                .map_err(|error| format!("could not move {} out of {id}: {error}", child.id))?;
        }
    }

    // Removing any segment promotes its direct children to the removed
    // segment's parent. Their world geometry and deeper subtrees stay intact.
    if let Some(segment) = atlas.shape(id).filter(|shape| shape.form == "segment") {
        let parent_id = segment.segment_parent_id.clone();
        for child in atlas
            .shapes
            .iter()
            .filter(|shape| shape.segment_parent_id == id)
        {
            scene
                .set_props(
                    &ObjectId::from(child.id.clone()),
                    &[("segment_parent_id", PropValue::Str(parent_id.clone()))],
                )
                .map_err(|error| format!("could not move {} out of {id}: {error}", child.id))?;
        }
    }

    if atlas.node(id).is_some() || atlas.binding_bounds(id).is_some() {
        for shape in &atlas.shapes {
            if shape.id == id {
                continue;
            }
            if shape.from != id && shape.to != id {
                continue;
            }
            if shape.points.is_empty() {
                doomed.push(shape.id.clone());
                continue;
            }
            let mut unbind: Vec<(&str, PropValue)> = Vec::new();
            if shape.from == id {
                unbind.push(("from", PropValue::Str(String::new())));
            }
            if shape.to == id {
                unbind.push(("to", PropValue::Str(String::new())));
            }
            scene
                .set_props(&ObjectId::from(shape.id.clone()), &unbind)
                .map_err(|error| format!("could not unbind {}: {error}", shape.id))?;
        }
    }
    for target in &doomed {
        scene
            .delete_object(&ObjectId::from(target.clone()))
            .map_err(|error| format!("could not remove {target}: {error}"))?;
    }
    Ok(doomed.len())
}

/// Promote one drawn shape into a node: the same words in the same place, but
/// in the register the agent can reason about. Returns the new node's id.
///
/// The register split is the failure this repairs. A picture authored as ink
/// reads back as decoration, so the fastest thing to draw becomes the one
/// thing on the page with no standing. Lifting is meaning-preserving by
/// construction: the node takes the shape's title and box, and every mark,
/// arrow binding, and constraint that pointed at the shape is retargeted to
/// the node rather than orphaned. The lifted shape, and a text consumed as
/// its title, leave the drawn layer: this is a move between registers, not a
/// copy, and the read-back's delta reports it as exactly that.
pub fn lift_shape(scene: &mut Scene, id: &str, author: &Author) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    let Some(shape) = atlas.shape(id) else {
        return Err(format!("no shape {id:?} on the atlas"));
    };
    if !is_matter(shape) {
        return Err(
            "an arrow relates things rather than being one; lift what it points at, or link two cards instead"
                .to_string(),
        );
    }

    // The title: the shape's own words, or the one text written inside its
    // region. Two texts inside is a genuine ambiguity, and picking one for
    // the human would invent an intent nobody expressed.
    let mut consumed: Option<String> = None;
    let title_source = if !shape.label.is_empty() {
        shape.label.clone()
    } else if shape.is_closed() {
        let texts: Vec<&Shape> = atlas
            .shapes
            .iter()
            .filter(|other| other.form == "text" && other.id != shape.id)
            .filter(|other| shape.covers_bounds(other.bounds()) == Coverage::Inside)
            .collect();
        match texts.len() {
            1 => {
                consumed = Some(texts[0].id.clone());
                texts[0].label.clone()
            }
            0 => return Err(
                "this shape has no words to become a claim; give it a label first, or lift a text"
                    .to_string(),
            ),
            count => {
                return Err(format!(
                    "this {} holds {count} texts and a claim needs one title; lift the text you mean, or label the shape itself",
                    shape.form
                ));
            }
        }
    } else {
        return Err(
            "this shape has no words to become a claim; give it a label first, or lift a text"
                .to_string(),
        );
    };

    // Both registers share MAX_LABEL, so words that fit on a shape fit on a
    // card: nothing the human wrote is dropped on the way between registers.
    let (left, top, right, _) = shape.bounds();
    let node_id = place_node(
        scene,
        &NodePatch {
            label: Some(title_source),
            x: Some(left),
            y: Some(top),
            w: Some(right - left),
            ..NodePatch::default()
        },
        author,
    )?;

    // Everything that pointed at the drawing now points at the claim.
    let mut lifted = vec![shape.id.clone()];
    lifted.extend(consumed.clone());
    for mark in &atlas.marks {
        if !lifted.contains(&mark.target) {
            continue;
        }
        scene
            .set_props(
                &ObjectId::from(mark.id.clone()),
                &[
                    ("target", PropValue::Str(node_id.clone())),
                    ("touched_by", PropValue::Str(author.as_str().to_string())),
                ],
            )
            .map_err(|error| format!("could not retarget mark {}: {error}", mark.id))?;
    }
    for other in &atlas.shapes {
        if lifted.contains(&other.id) {
            continue;
        }
        let mut rebind: Vec<(&str, PropValue)> = Vec::new();
        if lifted.contains(&other.from) {
            rebind.push(("from", PropValue::Str(node_id.clone())));
        }
        if lifted.contains(&other.to) {
            rebind.push(("to", PropValue::Str(node_id.clone())));
        }
        if !rebind.is_empty() {
            scene
                .set_props(&ObjectId::from(other.id.clone()), &rebind)
                .map_err(|error| format!("could not rebind {}: {error}", other.id))?;
        }
    }
    for constraint in &atlas.constraints {
        if !constraint
            .members
            .iter()
            .any(|member| lifted.contains(member))
        {
            continue;
        }
        let mut members: Vec<String> = Vec::new();
        for member in &constraint.members {
            let target = if lifted.contains(member) {
                node_id.clone()
            } else {
                member.clone()
            };
            if !members.contains(&target) {
                members.push(target);
            }
        }
        scene
            .set_props(
                &ObjectId::from(constraint.id.clone()),
                &[("members", PropValue::Str(members.join(",")))],
            )
            .map_err(|error| format!("could not retarget constraint {}: {error}", constraint.id))?;
    }
    for gone in &lifted {
        scene
            .delete_object(&ObjectId::from(gone.clone()))
            .map_err(|error| format!("could not retire the lifted {gone}: {error}"))?;
    }
    Ok(node_id)
}

// ── drawing ───────────────────────────────────────────────────────────────
//
// Nodes say things; shapes point at them. The two halves of this section are
// deliberately asymmetric: writing a shape is small, and reading one back is
// where the work is. A drawing that only the person looking at the screen can
// interpret would be a private channel on a surface whose entire premise is
// that neither side gets one.

/// CRDT key holding the encoded point list.
const K_POINTS: &str = "points";

/// How close an ink stroke's ends must come, relative to its own size, before
/// it counts as encircling something rather than passing by.
const CLOSURE_TOLERANCE: f64 = 0.34;

/// A point list survives in the CRDT as ONE string property.
///
/// This is a deliberate exception to the flat-scalar rule, not an oversight.
/// Every other property is per-key last-write-wins so a human dragging `x` and
/// an agent writing `note` both survive; points get no such treatment, because
/// there is no such conflict to survive. Nobody edits the middle of somebody
/// else's scribble while they are drawing it — a stroke is authored once, as a
/// unit, by one hand. Encoding it per-point would buy merge semantics for an
/// event that does not happen and cost 200 CRDT entries per squiggle.
fn parse_points(raw: &str) -> Vec<(f64, f64)> {
    raw.split_whitespace()
        .filter_map(|pair| {
            let (x, y) = pair.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .collect()
}

/// Group ids survive as ONE whitespace-joined string property, for the same
/// reason a point list does: a shape's group membership is authored as a unit.
fn parse_groups(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(str::to_string).collect()
}

fn parse_json_string_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn parse_json_numbers(raw: &str) -> Vec<f64> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn whole_number(value: f64) -> u32 {
    if value.is_finite() && value >= 0.0 && value <= f64::from(u32::MAX) {
        value.round() as u32
    } else {
        0
    }
}

fn format_groups(groups: &[String]) -> String {
    groups.join(" ")
}

fn format_points(points: &[(f64, f64)]) -> String {
    points
        .iter()
        .map(|(x, y)| format!("{x:.1},{y:.1}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Oracle semantics for one bounded raster import. The host mints the object
/// id and owns these fields before any browser inference runs.
#[derive(Debug, Clone, Deserialize)]
pub struct SegmentProposal {
    pub label: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub ocr: Vec<String>,
    pub prompt_box: [f64; 4],
    pub x: f64,
    pub y: f64,
    pub w: f64,
    #[serde(default = "default_segment_animation")]
    pub animation: String,
    /// Optional segment this human-selected mask is directly attached to.
    #[serde(default)]
    pub parent_id: String,
}

/// One expressive, persisted 2D motion program owned by a segment.
///
/// An owner may target itself and any materialized descendant in its bounded
/// part hierarchy. The program is structured data rather than a preset name,
/// so an agent can author any bounded translation, rotation, scale, and
/// opacity path without exposing a timeline editor to the human.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SegmentMotionProgram {
    pub schema: String,
    pub label: String,
    /// How descendant masks combine with the owner's source image while they
    /// move. `cutout` articulates physical parts. `overlay` preserves a clean
    /// source image beneath instructional callouts and infographic regions.
    #[serde(default = "default_segment_child_compositing")]
    pub child_compositing: String,
    pub tracks: Vec<SegmentMotionTrack>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SegmentMotionTrack {
    pub label: String,
    pub target_ids: Vec<String>,
    pub duration_ms: u32,
    #[serde(default)]
    pub delay_ms: u32,
    #[serde(default)]
    pub stagger_ms: u32,
    #[serde(default)]
    pub r#loop: bool,
    #[serde(default)]
    pub alternate: bool,
    #[serde(default)]
    pub curve: Option<[f64; 4]>,
    #[serde(default)]
    pub origin: Option<[f64; 2]>,
    pub keyframes: Vec<SegmentMotionKeyframe>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SegmentMotionKeyframe {
    pub at: f64,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub rotate: f64,
    #[serde(default = "one")]
    pub scale_x: f64,
    #[serde(default = "one")]
    pub scale_y: f64,
    #[serde(default = "one")]
    pub opacity: f64,
}

fn one() -> f64 {
    1.0
}

fn default_segment_child_compositing() -> String {
    "cutout".to_string()
}

fn segment_depth(atlas: &Atlas, id: &str) -> Result<usize, AtlasError> {
    let mut depth = 0usize;
    let mut current = id;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current.to_string()) {
            return Err(format!("segment hierarchy contains a cycle at {current:?}"));
        }
        let segment = atlas
            .shape(current)
            .filter(|shape| shape.form == "segment")
            .ok_or_else(|| format!("no segment {current:?} on the atlas"))?;
        if segment.segment_parent_id.is_empty() {
            return Ok(depth);
        }
        depth += 1;
        current = &segment.segment_parent_id;
    }
}

fn segment_root_id<'a>(atlas: &'a Atlas, id: &'a str) -> Result<&'a str, AtlasError> {
    let mut current = id;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(current.to_string()) {
            return Err(format!("segment hierarchy contains a cycle at {current:?}"));
        }
        let segment = atlas
            .shape(current)
            .filter(|shape| shape.form == "segment")
            .ok_or_else(|| format!("no segment {current:?} on the atlas"))?;
        if segment.segment_parent_id.is_empty() {
            return Ok(current);
        }
        current = &segment.segment_parent_id;
    }
}

fn segment_is_descendant(atlas: &Atlas, candidate_id: &str, ancestor_id: &str) -> bool {
    if candidate_id == ancestor_id {
        return false;
    }
    let mut current = candidate_id;
    let mut visited = std::collections::BTreeSet::new();
    while visited.insert(current.to_string()) {
        let Some(segment) = atlas.shape(current).filter(|shape| shape.form == "segment") else {
            return false;
        };
        if segment.segment_parent_id == ancestor_id {
            return true;
        }
        if segment.segment_parent_id.is_empty() {
            return false;
        }
        current = &segment.segment_parent_id;
    }
    false
}

fn segment_subtree_depth(atlas: &Atlas, id: &str) -> usize {
    atlas
        .shapes
        .iter()
        .filter(|shape| shape.form == "segment" && shape.segment_parent_id == id)
        .map(|child| 1 + segment_subtree_depth(atlas, &child.id))
        .max()
        .unwrap_or(0)
}

/// Participant-claimed provenance for semantics selected by a real
/// multimodal agent outside the Atlas host. The host can bind this receipt to
/// the pinned source and exact semantic output, but it cannot independently
/// attest to an external model invocation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SegmentSemanticReceipt {
    pub schema: String,
    pub provider: String,
    pub model: String,
    pub request_id: String,
    pub execution_location: String,
    pub trust: String,
    pub source_sha256: String,
    pub source_size: [u32; 2],
    pub input_mime_type: String,
    pub output_sha256: String,
    pub fallback_used: bool,
    pub mock_used: bool,
}

/// One model-authored proposal plus the receipt that binds it to the source.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentSegmentProposal {
    #[serde(flatten)]
    pub proposal: SegmentProposal,
    pub semantic_receipt: SegmentSemanticReceipt,
}

#[derive(Serialize)]
struct SegmentSemanticOutput<'a> {
    label: &'a str,
    tags: &'a [String],
    ocr: &'a [String],
    prompt_box: [f64; 4],
}

fn default_segment_animation() -> String {
    "none".to_string()
}

/// Browser-derived appearance for one exact proposed generation.
#[derive(Debug, Clone, Deserialize)]
pub struct SegmentMaterialization {
    pub generation: u32,
    pub model_id: String,
    pub encoder_sha256: String,
    pub decoder_sha256: String,
    pub predicted_iou: f64,
    pub mask_encoding: String,
    pub mask_width: u32,
    pub mask_height: u32,
    pub mask_runs: String,
    pub mask_source_box: [f64; 4],
    #[serde(default)]
    pub parts: Vec<SegmentPartMaterialization>,
    pub receipt: JsonValue,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SegmentPartMaterialization {
    pub role: String,
    pub prompt_box: [f64; 4],
    pub predicted_iou: f64,
    pub mask_encoding: String,
    pub mask_width: u32,
    pub mask_height: u32,
    pub mask_runs: String,
    pub mask_source_box: [f64; 4],
    pub receipt: JsonValue,
}

fn validate_source_box_for(
    values: &[f64; 4],
    name: &str,
    source: &SegmentSourceContract,
) -> Result<(), AtlasError> {
    if !values.iter().all(|value| value.is_finite()) {
        return Err(format!(
            "{name} must contain four finite source-pixel numbers"
        ));
    }
    let [left, top, right, bottom] = *values;
    if left < 0.0
        || top < 0.0
        || right <= left
        || bottom <= top
        || right > f64::from(source.width)
        || bottom > f64::from(source.height)
    {
        return Err(format!(
            "{name} must be [left, top, right, bottom] inside {}x{} source pixels",
            source.width, source.height
        ));
    }
    Ok(())
}

fn default_segment_source() -> Result<&'static SegmentSourceContract, AtlasError> {
    segment_source_by_id(DEFAULT_SEGMENT_SOURCE_ID).ok_or_else(|| {
        format!(
            "generated segmentation catalog is missing default source {DEFAULT_SEGMENT_SOURCE_ID:?}"
        )
    })
}

fn validate_source_box(values: &[f64; 4], name: &str) -> Result<(), AtlasError> {
    validate_source_box_for(values, name, default_segment_source()?)
}

fn receipt_segment_source(
    receipt: &JsonValue,
) -> Result<&'static SegmentSourceContract, AtlasError> {
    let receipt = receipt
        .as_object()
        .ok_or_else(|| "segment receipt must be a JSON object".to_string())?;
    let sha256 = receipt
        .get("sourceSha256")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "segment receipt sourceSha256 must be a string".to_string())?;
    let size = receipt
        .get("sourceSize")
        .and_then(JsonValue::as_array)
        .filter(|size| size.len() == 2)
        .ok_or_else(|| "segment receipt sourceSize must contain two integers".to_string())?;
    let width = size[0]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "segment receipt source width must fit u32".to_string())?;
    let height = size[1]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "segment receipt source height must fit u32".to_string())?;
    segment_source_by_identity(sha256, width, height)
        .ok_or_else(|| "segment receipt does not identify a generated source".to_string())
}

fn validate_semantic_list(values: &[String], name: &str) -> Result<(), AtlasError> {
    if values.len() > 16 {
        return Err(format!("{name} may contain at most 16 values"));
    }
    for value in values {
        text(value, 120, name)?;
    }
    Ok(())
}

/// Digest the exact semantic fields Atlas will store, excluding placement and
/// animation because those are scene composition rather than image meaning.
pub fn segment_semantic_output_sha256(proposal: &SegmentProposal) -> Result<String, AtlasError> {
    let bytes = serde_json::to_vec(&SegmentSemanticOutput {
        label: proposal.label.trim(),
        tags: &proposal.tags,
        ocr: &proposal.ocr,
        prompt_box: proposal.prompt_box,
    })
    .map_err(|error| format!("could not encode segment semantic output: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn required_receipt_text(value: &str, limit: usize, field: &str) -> Result<String, AtlasError> {
    let value = text(value, limit, field)?;
    if value.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(value)
    }
}

fn validate_segment_semantic_receipt(
    proposal: &SegmentProposal,
    receipt: &SegmentSemanticReceipt,
) -> Result<(), AtlasError> {
    if receipt.schema != SEGMENT_SEMANTIC_RECEIPT_SCHEMA {
        return Err(format!(
            "semantic receipt schema must be {SEGMENT_SEMANTIC_RECEIPT_SCHEMA:?}"
        ));
    }
    required_receipt_text(&receipt.provider, 120, "semantic receipt provider")?;
    required_receipt_text(&receipt.model, 120, "semantic receipt model")?;
    required_receipt_text(&receipt.request_id, 200, "semantic receipt request_id")?;
    if receipt.execution_location != "external-multimodal-agent" {
        return Err(
            "semantic receipt execution_location must be external-multimodal-agent".to_string(),
        );
    }
    if receipt.trust != "participant-claimed" {
        return Err("semantic receipt trust must be participant-claimed".to_string());
    }
    if receipt.source_sha256 != SEGMENT_SOURCE_SHA256
        || receipt.source_size != [SEGMENT_SOURCE_WIDTH, SEGMENT_SOURCE_HEIGHT]
        || receipt.input_mime_type != "image/png"
    {
        return Err("semantic receipt does not identify the pinned source image".to_string());
    }
    if receipt.output_sha256 != segment_semantic_output_sha256(proposal)? {
        return Err("semantic receipt output digest does not match the proposal".to_string());
    }
    if receipt.fallback_used || receipt.mock_used {
        return Err("semantic receipt reports mock or fallback execution".to_string());
    }
    Ok(())
}

/// Mint one stable semantic object before MobileSAM runs.
pub fn propose_segment(
    scene: &mut Scene,
    proposal: &SegmentProposal,
    author: &Author,
) -> Result<String, AtlasError> {
    propose_segment_with_semantics(
        scene,
        proposal,
        "oracle",
        JsonValue::Null,
        default_segment_source()?,
        author,
    )
}

/// Accept one human-prompted mask as one Atlas object.
///
/// Atlas binds its newly minted identity into the browser Worker receipt. If
/// any materialization check fails, the proposal is removed before the error
/// returns so a rejected mask cannot leave a partial object in the document.
pub fn accept_human_segment(
    scene: &mut Scene,
    proposal: &SegmentProposal,
    result: &mut SegmentMaterialization,
    author: &Author,
) -> Result<String, AtlasError> {
    if !matches!(author, Author::Human) {
        return Err("a human mask acceptance requires human authorship".to_string());
    }
    if !result.receipt.is_object() {
        return Err("segment receipt must be a JSON object".to_string());
    }
    let source = receipt_segment_source(&result.receipt)?;
    result.generation = 1;
    let receipt = result
        .receipt
        .as_object_mut()
        .ok_or_else(|| "segment receipt must be a JSON object".to_string())?;
    receipt.insert("generation".to_string(), JsonValue::from(1));
    receipt.insert("occlusionApplied".to_string(), JsonValue::Bool(false));
    receipt.insert("occlusionBox".to_string(), JsonValue::Null);

    let id = propose_segment_with_semantics(
        scene,
        proposal,
        "human-prompt",
        JsonValue::Null,
        source,
        author,
    )?;
    let receipt = result
        .receipt
        .as_object_mut()
        .ok_or_else(|| "segment receipt must be a JSON object".to_string())?;
    receipt.insert("stableObjectId".to_string(), JsonValue::String(id.clone()));
    if let Err(error) = materialize_segment(scene, &id, result) {
        return match remove(scene, &id) {
            Ok(_) => Err(error),
            Err(cleanup) => Err(format!(
                "{error}; rejected segment {id} could not be removed: {cleanup}"
            )),
        };
    }
    Ok(id)
}

/// Mint a stable object from semantics authored by an external multimodal
/// agent. Receipt trust remains participant-claimed, while source identity,
/// semantic output, and no-mock/no-fallback fields are host-validated.
pub fn propose_agent_segment(
    scene: &mut Scene,
    proposal: &SegmentProposal,
    receipt: &SegmentSemanticReceipt,
    author: &Author,
) -> Result<String, AtlasError> {
    validate_segment_semantic_receipt(proposal, receipt)?;
    let receipt = serde_json::to_value(receipt)
        .map_err(|error| format!("could not encode semantic receipt: {error}"))?;
    propose_segment_with_semantics(
        scene,
        proposal,
        "multimodal-agent",
        receipt,
        default_segment_source()?,
        author,
    )
}

fn propose_segment_with_semantics(
    scene: &mut Scene,
    proposal: &SegmentProposal,
    semantics_source: &str,
    semantics_receipt: JsonValue,
    source: &SegmentSourceContract,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    capacity(&atlas)?;
    let label = text(&proposal.label, MAX_LABEL, "segment label")?;
    validate_semantic_list(&proposal.tags, "segment tags")?;
    validate_semantic_list(&proposal.ocr, "segment OCR associations")?;
    validate_source_box_for(&proposal.prompt_box, "prompt_box", source)?;
    coord(proposal.x, "x")?;
    coord(proposal.y, "y")?;
    let animation = one_of(&proposal.animation, SEGMENT_ANIMATIONS, "animation")?;
    let parent_id = proposal.parent_id.trim();
    if !parent_id.is_empty() {
        let parent = atlas
            .shape(parent_id)
            .filter(|shape| shape.form == "segment")
            .ok_or_else(|| format!("no parent segment {parent_id:?} on the atlas"))?;
        if parent.segment_status != "materialized" {
            return Err("a part can only be added to a materialized segment".to_string());
        }
        if parent.segment_source_sha256 != source.sha256
            || parent.segment_source_width != source.width
            || parent.segment_source_height != source.height
        {
            return Err("a part must use the same generated source as its parent".to_string());
        }
        let parent_depth = segment_depth(&atlas, parent_id)?;
        if parent_depth >= SEGMENT_MAX_DEPTH {
            return Err(format!(
                "segment hierarchy may be at most {} levels below the root",
                SEGMENT_MAX_DEPTH
            ));
        }
    }
    let minimum_width = if parent_id.is_empty() { 64.0 } else { 8.0 };
    if !proposal.w.is_finite() || !(minimum_width..=2_000.0).contains(&proposal.w) {
        return Err(format!(
            "segment w must be a finite number from {minimum_width:.0} to 2000"
        ));
    }
    let source_width = proposal.prompt_box[2] - proposal.prompt_box[0];
    let source_height = proposal.prompt_box[3] - proposal.prompt_box[1];
    let height = proposal.w * source_height / source_width;
    let prompt_box = serde_json::to_string(&proposal.prompt_box)
        .map_err(|error| format!("could not encode prompt box: {error}"))?;
    let tags = serde_json::to_string(&proposal.tags)
        .map_err(|error| format!("could not encode segment tags: {error}"))?;
    let ocr = serde_json::to_string(&proposal.ocr)
        .map_err(|error| format!("could not encode segment OCR associations: {error}"))?;
    let semantics_receipt = serde_json::to_string(&semantics_receipt)
        .map_err(|error| format!("could not encode semantic receipt: {error}"))?;
    let props = vec![
        ("form", PropValue::Str("segment".to_string())),
        ("label", PropValue::Str(label)),
        ("x", PropValue::Num(proposal.x)),
        ("y", PropValue::Num(proposal.y)),
        ("w", PropValue::Num(proposal.w)),
        ("h", PropValue::Num(height)),
        ("segment_status", PropValue::Str("proposed".to_string())),
        ("segment_tags", PropValue::Str(tags)),
        ("segment_ocr", PropValue::Str(ocr)),
        (
            "segment_semantics_source",
            PropValue::Str(semantics_source.to_string()),
        ),
        (
            "segment_semantics_receipt",
            PropValue::Str(semantics_receipt),
        ),
        ("segment_source_url", PropValue::Str(source.url.to_string())),
        (
            "segment_source_sha256",
            PropValue::Str(source.sha256.to_string()),
        ),
        (
            "segment_source_width",
            PropValue::Num(f64::from(source.width)),
        ),
        (
            "segment_source_height",
            PropValue::Num(f64::from(source.height)),
        ),
        ("segment_prompt_box", PropValue::Str(prompt_box)),
        ("segment_occlusion_box", PropValue::Str("[]".to_string())),
        ("segment_wing_prompts", PropValue::Str("{}".to_string())),
        ("segment_parts", PropValue::Str("[]".to_string())),
        ("segment_parent_id", PropValue::Str(parent_id.to_string())),
        ("segment_flap", PropValue::Num(0.0)),
        ("segment_generation", PropValue::Num(1.0)),
        ("segment_animation", PropValue::Str(animation)),
        ("segment_motion", PropValue::Str("null".to_string())),
        ("segment_pivot_x", PropValue::Num(0.5)),
        ("segment_pivot_y", PropValue::Num(0.5)),
        ("touched_by", PropValue::Str(author.as_str().to_string())),
        (
            "z",
            PropValue::Num(
                atlas
                    .shapes
                    .iter()
                    .fold(0.0f64, |top, shape| top.max(shape.z))
                    + 1.0,
            ),
        ),
    ];
    scene
        .create_object_with_props(KIND_SHAPE, author.clone(), &props)
        .map(ObjectId::into_string)
        .map_err(|error| format!("could not mint segment object: {error}"))
}

/// Mark a new prompt generation on the same object id. Existing appearance
/// remains visible until the browser attaches the matching generation.
pub fn regenerate_segment(
    scene: &mut Scene,
    id: &str,
    prompt_box: [f64; 4],
    occlusion_box: [f64; 4],
    author: &Author,
) -> Result<u32, AtlasError> {
    let atlas = read(scene)?;
    let segment = atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    let source = segment_source_by_identity(
        &segment.segment_source_sha256,
        segment.segment_source_width,
        segment.segment_source_height,
    )
    .ok_or_else(|| format!("segment {id:?} does not identify a generated source"))?;
    validate_source_box_for(&prompt_box, "prompt_box", source)?;
    validate_source_box_for(&occlusion_box, "occlusion_box", source)?;
    let generation = segment
        .segment_generation
        .checked_add(1)
        .ok_or_else(|| "segment generation overflow".to_string())?;
    let prompt = serde_json::to_string(&prompt_box)
        .map_err(|error| format!("could not encode prompt box: {error}"))?;
    let occlusion = serde_json::to_string(&occlusion_box)
        .map_err(|error| format!("could not encode occlusion box: {error}"))?;
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("segment_status", PropValue::Str("proposed".to_string())),
                ("segment_prompt_box", PropValue::Str(prompt)),
                ("segment_occlusion_box", PropValue::Str(occlusion)),
                ("segment_wing_prompts", PropValue::Str("{}".to_string())),
                ("segment_parts", PropValue::Str("[]".to_string())),
                ("segment_flap", PropValue::Num(0.0)),
                ("segment_generation", PropValue::Num(f64::from(generation))),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not request segment regeneration: {error}"))?;
    Ok(generation)
}

/// Request two independently measured wing masks for the same semantic
/// object. The browser reuses one real MobileSAM encoder result and performs
/// a decoder pass for each source-pixel box.
pub fn request_segment_wings(
    scene: &mut Scene,
    id: &str,
    left: [f64; 4],
    right: [f64; 4],
    author: &Author,
) -> Result<u32, AtlasError> {
    validate_source_box(&left, "left wing box")?;
    validate_source_box(&right, "right wing box")?;
    if left[2] > right[0] {
        return Err("left and right wing boxes must not overlap horizontally".to_string());
    }
    let atlas = read(scene)?;
    let segment = atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    let generation = segment
        .segment_generation
        .checked_add(1)
        .ok_or_else(|| "segment generation overflow".to_string())?;
    let prompts = serde_json::to_string(&serde_json::json!({
        "left-wing": left,
        "right-wing": right,
    }))
    .map_err(|error| format!("could not encode wing prompt boxes: {error}"))?;
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("segment_status", PropValue::Str("proposed".to_string())),
                ("segment_occlusion_box", PropValue::Str("[]".to_string())),
                ("segment_wing_prompts", PropValue::Str(prompts)),
                ("segment_parts", PropValue::Str("[]".to_string())),
                ("segment_flap", PropValue::Num(0.0)),
                ("segment_generation", PropValue::Num(f64::from(generation))),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not request segment wing masks: {error}"))?;
    Ok(generation)
}

fn validate_segment_part(
    id: &str,
    generation: u32,
    part: &SegmentPartMaterialization,
    expected_role: &str,
    expected_prompt: [f64; 4],
) -> Result<(), AtlasError> {
    if part.role != expected_role {
        return Err(format!(
            "wing role must be {expected_role:?}; got {:?}",
            part.role
        ));
    }
    if part.prompt_box != expected_prompt {
        return Err(format!(
            "{expected_role} wing prompt does not match the request"
        ));
    }
    validate_source_box(&part.prompt_box, &format!("{expected_role} wing prompt"))?;
    validate_source_box(
        &part.mask_source_box,
        &format!("{expected_role} wing mask source box"),
    )?;
    if part.mask_encoding != SEGMENT_MASK_ENCODING {
        return Err(format!(
            "{expected_role} wing mask encoding must be {SEGMENT_MASK_ENCODING}"
        ));
    }
    if part.mask_width == 0
        || part.mask_height == 0
        || part.mask_width > MAX_SEGMENT_MASK_SIDE
        || part.mask_height > MAX_SEGMENT_MASK_SIDE
    {
        return Err(format!(
            "{expected_role} wing mask must be 1..={MAX_SEGMENT_MASK_SIDE} pixels on each side"
        ));
    }
    if !part.predicted_iou.is_finite() {
        return Err(format!(
            "{expected_role} wing predicted_iou must be the finite raw MobileSAM regressor output"
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&part.mask_runs)
        .map_err(|error| format!("{expected_role} wing mask is not valid base64: {error}"))?;
    if bytes.len() > MAX_SEGMENT_MASK_BYTES || bytes.len() % 4 != 0 {
        return Err(format!(
            "{expected_role} wing mask RLE must be bounded little-endian u32 values"
        ));
    }
    let mut total = 0u64;
    for chunk in bytes.chunks_exact(4) {
        let run = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        total = total
            .checked_add(u64::from(run))
            .ok_or_else(|| format!("{expected_role} wing mask RLE length overflow"))?;
    }
    let expected = u64::from(part.mask_width) * u64::from(part.mask_height);
    if total != expected {
        return Err(format!(
            "{expected_role} wing mask RLE covers {total} pixels, expected {expected}"
        ));
    }
    let receipt = part
        .receipt
        .as_object()
        .ok_or_else(|| format!("{expected_role} wing receipt must be a JSON object"))?;
    for (name, expected) in [
        ("role", expected_role),
        ("modelId", SEGMENT_MODEL_ID),
        ("backendRequested", "webgpu"),
        ("executionLocation", "browser-worker"),
        ("sourceSha256", SEGMENT_SOURCE_SHA256),
        ("encoderSha256", SEGMENT_ENCODER_SHA256),
        ("decoderSha256", SEGMENT_DECODER_SHA256),
        ("stableObjectId", id),
        (
            "predictedIoUContract",
            "raw-regression-output-not-probability",
        ),
    ] {
        if receipt.get(name).and_then(JsonValue::as_str) != Some(expected) {
            return Err(format!(
                "{expected_role} wing receipt {name} must be {expected:?}"
            ));
        }
    }
    for name in ["alternateProviderConfigured", "fallbackUsed", "mockUsed"] {
        if receipt.get(name).and_then(JsonValue::as_bool) != Some(false) {
            return Err(format!("{expected_role} wing receipt {name} must be false"));
        }
    }
    if receipt
        .get("adapterBoundToRuntime")
        .and_then(JsonValue::as_bool)
        != Some(true)
    {
        return Err(format!(
            "{expected_role} wing receipt adapterBoundToRuntime must be true"
        ));
    }
    if receipt.get("generation").and_then(JsonValue::as_u64) != Some(u64::from(generation)) {
        return Err(format!(
            "{expected_role} wing receipt generation does not match the materialization"
        ));
    }
    let numbers = |name: &str| -> Result<Vec<f64>, AtlasError> {
        receipt
            .get(name)
            .and_then(JsonValue::as_array)
            .ok_or_else(|| format!("{expected_role} wing receipt {name} must be a numeric array"))?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| {
                        format!("{expected_role} wing receipt {name} must contain finite numbers")
                    })
            })
            .collect()
    };
    if numbers("promptBox")? != part.prompt_box
        || numbers("maskSourceBox")? != part.mask_source_box
        || numbers("materializedMaskSize")?
            != vec![f64::from(part.mask_width), f64::from(part.mask_height)]
    {
        return Err(format!(
            "{expected_role} wing receipt geometry does not match the materialization"
        ));
    }
    let receipt_iou = receipt
        .get("predictedIoU")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| format!("{expected_role} wing receipt predictedIoU must be a number"))?;
    if (receipt_iou - part.predicted_iou).abs() > f64::EPSILON {
        return Err(format!(
            "{expected_role} wing receipt predictedIoU does not match the materialization"
        ));
    }
    let raw_in_unit_interval = (0.0..=1.0).contains(&part.predicted_iou);
    if receipt
        .get("predictedIoURawInUnitInterval")
        .and_then(JsonValue::as_bool)
        != Some(raw_in_unit_interval)
    {
        return Err(format!(
            "{expected_role} wing receipt raw score interval flag does not match"
        ));
    }
    Ok(())
}

/// Attach browser-derived mask pixels to an already-minted generation.
/// This is a derived measurement, so it never rewrites either attribution
/// field or the world transform.
pub fn materialize_segment(
    scene: &mut Scene,
    id: &str,
    result: &SegmentMaterialization,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    let segment = atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    let source = segment_source_by_identity(
        &segment.segment_source_sha256,
        segment.segment_source_width,
        segment.segment_source_height,
    )
    .ok_or_else(|| format!("segment {id:?} does not identify a generated source"))?;
    if segment.segment_status != "proposed" || result.generation != segment.segment_generation {
        return Err(format!(
            "segment {id} expects proposed generation {}, got {}",
            segment.segment_generation, result.generation
        ));
    }
    if result.model_id != SEGMENT_MODEL_ID
        || result.encoder_sha256 != SEGMENT_ENCODER_SHA256
        || result.decoder_sha256 != SEGMENT_DECODER_SHA256
    {
        return Err("segment materialization did not use the pinned MobileSAM graphs".to_string());
    }
    if result.mask_encoding != SEGMENT_MASK_ENCODING {
        return Err(format!(
            "segment mask encoding must be {SEGMENT_MASK_ENCODING}; got {:?}",
            result.mask_encoding
        ));
    }
    if result.mask_width == 0
        || result.mask_height == 0
        || result.mask_width > MAX_SEGMENT_MASK_SIDE
        || result.mask_height > MAX_SEGMENT_MASK_SIDE
    {
        return Err(format!(
            "segment mask must be 1..={MAX_SEGMENT_MASK_SIDE} pixels on each side"
        ));
    }
    validate_source_box_for(&result.mask_source_box, "mask_source_box", source)?;
    if !result.predicted_iou.is_finite() {
        return Err("predicted_iou must be the finite raw MobileSAM regressor output".to_string());
    }
    let mask_bytes = base64::engine::general_purpose::STANDARD
        .decode(&result.mask_runs)
        .map_err(|error| format!("segment mask is not valid base64: {error}"))?;
    if mask_bytes.len() > MAX_SEGMENT_MASK_BYTES || mask_bytes.len() % 4 != 0 {
        return Err("segment mask RLE must be bounded little-endian u32 values".to_string());
    }
    let mut total = 0u64;
    for chunk in mask_bytes.chunks_exact(4) {
        let run = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        total = total
            .checked_add(u64::from(run))
            .ok_or_else(|| "segment mask RLE length overflow".to_string())?;
    }
    let expected = u64::from(result.mask_width) * u64::from(result.mask_height);
    if total != expected {
        return Err(format!(
            "segment mask RLE covers {total} pixels, expected {expected}"
        ));
    }
    if !result.receipt.is_object() {
        return Err("segment receipt must be a JSON object".to_string());
    }
    let receipt = result
        .receipt
        .as_object()
        .ok_or_else(|| "segment receipt must be a JSON object".to_string())?;
    let exact_string = |name: &str, expected: &str| {
        if receipt.get(name).and_then(JsonValue::as_str) == Some(expected) {
            Ok(())
        } else {
            Err(format!("segment receipt {name} must be {expected:?}"))
        }
    };
    let exact_bool = |name: &str, expected: bool| {
        if receipt.get(name).and_then(JsonValue::as_bool) == Some(expected) {
            Ok(())
        } else {
            Err(format!("segment receipt {name} must be {expected}"))
        }
    };
    exact_string("modelId", SEGMENT_MODEL_ID)?;
    exact_string("backendRequested", "webgpu")?;
    exact_string("executionLocation", "browser-worker")?;
    exact_string("sourceSha256", source.sha256)?;
    exact_string("encoderSha256", SEGMENT_ENCODER_SHA256)?;
    exact_string("decoderSha256", SEGMENT_DECODER_SHA256)?;
    exact_string("stableObjectId", id)?;
    exact_string(
        "predictedIoUContract",
        "raw-regression-output-not-probability",
    )?;
    exact_bool("alternateProviderConfigured", false)?;
    exact_bool("adapterBoundToRuntime", true)?;
    exact_bool("fallbackUsed", false)?;
    exact_bool("mockUsed", false)?;
    if receipt.get("generation").and_then(JsonValue::as_u64) != Some(u64::from(result.generation)) {
        return Err("segment receipt generation does not match the materialization".to_string());
    }
    let receipt_numbers = |name: &str| -> Result<Vec<f64>, AtlasError> {
        receipt
            .get(name)
            .and_then(JsonValue::as_array)
            .ok_or_else(|| format!("segment receipt {name} must be a numeric array"))?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("segment receipt {name} must contain finite numbers"))
            })
            .collect()
    };
    if receipt_numbers("sourceSize")? != vec![f64::from(source.width), f64::from(source.height)] {
        return Err("segment receipt sourceSize does not match the pinned source".to_string());
    }
    if receipt_numbers("maskSourceBox")? != result.mask_source_box {
        return Err("segment receipt maskSourceBox does not match the materialization".to_string());
    }
    if receipt_numbers("materializedMaskSize")?
        != vec![f64::from(result.mask_width), f64::from(result.mask_height)]
    {
        return Err(
            "segment receipt materializedMaskSize does not match the materialization".to_string(),
        );
    }
    let receipt_iou = receipt
        .get("predictedIoU")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| "segment receipt predictedIoU must be a number".to_string())?;
    if (receipt_iou - result.predicted_iou).abs() > f64::EPSILON {
        return Err("segment receipt predictedIoU does not match the materialization".to_string());
    }
    let raw_in_unit_interval = (0.0..=1.0).contains(&result.predicted_iou);
    if receipt
        .get("predictedIoURawInUnitInterval")
        .and_then(JsonValue::as_bool)
        != Some(raw_in_unit_interval)
    {
        return Err(
            "segment receipt predictedIoURawInUnitInterval does not match the raw score"
                .to_string(),
        );
    }
    let occluded = !segment.segment_occlusion_box.is_empty();
    if receipt.get("occlusionApplied").and_then(JsonValue::as_bool) != Some(occluded) {
        return Err(
            "segment receipt occlusionApplied does not match the proposed source".to_string(),
        );
    }
    if occluded {
        if receipt_numbers("occlusionBox")? != segment.segment_occlusion_box {
            return Err(
                "segment receipt occlusionBox does not match the proposed source".to_string(),
            );
        }
    } else if !receipt.get("occlusionBox").is_some_and(JsonValue::is_null) {
        return Err(
            "segment receipt occlusionBox must be null for the unoccluded source".to_string(),
        );
    }
    let wing_prompts = segment
        .segment_wing_prompts
        .as_object()
        .filter(|prompts| !prompts.is_empty());
    if let Some(prompts) = wing_prompts {
        if result.parts.len() != 2 {
            return Err("wing materialization must contain exactly two parts".to_string());
        }
        let prompt = |role: &str| -> Result<[f64; 4], AtlasError> {
            let values = prompts
                .get(role)
                .and_then(JsonValue::as_array)
                .ok_or_else(|| format!("wing request is missing {role:?}"))?;
            if values.len() != 4 {
                return Err(format!("wing request {role:?} must contain four numbers"));
            }
            let mut out = [0.0; 4];
            for (slot, value) in out.iter_mut().zip(values) {
                *slot = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("wing request {role:?} contains an invalid number"))?;
            }
            Ok(out)
        };
        let expected = [
            ("left-wing", prompt("left-wing")?),
            ("right-wing", prompt("right-wing")?),
        ];
        for (role, box_) in expected {
            let matching = result.parts.iter().filter(|part| part.role == role).count();
            if matching != 1 {
                return Err(format!(
                    "wing materialization must contain one {role}, found {matching}"
                ));
            }
            let part = result
                .parts
                .iter()
                .find(|part| part.role == role)
                .ok_or_else(|| format!("wing materialization is missing {role}"))?;
            validate_segment_part(id, result.generation, part, role, box_)?;
        }
        let receipt_prompt_boxes = receipt
            .get("partPromptBoxes")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| "segment receipt partPromptBoxes must be an array".to_string())?
            .iter()
            .map(|box_| {
                box_.as_array()
                    .ok_or_else(|| {
                        "segment receipt partPromptBoxes must contain numeric arrays".to_string()
                    })?
                    .iter()
                    .map(|value| {
                        value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| {
                                "segment receipt partPromptBoxes must contain finite numbers"
                                    .to_string()
                            })
                    })
                    .collect::<Result<Vec<_>, AtlasError>>()
            })
            .collect::<Result<Vec<_>, AtlasError>>()?;
        if receipt.get("partCount").and_then(JsonValue::as_u64) != Some(2)
            || receipt.get("partRoles") != Some(&serde_json::json!(["left-wing", "right-wing"]))
            || receipt_prompt_boxes != vec![expected[0].1.to_vec(), expected[1].1.to_vec()]
        {
            return Err("segment receipt wing manifest does not match the request".to_string());
        }
    } else if !result.parts.is_empty() {
        return Err("wing parts were returned without a wing request".to_string());
    }
    let receipt = serde_json::to_string(&result.receipt)
        .map_err(|error| format!("could not encode segment receipt: {error}"))?;
    if receipt.len() > 40_000 {
        return Err("segment receipt exceeds 40000 bytes".to_string());
    }
    let source_box = serde_json::to_string(&result.mask_source_box)
        .map_err(|error| format!("could not encode mask source box: {error}"))?;
    let parts = serde_json::to_string(&result.parts)
        .map_err(|error| format!("could not encode segment wing parts: {error}"))?;
    if parts.len() > 100_000 {
        return Err("segment wing evidence exceeds 100000 bytes".to_string());
    }
    let source_width = result.mask_source_box[2] - result.mask_source_box[0];
    let source_height = result.mask_source_box[3] - result.mask_source_box[1];
    let next_height = segment.w * source_height / source_width;
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("h", PropValue::Num(next_height)),
                ("segment_status", PropValue::Str("materialized".to_string())),
                ("segment_model_id", PropValue::Str(result.model_id.clone())),
                (
                    "segment_encoder_sha256",
                    PropValue::Str(result.encoder_sha256.clone()),
                ),
                (
                    "segment_decoder_sha256",
                    PropValue::Str(result.decoder_sha256.clone()),
                ),
                (
                    "segment_predicted_iou",
                    PropValue::Num(result.predicted_iou),
                ),
                (
                    "segment_mask_encoding",
                    PropValue::Str(result.mask_encoding.clone()),
                ),
                (
                    "segment_mask_width",
                    PropValue::Num(f64::from(result.mask_width)),
                ),
                (
                    "segment_mask_height",
                    PropValue::Num(f64::from(result.mask_height)),
                ),
                (
                    "segment_mask_runs",
                    PropValue::Str(result.mask_runs.clone()),
                ),
                ("segment_mask_source_box", PropValue::Str(source_box)),
                ("segment_parts", PropValue::Str(parts)),
                ("segment_receipt", PropValue::Str(receipt)),
            ],
        )
        .map_err(|error| format!("could not attach segment appearance: {error}"))
}

/// Attach an existing mask part beneath another part of the same root object.
/// World geometry is unchanged. Roots cannot be nested, cross-object moves are
/// refused, and the resulting subtree must stay within [`SEGMENT_MAX_DEPTH`].
pub fn set_segment_parent(
    scene: &mut Scene,
    id: &str,
    parent_id: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    let segment = atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    if segment.segment_status != "materialized" {
        return Err("only a materialized segment can be reparented".to_string());
    }
    if segment.segment_parent_id.is_empty() {
        return Err("a root object cannot be nested under another segment".to_string());
    }
    let parent_id = parent_id.trim();
    let parent = atlas
        .shape(parent_id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no parent segment {parent_id:?} on the atlas"))?;
    if parent.segment_status != "materialized" {
        return Err("a part can only be attached to a materialized segment".to_string());
    }
    if id == parent_id || segment_is_descendant(&atlas, parent_id, id) {
        return Err("segment reparenting cannot create a hierarchy cycle".to_string());
    }
    if segment_root_id(&atlas, id)? != segment_root_id(&atlas, parent_id)? {
        return Err("a segment part cannot move between root objects".to_string());
    }
    let resulting_depth = segment_depth(&atlas, parent_id)? + 1 + segment_subtree_depth(&atlas, id);
    if resulting_depth > SEGMENT_MAX_DEPTH {
        return Err(format!(
            "segment hierarchy may be at most {} levels below the root",
            SEGMENT_MAX_DEPTH
        ));
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("segment_parent_id", PropValue::Str(parent_id.to_string())),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not reparent segment: {error}"))
}

fn validate_segment_motion(
    atlas: &Atlas,
    owner_id: &str,
    motion: &SegmentMotionProgram,
) -> Result<usize, AtlasError> {
    if motion.schema != SEGMENT_MOTION_SCHEMA {
        return Err(format!(
            "motion schema must be {SEGMENT_MOTION_SCHEMA:?}; got {:?}",
            motion.schema
        ));
    }
    let _ = text(&motion.label, MAX_LABEL, "motion label")?;
    let _ = one_of(
        &motion.child_compositing,
        SEGMENT_CHILD_COMPOSITING,
        "child_compositing",
    )?;
    if motion.tracks.is_empty() || motion.tracks.len() > 16 {
        return Err("motion must contain from 1 to 16 tracks".to_string());
    }
    let owner = atlas
        .shape(owner_id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {owner_id:?} on the atlas"))?;
    if owner.segment_status != "materialized" {
        return Err("motion owner must be a materialized segment".to_string());
    }
    let mut allowed = std::collections::BTreeSet::from([owner_id.to_string()]);
    allowed.extend(
        atlas
            .shapes
            .iter()
            .filter(|shape| {
                shape.form == "segment"
                    && shape.segment_status == "materialized"
                    && segment_is_descendant(atlas, &shape.id, owner_id)
            })
            .map(|shape| shape.id.clone()),
    );
    let mut targeted = std::collections::BTreeSet::new();
    for track in &motion.tracks {
        let _ = text(&track.label, MAX_LABEL, "motion track label")?;
        if track.target_ids.is_empty() || track.target_ids.len() > 32 {
            return Err("each motion track must target from 1 to 32 segments".to_string());
        }
        if !(50..=120_000).contains(&track.duration_ms) {
            return Err("motion duration_ms must be from 50 to 120000".to_string());
        }
        if track.delay_ms > 60_000 || track.stagger_ms > 60_000 {
            return Err("motion delay_ms and stagger_ms must not exceed 60000".to_string());
        }
        if let Some(curve) = track.curve {
            if !curve.iter().all(|value| value.is_finite())
                || !(0.0..=1.0).contains(&curve[0])
                || !(0.0..=1.0).contains(&curve[2])
                || !(-10.0..=10.0).contains(&curve[1])
                || !(-10.0..=10.0).contains(&curve[3])
            {
                return Err(
                    "motion curve must be finite [x1,y1,x2,y2], with x from 0 to 1 and y from -10 to 10"
                        .to_string(),
                );
            }
        }
        if let Some(origin) = track.origin {
            if !origin
                .iter()
                .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
            {
                return Err("motion origin values must be finite numbers from 0 to 1".to_string());
            }
        }
        if !(2..=24).contains(&track.keyframes.len()) {
            return Err("each motion track must contain from 2 to 24 keyframes".to_string());
        }
        let first = track
            .keyframes
            .first()
            .map(|frame| frame.at)
            .unwrap_or(f64::NAN);
        let last = track
            .keyframes
            .last()
            .map(|frame| frame.at)
            .unwrap_or(f64::NAN);
        if first.abs() > 1e-9 || (last - 1.0).abs() > 1e-9 {
            return Err("motion keyframes must start at 0 and end at 1".to_string());
        }
        let mut prior = -1.0;
        for frame in &track.keyframes {
            if !frame.at.is_finite() || !(0.0..=1.0).contains(&frame.at) || frame.at <= prior {
                return Err(
                    "motion keyframe `at` values must be finite, unique, and strictly increasing"
                        .to_string(),
                );
            }
            prior = frame.at;
            for (name, value, low, high) in [
                ("x", frame.x, -4_000.0, 4_000.0),
                ("y", frame.y, -4_000.0, 4_000.0),
                ("rotate", frame.rotate, -7_200.0, 7_200.0),
                ("scale_x", frame.scale_x, 0.01, 100.0),
                ("scale_y", frame.scale_y, 0.01, 100.0),
                ("opacity", frame.opacity, 0.0, 1.0),
            ] {
                if !value.is_finite() || !(low..=high).contains(&value) {
                    return Err(format!(
                        "motion keyframe {name} must be a finite number from {low} to {high}"
                    ));
                }
            }
        }
        for target_id in &track.target_ids {
            if !allowed.contains(target_id) {
                return Err(format!(
                    "motion owner {owner_id} cannot target segment {target_id:?}; use the owner itself or a materialized descendant"
                ));
            }
            if !targeted.insert(target_id.clone()) {
                return Err(format!(
                    "segment {target_id:?} appears in more than one motion track; combine its transforms into one track"
                ));
            }
        }
    }
    Ok(targeted.len())
}

/// Set or clear one validated keyframe program without changing any segment's
/// saved position. Returns the number of targeted segments.
pub fn set_segment_motion(
    scene: &mut Scene,
    owner_id: &str,
    motion: Option<&SegmentMotionProgram>,
    author: &Author,
) -> Result<usize, AtlasError> {
    let atlas = read(scene)?;
    let owner = atlas
        .shape(owner_id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {owner_id:?} on the atlas"))?;
    if owner.segment_status != "materialized" {
        return Err("motion owner must be a materialized segment".to_string());
    }
    let (encoded, target_count) = match motion {
        Some(program) => (
            serde_json::to_string(program)
                .map_err(|error| format!("could not encode segment motion: {error}"))?,
            validate_segment_motion(&atlas, owner_id, program)?,
        ),
        None => ("null".to_string(), 0),
    };
    scene
        .set_props(
            &ObjectId::from(owner_id.to_string()),
            &[
                ("segment_motion", PropValue::Str(encoded)),
                ("segment_animation", PropValue::Str("none".to_string())),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not set segment motion: {error}"))?;
    Ok(target_count)
}

pub fn set_segment_animation(
    scene: &mut Scene,
    id: &str,
    animation: &str,
    author: &Author,
) -> Result<(), AtlasError> {
    let atlas = read(scene)?;
    let segment = atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    let animation = one_of(animation, SEGMENT_ANIMATIONS, "animation")?;
    if animation == "tap-parts" {
        if !segment.segment_parent_id.is_empty() {
            return Err("tap-parts can only animate a root segment".to_string());
        }
        if !atlas.shapes.iter().any(|shape| {
            shape.form == "segment"
                && shape.segment_parent_id == id
                && shape.segment_status == "materialized"
        }) {
            return Err("tap-parts requires at least one materialized mask part".to_string());
        }
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("segment_animation", PropValue::Str(animation)),
                ("segment_motion", PropValue::Str("null".to_string())),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not animate segment: {error}"))
}

/// Set the normalized 2D transform origin used by the segment animation.
///
/// This is intentionally a point on the image plane. Arbitrary 3D axes and a
/// modeling gizmo are outside this bounded canvas workflow.
pub fn set_segment_pivot(
    scene: &mut Scene,
    id: &str,
    x: f64,
    y: f64,
    author: &Author,
) -> Result<(), AtlasError> {
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(
            "segment pivot x and y must be finite normalized numbers from 0 to 1".to_string(),
        );
    }
    let atlas = read(scene)?;
    atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("segment_pivot_x", PropValue::Num(x)),
                ("segment_pivot_y", PropValue::Num(y)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not set segment pivot: {error}"))
}

/// Set the mirrored wing rotation authored through the human slider.
pub fn set_segment_flap(
    scene: &mut Scene,
    id: &str,
    flap: f64,
    author: &Author,
) -> Result<(), AtlasError> {
    if !flap.is_finite() || !(-1.0..=1.0).contains(&flap) {
        return Err("segment flap must be a finite number from -1 to 1".to_string());
    }
    let atlas = read(scene)?;
    let segment = atlas
        .shape(id)
        .filter(|shape| shape.form == "segment")
        .ok_or_else(|| format!("no segment {id:?} on the atlas"))?;
    if segment.segment_status != "materialized"
        || segment.segment_parts.as_array().map(Vec::len) != Some(2)
    {
        return Err("segment wings must be materialized before they can flap".to_string());
    }
    scene
        .set_props(
            &ObjectId::from(id.to_string()),
            &[
                ("segment_flap", PropValue::Num(flap)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not set segment flap: {error}"))
}

/// A create-or-update for one shape.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShapePatch {
    pub id: Option<String>,
    pub form: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    /// `"x,y x,y ..."` relative to the origin.
    pub points: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub head: Option<String>,
    pub ink: Option<String>,
    pub fill: Option<String>,
    pub label: Option<String>,
    pub stroke_width: Option<f64>,
    pub stroke_style: Option<String>,
    pub opacity: Option<f64>,
    pub roundness: Option<String>,
    pub font_size: Option<f64>,
    /// Radians, clockwise on screen. Sized forms only.
    pub angle: Option<f64>,
    /// Whitespace-separated group ids, innermost last. Empty string leaves
    /// every group.
    pub groups: Option<String>,
    /// A name this call gives the shape so a later shape in the same batch
    /// can bind `from`/`to` to it, or sit in it as a `frame`, before it has
    /// an id. Never stored.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Id of a `frame` shape, or empty string to take the shape out of its
    /// frame.
    pub frame: Option<String>,
}

/// Create or update one shape. Returns the object id.
pub fn place_shape(
    scene: &mut Scene,
    patch: &ShapePatch,
    author: &Author,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    let existing = patch
        .id
        .as_ref()
        .map(|id| {
            atlas
                .shapes
                .iter()
                .find(|shape| &shape.id == id)
                .ok_or_else(|| format!("no shape {id:?} on the atlas"))
        })
        .transpose()?;

    if existing.is_some_and(|shape| shape.form == "segment") && patch.form.is_some() {
        return Err(
            "an existing segment keeps its form for its whole identity lifecycle".to_string(),
        );
    }

    let form = match (&patch.form, existing) {
        (Some(form), _) => one_of(form, FORMS, "form")?,
        (None, Some(shape)) => shape.form.clone(),
        (None, None) => return Err("a new shape needs a form".to_string()),
    };
    let is_box = matches!(form.as_str(), "rect" | "ellipse" | "diamond" | "frame");
    let is_text = form == "text";
    let is_segment = form == "segment";
    // A text has a box on screen but nothing is ever inside it, so it takes
    // `w` like a box form and takes nothing else from them.
    let is_sized = is_box || is_text || is_segment;

    let mut props: Vec<(&str, PropValue)> = vec![("form", PropValue::Str(form.clone()))];

    if let Some(width) = patch.stroke_width {
        if !width.is_finite() {
            return Err("stroke_width must be a finite number".to_string());
        }
        props.push((
            "stroke_width",
            PropValue::Num(width.clamp(STROKE_WIDTH_RANGE.0, STROKE_WIDTH_RANGE.1)),
        ));
    }
    if let Some(style) = &patch.stroke_style {
        props.push((
            "stroke_style",
            PropValue::Str(one_of(style, STROKE_STYLES, "stroke_style")?),
        ));
    }
    if let Some(opacity) = patch.opacity {
        if !opacity.is_finite() {
            return Err("opacity must be a finite number from 0 to 100".to_string());
        }
        props.push((
            "opacity",
            PropValue::Num(opacity.clamp(OPACITY_RANGE.0, OPACITY_RANGE.1)),
        ));
    }
    if let Some(roundness) = &patch.roundness {
        props.push((
            "roundness",
            PropValue::Str(one_of(roundness, ROUNDNESS, "roundness")?),
        ));
    }
    if let Some(size) = patch.font_size {
        if !size.is_finite() {
            return Err("font_size must be a finite number".to_string());
        }
        // Zero means "unset"; anything else lands inside the range.
        let size = if size <= 0.0 {
            0.0
        } else {
            size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1)
        };
        props.push(("font_size", PropValue::Num(size)));
    }
    if let Some(angle) = patch.angle {
        if !angle.is_finite() {
            return Err("angle must be a finite number of radians".to_string());
        }
        if !is_sized {
            return Err(format!(
                "a {form} is rotated by rotating its points, not with `angle`"
            ));
        }
        // Normalised so two writers cannot disagree about the same rotation.
        let turn = std::f64::consts::TAU;
        let mut angle = angle % turn;
        if angle > std::f64::consts::PI {
            angle -= turn;
        } else if angle <= -std::f64::consts::PI {
            angle += turn;
        }
        props.push(("angle", PropValue::Num(angle)));
    }
    if let Some(groups) = &patch.groups {
        let ids = parse_groups(groups);
        if ids.len() > MAX_GROUPS {
            return Err(format!(
                "a shape may belong to at most {MAX_GROUPS} groups; got {}",
                ids.len()
            ));
        }
        for id in &ids {
            text(id, MAX_LABEL, "group id")?;
        }
        props.push(("groups", PropValue::Str(format_groups(&ids))));
    }
    if let Some(frame) = &patch.frame {
        let frame = frame.trim();
        if frame.is_empty() {
            props.push(("frame", PropValue::Str(String::new())));
        } else {
            if form == "frame" {
                return Err("a frame cannot sit inside another frame".to_string());
            }
            match atlas.shape(frame) {
                Some(holder) if holder.form == "frame" => {}
                Some(holder) => {
                    return Err(format!(
                        "{frame:?} is a {}, not a frame; only a frame holds other shapes",
                        holder.form
                    ));
                }
                None => return Err(format!("no frame {frame:?} on the atlas")),
            }
            props.push(("frame", PropValue::Str(frame.to_string())));
        }
    }

    // A bound endpoint is resolved from its target's live position every time
    // the shape is drawn or read, so it has to name a claim card or a sized
    // drawing shape that exists.
    for (key, value) in [("from", &patch.from), ("to", &patch.to)] {
        let Some(id) = value else { continue };
        if id.is_empty() {
            props.push((key, PropValue::Str(String::new())));
            continue;
        }
        if form != "arrow" && form != "line" {
            return Err(format!(
                "only an arrow or a line can bind to another object; this is a {form}"
            ));
        }
        if atlas.binding_bounds(id).is_none() {
            return Err(format!(
                "no bindable node or shape {id:?}; arrows bind to cards, boxes, diamonds, ellipses, or text"
            ));
        }
        props.push((key, PropValue::Str(id.clone())));
    }

    if let Some(raw) = &patch.points {
        if is_sized {
            return Err(format!("a {form} is sized with w/h, not points"));
        }
        let points = simplify(&parse_points(raw), SIMPLIFY_TOLERANCE);
        if points.len() < 2 {
            return Err("a stroke needs at least two points".to_string());
        }
        if points.len() > MAX_POINTS {
            return Err(format!(
                "that stroke has {} points after simplification; the limit is {MAX_POINTS}",
                points.len()
            ));
        }
        for (x, y) in &points {
            coord(*x, "point x")?;
            coord(*y, "point y")?;
        }
        props.push((K_POINTS, PropValue::Str(format_points(&points))));
    }

    for (key, value) in [("x", patch.x), ("y", patch.y)] {
        if let Some(value) = value {
            props.push((key, PropValue::Num(coord(value, key)?)));
        }
    }
    for (key, value) in [("w", patch.w), ("h", patch.h)] {
        if let Some(value) = value {
            if !is_sized {
                return Err(format!("a {form} is sized by its points, not w/h"));
            }
            // The one property of a text nobody gets to author. It is however
            // many lines the words wrap to at `w`, which only the thing laying
            // it out knows — see `measure_shape`.
            if is_text && key == "h" {
                return Err(
                    "a text is as tall as its own words; set `w` to change where it wraps"
                        .to_string(),
                );
            }
            props.push((key, PropValue::Num(value.clamp(8.0, 8_000.0))));
        }
    }
    if let Some(head) = &patch.head {
        props.push(("head", PropValue::Str(one_of(head, HEADS, "head")?)));
    }
    if let Some(ink) = &patch.ink {
        props.push(("ink", PropValue::Str(one_of(ink, INKS, "ink")?)));
    }
    if let Some(fill) = &patch.fill {
        let fill = fill.trim();
        if fill != "none" && !INKS.contains(&fill) {
            return Err(format!(
                "fill must be `none` or one of {}; got {fill:?}",
                INKS.join(", ")
            ));
        }
        props.push(("fill", PropValue::Str(fill.to_string())));
    }
    if let Some(label) = &patch.label {
        if is_text && label.trim().is_empty() {
            return Err(
                "a text with no words is an invisible object; remove it instead".to_string(),
            );
        }
        props.push((
            "label",
            PropValue::Str(text(label, MAX_LABEL, "shape label")?),
        ));
    }
    props.push(("touched_by", PropValue::Str(author.as_str().to_string())));

    match existing {
        Some(shape) => {
            let id = ObjectId::from(shape.id.clone());
            scene
                .set_props(&id, &props)
                .map_err(|error| format!("could not update shape: {error}"))?;
            Ok(id.into_string())
        }
        None => {
            capacity(&atlas)?;
            let bound = patch.from.as_deref().is_some_and(|id| !id.is_empty())
                && patch.to.as_deref().is_some_and(|id| !id.is_empty());
            if !is_sized && patch.points.is_none() && !bound {
                return Err(format!(
                    "a new {form} needs either `points` or both `from` and `to`"
                ));
            }
            // A text is nothing but its words. Creating an empty one would put
            // an invisible object on a shared surface, which is the one thing
            // a page both sides have to be able to read cannot afford.
            if is_text && patch.label.as_deref().unwrap_or("").trim().is_empty() {
                return Err("a text needs `label` — that is the whole shape".to_string());
            }
            if patch.x.is_none() {
                props.push(("x", PropValue::Num(0.0)));
            }
            if patch.y.is_none() {
                props.push(("y", PropValue::Num(0.0)));
            }
            if is_box {
                if patch.w.is_none() {
                    props.push(("w", PropValue::Num(240.0)));
                }
                if patch.h.is_none() {
                    props.push(("h", PropValue::Num(160.0)));
                }
            }
            if is_text && patch.w.is_none() {
                props.push(("w", PropValue::Num(TEXT_WIDTH)));
            }
            if patch.head.is_none() {
                // An arrow without a head is a line; defaulting it the other
                // way would make every agent-drawn arrow silently directionless.
                let head = if form == "arrow" { "triangle" } else { "none" };
                props.push(("head", PropValue::Str(head.to_string())));
            }
            if patch.ink.is_none() {
                props.push(("ink", PropValue::Str("slate".to_string())));
            }
            if patch.fill.is_none() {
                props.push(("fill", PropValue::Str("none".to_string())));
            }
            // Zero is a real opacity, so absent has to be written as opaque
            // rather than read as invisible.
            if patch.opacity.is_none() {
                props.push(("opacity", PropValue::Num(OPACITY_RANGE.1)));
            }
            let top = atlas
                .shapes
                .iter()
                .fold(0.0f64, |top, shape| top.max(shape.z));
            props.push(("z", PropValue::Num(top + 1.0)));
            let id = scene
                .create_object_with_props(KIND_SHAPE, author.clone(), &props)
                .map_err(|error| format!("could not create shape: {error}"))?;
            Ok(id.into_string())
        }
    }
}

/// Put several shapes down in one call.
///
/// Same reasoning as [`draw`]: the expensive part of an agent drawing six
/// arrows is six round trips of thinking, not six mutations. Everything that
/// can be rejected without writing is — capacity for the whole batch — and a
/// failure part-way names what already landed instead of implying nothing did.
pub fn sketch(
    scene: &mut Scene,
    shapes: &[ShapePatch],
    author: &Author,
) -> Result<Vec<String>, AtlasError> {
    if shapes.is_empty() {
        return Err("a sketch needs at least one shape".to_string());
    }
    let atlas = read(scene)?;
    let creating = shapes.iter().filter(|shape| shape.id.is_none()).count();
    let total = atlas.nodes.len()
        + atlas.edges.len()
        + atlas.marks.len()
        + atlas.shapes.len()
        + atlas.constraints.len()
        + atlas.variables.len()
        + atlas.relations.len()
        + creating;
    if total > MAX_OBJECTS {
        return Err(format!(
            "this sketch would put {total} objects on an atlas that holds {MAX_OBJECTS}; split it or remove something first"
        ));
    }

    let mut drawn = Vec::new();
    // A `ref` names a shape for the rest of this batch, so an arrow can bind
    // to the box drawn three shapes earlier in the same call. Paint order is
    // the only order there is, so a ref must come before its first use.
    let mut refs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (index, patch) in shapes.iter().enumerate() {
        let mut resolved = patch.clone();
        for end in [&mut resolved.from, &mut resolved.to, &mut resolved.frame] {
            if let Some(name) = end.as_deref() {
                if let Some(id) = refs.get(name) {
                    *end = Some(id.clone());
                }
            }
        }
        match place_shape(scene, &resolved, author) {
            Ok(id) => {
                if let Some(name) = patch.reference.as_deref().filter(|name| !name.is_empty()) {
                    refs.insert(name.to_string(), id.clone());
                }
                drawn.push(id)
            }
            Err(error) if drawn.is_empty() => return Err(format!("shapes[{index}]: {error}")),
            Err(error) => {
                return Err(format!(
                    "shapes[{index}]: {error} — {} shape(s) from this sketch are already on the atlas and were not undone; read it back before retrying",
                    drawn.len()
                ));
            }
        }
    }
    Ok(drawn)
}

impl Atlas {
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

/// Nib width for ink, in world units. Here rather than in CSS because the
/// outline is geometry: the host reasons about the same band the browser
/// fills, and a stroke width that only one side knows is a stroke width the
/// two sides disagree about.
pub const INK_SIZE: f64 = 7.0;

/// A shape resolved to exactly what gets painted.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PaintedShape {
    pub shape: Shape,
    /// Resolved world endpoints for `line`/`arrow`, bindings applied.
    pub ends: Option<[(f64, f64); 2]>,
    /// Full resolved connector path in world coordinates. Empty for non-line
    /// shapes. The first and last point match `ends`.
    pub path: Vec<(f64, f64)>,
    /// Filled boundary for `ink`. Empty for every other form.
    pub outline: Vec<(f64, f64)>,
    /// What this shape is saying — the same interpretation the agent is given.
    ///
    /// Carried to the browser so the human can select a stroke and read the
    /// sentence the agent will read. A surface whose premise is that both
    /// sides see the same thing should not make one of them take that on
    /// faith when it costs a field to prove.
    pub reading: Reading,
}

/// A node-to-node link resolved to where it is actually drawn.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PaintedLink {
    /// Shared orthogonal route. Empty means routing is unavailable.
    pub path: Vec<(f64, f64)>,
    pub id: String,
    pub label: String,
    /// Clipped to both cards' edges, so the arrowhead lands on the box it
    /// means rather than under it.
    pub ends: [(f64, f64); 2],
}

/// Everything drawn, resolved for the renderer.
///
/// The browser does no drawing geometry of its own. It cannot honestly: an
/// arrow's real endpoints depend on bindings and on live, measured node boxes,
/// and an ink stroke's real edge is an outline polygon rather than its centre
/// line. Deriving either one in JavaScript would put a second opinion about
/// where the ink is on the page, and the rule here has always been that there
/// is exactly one.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Painting {
    pub shapes: Vec<PaintedShape>,
    pub links: Vec<PaintedLink>,
    pub constraints: Vec<Constraint>,
    pub constraint_state: ConstraintState,
}

impl Atlas {
    /// Every drawn thing, in paint order, resolved for the renderer.
    pub fn painting(&self) -> Painting {
        let containers: std::collections::HashSet<_> =
            self.nodes.iter().map(|node| &node.parent).collect();
        let obstacles: Vec<_> = self
            .nodes
            .iter()
            .filter(|node| !containers.contains(&node.id))
            .map(Node::bounds)
            .collect();
        let connected: Vec<_> = self
            .edges
            .iter()
            .filter_map(|edge| Some((edge, self.node(&edge.from)?, self.node(&edge.to)?)))
            .collect();
        // State-machine curves have their own renderer. They must not reserve
        // invisible orthogonal lanes around ordinary card relationships.
        let ordinary: Vec<_> = connected
            .iter()
            .filter(|(edge, _, _)| edge.event.is_empty())
            .map(|(edge, from, to)| routing::Connection {
                id: &edge.id,
                from: &edge.from,
                to: &edge.to,
                from_box: from.bounds(),
                to_box: to.bounds(),
            })
            .collect();
        let mut paths: std::collections::BTreeMap<_, _> = ordinary
            .iter()
            .zip(routing::routes(&ordinary, &obstacles))
            .map(|(edge, path)| (edge.id, path))
            .collect();
        Painting {
            shapes: self
                .shapes
                .iter()
                .map(|shape| PaintedShape {
                    ends: self.shape_endpoints(shape).map(|(a, b)| [a, b]),
                    path: self.shape_path(shape).unwrap_or_default(),
                    outline: if shape.form == "ink" {
                        outline(
                            &shape.world_points(),
                            INK_SIZE * shape.stroke_width / DEFAULT_STROKE_WIDTH,
                        )
                    } else {
                        Vec::new()
                    },
                    reading: read_shape(self, shape),
                    shape: shape.clone(),
                })
                .collect(),
            links: connected
                .into_iter()
                .map(|(edge, from, to)| {
                    let path = if edge.event.is_empty() {
                        paths.remove(edge.id.as_str()).unwrap_or_default()
                    } else {
                        routing::orthogonal(from.bounds(), to.bounds(), &obstacles)
                    };
                    let ends = match (path.first(), path.last()) {
                        (Some(a), Some(b)) => [*a, *b],
                        _ => [
                            box_edge_toward(from.bounds(), from.center(), to.center()),
                            box_edge_toward(to.bounds(), to.center(), from.center()),
                        ],
                    };
                    PaintedLink {
                        path,
                        id: edge.id.clone(),
                        label: edge.label.clone(),
                        ends,
                    }
                })
                .collect(),
            constraints: self.constraints.clone(),
            constraint_state: self.constraint_state.clone(),
        }
    }
}

/// How far a point may sit from the line its neighbours make before it is
/// carrying information rather than sampling noise.
const SIMPLIFY_TOLERANCE: f64 = 0.9;

/// Ramer–Douglas–Peucker. A pointer emits samples at the display's refresh
/// rate, not at the rate the hand changes direction, so a two-second stroke
/// arrives as several hundred points describing a shape that needs a few
/// dozen. Dropping the redundant ones at the writer keeps the document small
/// and — because every point is a byte on the wire to the other replica —
/// keeps the stroke cheap to sync.
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

// ── what a drawing is saying ──────────────────────────────────────────────

/// One shape, read as the claim it makes about the nodes around it.
///
/// This is the whole justification for letting a drawing exist on a surface
/// whose contract is that both sides can read everything. Without it the
/// agent's read-back would carry "shape s_4f2: 63 points starting at
/// (812,340)", which is not information, and the human's most natural gesture
/// — a circle around three cards — would be the one edit the agent could not
/// see.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Reading {
    pub shape: String,
    /// One of [`RELATIONS`].
    pub relation: String,
    /// Node ids involved, in a stable order.
    pub targets: Vec<String>,
    /// Cards this shape covers *some* of — drawn over, but not enough to be
    /// claimed. Carried separately rather than folded into `targets` because
    /// "I could not tell whether you meant to include this" is a different
    /// statement from "you included it", and the surface's whole contract is
    /// that the agent is told which one it is looking at.
    pub partial: Vec<String>,
}

/// A stroke counts as striking a node out when it is scrawled *across that one
/// node* rather than travelling past it: centre inside the card, and not much
/// bigger than the card.
const STRIKE_SIZE_FACTOR: f64 = 1.5;
/// How close an arrow's head must come to a card to be pointing at it.
const POINT_SLOP: f64 = 36.0;

/// Interpret every shape against the nodes it touches.
pub fn readings(atlas: &Atlas) -> Vec<Reading> {
    atlas
        .shapes
        .iter()
        .map(|shape| read_shape(atlas, shape))
        .collect()
}

/// Interpret a browser viewport against the atlas's semantic nodes.
///
/// Unlike a drawn grouping region, a viewport has no intent threshold: any
/// positive overlap means part of a node is visible. This still reuses the
/// read-back's [`Coverage`] vocabulary and bounds containment so callers can
/// distinguish fully visible nodes from clipped ones without raw geometry.
pub fn viewport_reading(atlas: &Atlas, rect: ViewportRect) -> ViewportReading {
    let viewport = rect.bounds();
    let mut reading = ViewportReading::default();
    for node in &atlas.nodes {
        let bounds = node.bounds();
        let coverage = if !overlaps(viewport, bounds, 0.0) {
            Coverage::Outside
        } else if bounds.0 >= viewport.0
            && bounds.1 >= viewport.1
            && bounds.2 <= viewport.2
            && bounds.3 <= viewport.3
        {
            Coverage::Inside
        } else {
            Coverage::Partial
        };
        match coverage {
            Coverage::Inside => reading.inside.push(node.id.clone()),
            Coverage::Partial => reading.partial.push(node.id.clone()),
            Coverage::Outside => {}
        }
    }
    reading
}

/// Drawn matter another shape can relate to: something with an area or words
/// of its own. Connectors relate things; they are not themselves related to,
/// and letting them be targets would report an arrow as grouped or struck
/// when the gesture was about whatever the arrow touches.
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

fn read_shape(atlas: &Atlas, shape: &Shape) -> Reading {
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

    // A model-derived segment is semantic matter in its own right. Its mask
    // is not a gesture about whatever happens to sit underneath it.
    if shape.form == "segment" {
        return reading("free", Vec::new(), Vec::new());
    }

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
        if matches!(shape.form.as_str(), "arrow" | "line") {
            if let Some(tip) = world.last() {
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
    }

    // A box that touches nothing is still worth reporting as a region.
    reading("free", Vec::new(), Vec::new())
}

/// Do two boxes overlap by more than `slop` on both axes?
fn overlaps(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64), slop: f64) -> bool {
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

// ── what changed since last time ──────────────────────────────────────────
//
// The read-back above describes the page as it *is*. That is the wrong shape
// for the question an agent actually has at the top of a turn, which is not
// "what is on the page" but "what did they do while I was thinking".
//
// A snapshot answers that only by making the reader diff two long documents
// from memory, and the thing they most need to notice — a shape that used to
// be decoration and now groups three cards — is a sentence that reads
// identically in both. So the difference is computed here and reported as the
// change it is, in the same vocabulary: relations, not coordinates.
//
// This is deliberately NOT part of [`Atlas::describe`]. "What you were last
// told" is not a property of the shared document — the browser has no business
// knowing it, and folding it in would make the two replicas' read-backs differ
// and break the "Do we agree?" check, which is the one thing on this surface
// that is supposed to be exactly comparable.

/// One object, reduced to what the agent was actually told about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeDigest {
    pub id: String,
    pub form: String,
    pub created_by: String,
    pub label: String,
    pub relation: String,
    pub targets: Vec<String>,
    pub partial: Vec<String>,
    /// Only consulted for a shape that relates to no node — see [`changes`].
    pub at: (f64, f64),
    /// Who wrote to this shape last, which is not who created it. Discounting
    /// a reader's own writes has to key on the last writer: a region the agent
    /// drew and the human then dragged over two cards is the human's edit, and
    /// keying on the creator would hide exactly that.
    pub touched_by: String,
    /// [`Shape::style_note`] at digest time. Dashing a box, fading it, or
    /// moving it into a frame is an authored change of meaning, so it has to
    /// show up in a delta the same way a rename does.
    pub style: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDigest {
    pub id: String,
    pub label: String,
    pub status: String,
    pub tone: String,
    pub x: f64,
    pub y: f64,
    /// Extent, because a move is only sayable in terms of what the card now
    /// sits beside, and "beside" needs the boxes rather than the corners.
    pub w: f64,
    pub h: f64,
    /// Who wrote to this node last. The change section attributes each line
    /// from this instead of asserting that everything it lists was the human.
    pub touched_by: String,
    /// Drawn regions that enclose this card, from the same read-back the agent
    /// is shown. A card crossing into or out of a lasso is the strongest thing
    /// a rearrangement can mean, and it is invisible in coordinates.
    pub enclosed_by: Vec<String>,
    /// The container this card is declared to be inside. Empty at the top
    /// level. Distinct from `enclosed_by`, which is geometry a drag can undo;
    /// this is the claim itself, and a change to it is its own sentence.
    #[serde(default)]
    pub parent: String,
    /// Containment depth at digest time. Stored because a reparent delta must
    /// say how the relation changed without reconstructing either old tree.
    #[serde(default)]
    pub depth: usize,
    /// The style register, carried whole so a restyle is comparable across two
    /// turns. A human muting a card, or an agent colouring one, is a claim
    /// about the picture and belongs in the change channel next to a status
    /// flip, not in the geometry the section deliberately refuses to print.
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub emphasis: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub kind: String,
    /// Typed diagram role for containers, when one exists.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub diagram_kind: String,
    /// Canonical `CURRENT STATE` sentence for a bound state machine.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state_read_back: String,
    /// Last writer of the bound variable that produced `state_read_back`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state_touched_by: String,
}

/// A link between two cards, in a form two turns apart can be compared.
///
/// Absent from the digest until now, which meant the whole relationship
/// register was invisible in the change channel: a human could draw a
/// dependency between two claims and the agent's next read said nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeDigest {
    pub id: String,
    pub from: String,
    pub to: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub guard: String,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub touched_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkDigest {
    pub id: String,
    pub target: String,
    pub glyph: String,
    pub text: String,
    pub answered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationDigest {
    pub id: String,
    pub current_beat: String,
    pub status: String,
    pub revision: u32,
    pub touched_by: String,
}

/// Everything the agent was told, in a form two turns apart can be compared.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Digest {
    #[serde(default)]
    pub proposals: Vec<architecture::Proposal>,
    #[serde(default)]
    pub context: context::ContextState,
    pub nodes: Vec<NodeDigest>,
    pub edges: Vec<EdgeDigest>,
    pub shapes: Vec<ShapeDigest>,
    pub marks: Vec<MarkDigest>,
    #[serde(default)]
    pub claims: Vec<ClaimDigest>,
    pub constraints: Vec<Constraint>,
    pub constraint_state: ConstraintState,
    #[serde(default)]
    pub explanation: Option<ExplanationDigest>,
    #[serde(default)]
    pub explanations: Vec<ExplanationDigest>,
    #[serde(default)]
    pub variables: Vec<agent_ink::VariableDigest>,
}

impl Atlas {
    /// Reduce the page to what a later turn will want to compare against.
    pub fn digest(&self) -> Digest {
        let readings = readings(self);
        Digest {
            proposals: self.proposals.clone(),
            context: self.context.clone(),
            explanations: self
                .explanations
                .iter()
                .map(|flow| ExplanationDigest {
                    id: flow.definition.id.clone(),
                    current_beat: flow.state.current_beat.clone(),
                    status: flow.state.status.clone(),
                    revision: flow.state.revision,
                    touched_by: flow.state.touched_by.clone(),
                })
                .collect(),
            nodes: self
                .nodes
                .iter()
                .map(|node| {
                    let state_read_back =
                        living_ink::current_state_sentence(self, node).unwrap_or_default();
                    let state_touched_by = self
                        .variables
                        .iter()
                        .find(|variable| variable.name == node.state_variable)
                        .map_or_else(String::new, |variable| variable.touched_by.clone());
                    NodeDigest {
                        id: node.id.clone(),
                        label: node.label.clone(),
                        status: node.status.clone(),
                        tone: node.tone.clone(),
                        x: node.x,
                        y: node.y,
                        w: node.w,
                        h: node.h,
                        touched_by: node.touched_by.clone(),
                        enclosed_by: readings
                            .iter()
                            .filter(|reading| {
                                reading.relation == "encloses" && reading.targets.contains(&node.id)
                            })
                            .map(|reading| reading.shape.clone())
                            .collect(),
                        parent: node.parent.clone(),
                        depth: self.depth(&node.id),
                        color: node.color.clone(),
                        emphasis: node.emphasis.clone(),
                        size: node.size.clone(),
                        kind: node.kind.clone(),
                        diagram_kind: node.diagram_kind.clone(),
                        state_read_back,
                        state_touched_by,
                    }
                })
                .collect(),
            edges: self
                .edges
                .iter()
                .map(|edge| EdgeDigest {
                    id: edge.id.clone(),
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    label: edge.label.clone(),
                    event: edge.event.clone(),
                    guard: edge.guard.clone(),
                    created_by: edge.created_by.clone(),
                    touched_by: edge.touched_by.clone(),
                })
                .collect(),
            shapes: self
                .shapes
                .iter()
                .map(|shape| {
                    let reading = readings.iter().find(|r| r.shape == shape.id);
                    let (l, t, _, _) = shape.bounds();
                    ShapeDigest {
                        id: shape.id.clone(),
                        form: shape.form.clone(),
                        created_by: shape.created_by.clone(),
                        label: shape.label.clone(),
                        relation: reading.map(|r| r.relation.clone()).unwrap_or_default(),
                        targets: reading.map(|r| r.targets.clone()).unwrap_or_default(),
                        partial: reading.map(|r| r.partial.clone()).unwrap_or_default(),
                        at: (l, t),
                        touched_by: shape.touched_by.clone(),
                        style: shape.style_note(self),
                    }
                })
                .collect(),
            marks: self
                .marks
                .iter()
                .map(|mark| MarkDigest {
                    id: mark.id.clone(),
                    target: mark.target.clone(),
                    glyph: mark.glyph.clone(),
                    text: mark.text.clone(),
                    answered: !mark.answer.is_empty(),
                })
                .collect(),
            claims: claims::digest(self),
            constraints: self.constraints.clone(),
            constraint_state: self.constraint_state.clone(),
            explanation: self.explanation.as_ref().map(|flow| ExplanationDigest {
                id: flow.definition.id.clone(),
                current_beat: flow.state.current_beat.clone(),
                status: flow.state.status.clone(),
                revision: flow.state.revision,
                touched_by: flow.state.touched_by.clone(),
            }),
            variables: agent_ink::digest_variables(&self.variables),
        }
    }
}

/// What a relation MEANS, as the thing a person was doing.
///
/// The relation name says what the geometry is; this says why somebody drew
/// it. They are different questions and the second one is the one worth acting
/// on, so both are reported rather than the reader being left to infer one.
fn intent_of(relation: &str) -> &'static str {
    match relation {
        "encloses" => "grouping them",
        "crosses-out" => "striking it through",
        "points-at" => "singling it out",
        "starts-at" => "pointing away from it",
        "connects" => "relating them",
        "says" => "labelling it",
        _ => "decoration",
    }
}

/// A movement below this is a nudge, not news.
const MOVED_ENOUGH: f64 = 24.0;
/// How far apart two cards can sit and still be described in terms of each
/// other. Wider than a column gap so a deliberate row survives being nudged,
/// narrower than the page so "beside" keeps meaning something.
const BESIDE_GAP: f64 = 340.0;
/// At most this many neighbours per card. A rearrangement described against
/// six things is a coordinate dump wearing a sentence.
const BESIDE_MAX: usize = 2;

/// Where a card sits, said in terms of the cards around it.
///
/// This is the node register's answer to what [`readings`] does for ink, and
/// its absence was the register split's real cost: a human rearranging cards
/// is *saying* something — these belong together, this one comes first — and
/// the agent was told a pixel count. Coordinates survive only where they do
/// for shapes: when the card sits near nothing, so there is nothing to say it
/// in terms of.
fn beside(node: &NodeDigest, all: &[NodeDigest]) -> Vec<String> {
    let (l, t, r, b) = (node.x, node.y, node.x + node.w, node.y + node.h);
    let mut found: Vec<(f64, String)> = Vec::new();
    for other in all {
        if other.id == node.id {
            continue;
        }
        let (ol, ot, or, ob) = (other.x, other.y, other.x + other.w, other.y + other.h);
        let overlaps_x = ol < r && or > l;
        let overlaps_y = ot < b && ob > t;
        // A card is only "under" something it shares a column with, and only
        // "right of" something it shares a row with. Diagonal neighbours are
        // left out on purpose: they are the ones a reader would picture wrong.
        let (gap, word) = if overlaps_x && ob <= t {
            (t - ob, "under")
        } else if overlaps_x && ot >= b {
            (ot - b, "above")
        } else if overlaps_y && or <= l {
            (l - or, "right of")
        } else if overlaps_y && ol >= r {
            (ol - r, "left of")
        } else {
            continue;
        };
        if gap <= BESIDE_GAP {
            found.push((gap, format!("{word} \"{}\"", other.label)));
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.truncate(BESIDE_MAX);
    found.into_iter().map(|(_, text)| text).collect()
}

/// Cards sharing real area with this one, as a spatial relation rather than a
/// nearest-neighbour fallback. A human dragging one card onto another is a
/// claim about the pair, so overlap takes priority over `beside` in the delta.
fn overlapping(node: &NodeDigest, all: &[NodeDigest]) -> Vec<String> {
    let bounds = (node.x, node.y, node.x + node.w, node.y + node.h);
    let mut found = all
        .iter()
        .filter(|other| other.id != node.id)
        // A card sharing area with its own container is the containment
        // working, not a collision. Reporting it would turn every hierarchy
        // into a page of overlap warnings the reader has to learn to ignore.
        .filter(|other| !nested(node, other, all))
        .filter(|other| {
            overlaps(
                bounds,
                (other.x, other.y, other.x + other.w, other.y + other.h),
                OVERLAP_SLOP,
            )
        })
        .map(|other| format!("\"{}\"", other.label))
        .collect::<Vec<_>>();
    found.sort();
    found
}

/// Whether one of these two digested cards is inside the other, at any depth.
///
/// The digest carries `parent` per node and nothing else about the tree, so
/// the walk happens here. It terminates for the same reason [`Atlas::ancestors`]
/// does: [`read`] has already healed containment into a forest, and a digest
/// is taken from a projected atlas.
fn nested(node: &NodeDigest, other: &NodeDigest, all: &[NodeDigest]) -> bool {
    let inside = |start: &NodeDigest, target: &str| {
        let mut at = start.parent.clone();
        let mut steps = 0;
        while !at.is_empty() && steps <= all.len() {
            if at == target {
                return true;
            }
            at = all
                .iter()
                .find(|candidate| candidate.id == at)
                .map(|candidate| candidate.parent.clone())
                .unwrap_or_default();
            steps += 1;
        }
        false
    };
    inside(node, &other.id) || inside(other, &node.id)
}

/// Beyond this many lines the section is a wall of text rather than a summary.
const MAX_CHANGES: usize = 24;
/// What happened to the page between two digests, as sentences.
///
/// Ordered by how much a reader needs it: what the *drawing* now says first,
/// because that is the human's fastest gesture and the easiest to miss;
/// then claims changing status; then the merely geometric.
pub fn changes(before: &Digest, after: &Digest) -> Vec<String> {
    let label_of = |id: &str| -> String {
        if let Some(node) = after
            .nodes
            .iter()
            .find(|node| node.id == id)
            .or_else(|| before.nodes.iter().find(|node| node.id == id))
        {
            return format!("\"{}\"", node.label);
        }
        after
            .shapes
            .iter()
            .find(|shape| shape.id == id)
            .or_else(|| before.shapes.iter().find(|shape| shape.id == id))
            .map(|shape| {
                if shape.label.is_empty() {
                    format!("\"{} {id}\"", shape.form)
                } else {
                    format!("\"{}\"", shape.label)
                }
            })
            .or_else(|| {
                after
                    .constraints
                    .iter()
                    .find(|constraint| constraint.id == id)
                    .or_else(|| {
                        before
                            .constraints
                            .iter()
                            .find(|constraint| constraint.id == id)
                    })
                    .map(Constraint::statement)
            })
            .or_else(|| claims::target_label(before, after, id))
            .unwrap_or_else(|| format!("<gone {id}>"))
    };
    let list = |ids: &[String]| {
        ids.iter()
            .map(|id| label_of(id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // What a shape is saying, in one clause.
    //
    // `positioned` is false for the "it *was* …" half of a change. A position
    // the shape has already left is the one fact on that line nobody can act
    // on, and printing it buries the half of the sentence that is the news.
    // Coordinates survive in exactly one place — a shape that relates to no
    // node and never did — because there they are the only thing to say.
    let says = |shape: &ShapeDigest, positioned: bool| -> String {
        if shape.targets.is_empty() {
            if positioned {
                format!(
                    "touching no node — at ({:.0},{:.0})",
                    shape.at.0, shape.at.1
                )
            } else {
                "touching no node".to_string()
            }
        } else if shape.relation == "connects" && shape.targets.len() == 2 {
            format!(
                "CONNECTS {} -> {}",
                label_of(&shape.targets[0]),
                label_of(&shape.targets[1])
            )
        } else {
            format!("{} {}", shape.relation.to_uppercase(), list(&shape.targets))
        }
    };
    let named = |shape: &ShapeDigest| {
        if shape.label.is_empty() {
            format!("{}'s {}", shape.created_by, shape.form)
        } else {
            format!("{}'s {} \"{}\"", shape.created_by, shape.form, shape.label)
        }
    };

    let mut drawn = Vec::new();
    let mut constraint_changes = Vec::new();
    let mut claims = Vec::new();
    let mut moved = Vec::new();
    for proposal in &after.proposals {
        if !before.proposals.iter().any(|old| old.id == proposal.id) {
            claims.push(format!("- NEW ARCHITECTURE PROPOSAL [{}] {:?} by {}. Current map unchanged; use Compare or atlas_review to inspect relationships and impact.", proposal.id, proposal.definition.title, proposal.created_by));
        }
    }
    if before.context != after.context {
        claims.push(format!(
            "- SHARED SCOPE CHANGED.{}",
            after.context.describe()
        ));
    }

    let old_flows: std::collections::BTreeMap<_, _> = before
        .explanations
        .iter()
        .chain(before.explanation.iter())
        .map(|flow| (&flow.id, flow))
        .collect();
    let new_flows: std::collections::BTreeMap<_, _> = after
        .explanations
        .iter()
        .chain(after.explanation.iter())
        .map(|flow| (&flow.id, flow))
        .collect();
    let flow_ids: std::collections::BTreeSet<_> =
        old_flows.keys().chain(new_flows.keys()).copied().collect();
    for id in flow_ids {
        match (old_flows.get(id), new_flows.get(id)) {
        (None, Some(flow)) => claims.push(format!(
            "- NEW EXPLANATION [{}] started at beat {} status={} revision={}",
            flow.id, flow.current_beat, flow.status, flow.revision
        )),
        (Some(old), Some(flow)) if old != flow => claims.push(format!(
            "- EXPLANATION [{}] is now beat {} status={} revision={} by {}; it was beat {} status={} revision={}",
            flow.id,
            flow.current_beat,
            flow.status,
            flow.revision,
            flow.touched_by,
            old.current_beat,
            old.status,
            old.revision,
        )),
        (Some(old), None) => claims.push(format!(
            "- GONE EXPLANATION [{}] was removed at beat {}",
            old.id, old.current_beat
        )),
        _ => {}
    }
    }

    // A constraint states itself in member ids, because the struct has no way
    // to look a label up. This section does, and a line naming four hex ids is
    // unreadable to whichever of the two readers did not author them.
    let named_constraint = |constraint: &Constraint| -> String {
        let members = constraint
            .members
            .iter()
            .map(|id| label_of(id))
            .collect::<Vec<_>>();
        let joined = match constraint.op.as_str() {
            "sequence" => members.join(" -> "),
            "attaches" => members.join(" <-> "),
            _ => members.join(", "),
        };
        match constraint.op.as_str() {
            "sequence" => format!(
                "sequence[{}]: {joined}",
                constraint.axis.as_deref().unwrap_or("x")
            ),
            "labels" => format!(
                "labels {joined} {:?}",
                constraint.text.as_deref().unwrap_or("")
            ),
            other => format!("{other}: {joined}"),
        }
    };

    for constraint in &after.constraints {
        match before
            .constraints
            .iter()
            .find(|old| old.id == constraint.id)
        {
            None => constraint_changes.push(format!(
                "- NEW CONSTRAINT [{}] {} ({})",
                constraint.id,
                named_constraint(constraint),
                constraint.status.to_uppercase()
            )),
            Some(old) if old.status != constraint.status => constraint_changes.push(format!(
                "- CONSTRAINT [{}] is now {} and was {}: {}",
                constraint.id,
                constraint.status.to_uppercase(),
                old.status.to_uppercase(),
                named_constraint(constraint)
            )),
            Some(_) => {}
        }
    }
    for old in &before.constraints {
        if !after
            .constraints
            .iter()
            .any(|constraint| constraint.id == old.id)
        {
            constraint_changes.push(format!(
                "- GONE CONSTRAINT [{}] {}",
                old.id,
                named_constraint(old)
            ));
        }
    }
    if before.constraint_state != after.constraint_state {
        constraint_changes.push(if after.constraint_state.status == "unsat" {
            format!(
                "- CONSTRAINT STATE is now UNSAT; minimal core: {}",
                after
                    .constraint_state
                    .core
                    .iter()
                    .map(|id| after
                        .constraints
                        .iter()
                        .find(|constraint| constraint.id == *id)
                        .map(&named_constraint)
                        .unwrap_or_else(|| id.clone()))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        } else {
            "- CONSTRAINT STATE is now SAT; no sequence cycle remains".to_string()
        });
    }

    for shape in &after.shapes {
        match before.shapes.iter().find(|old| old.id == shape.id) {
            None => drawn.push(format!(
                "- NEW [{}] {} — {} — {} is {}",
                shape.id,
                named(shape),
                says(shape, true),
                shape.created_by,
                intent_of(&shape.relation)
            )),
            Some(old) => {
                // The change worth reporting is what it now SAYS. A shape that
                // moved 200px and still groups the same two cards has not
                // changed its meaning, and reporting the move as news buries
                // the one that did.
                if old.relation != shape.relation || old.targets != shape.targets {
                    drawn.push(format!(
                        "- [{}] {} now {} — it was {} — {} is {}",
                        shape.id,
                        named(shape),
                        says(shape, false),
                        says(old, false),
                        shape.created_by,
                        intent_of(&shape.relation)
                    ));
                } else if old.partial != shape.partial {
                    drawn.push(if shape.partial.is_empty() {
                        format!(
                            "- [{}] {} no longer half-covers anything — it was ambiguous about {}",
                            shape.id,
                            named(shape),
                            list(&old.partial)
                        )
                    } else {
                        format!(
                            "- [{}] {} now only partly covers {} — neither in nor out",
                            shape.id,
                            named(shape),
                            list(&shape.partial)
                        )
                    });
                } else if old.label != shape.label {
                    drawn.push(format!(
                        "- [{}] {} was renamed — it was \"{}\"",
                        shape.id,
                        named(shape),
                        old.label
                    ));
                } else if old.style != shape.style {
                    let plain = |style: &str| {
                        if style.is_empty() {
                            "plain".to_string()
                        } else {
                            style.trim().to_string()
                        }
                    };
                    drawn.push(format!(
                        "- [{}] {} was restyled: now {} — it was {}",
                        shape.id,
                        named(shape),
                        plain(&shape.style),
                        plain(&old.style)
                    ));
                } else if shape.targets.is_empty() && distance(old.at, shape.at) > MOVED_ENOUGH {
                    // The one case coordinates are the best answer available:
                    // it relates to nothing, so there is nothing to say it in
                    // terms of.
                    moved.push(format!(
                        "- [{}] {} moved to ({:.0},{:.0}) and still touches no node",
                        shape.id,
                        named(shape),
                        shape.at.0,
                        shape.at.1
                    ));
                }
            }
        }
    }
    for old in &before.shapes {
        if !after.shapes.iter().any(|shape| shape.id == old.id) {
            drawn.push(format!(
                "- GONE [{}] {} — it was {}",
                old.id,
                named(old),
                says(old, false)
            ));
        }
    }

    for node in &after.nodes {
        match before.nodes.iter().find(|old| old.id == node.id) {
            None => claims.push(if node.parent.is_empty() {
                format!(
                    "- NEW node [{}] \"{}\" ({})",
                    node.id, node.label, node.status
                )
            } else {
                format!(
                    "- NEW node [{}] \"{}\" ({}) inside {}",
                    node.id,
                    node.label,
                    node.status,
                    label_of(&node.parent)
                )
            }),
            Some(old) => {
                if old.state_read_back != node.state_read_back {
                    claims.push(match (
                        old.state_read_back.is_empty(),
                        node.state_read_back.is_empty(),
                    ) {
                        (true, false) => format!(
                            "- [{}] state machine {:?} gained a current-state relation: {}",
                            node.id, node.label, node.state_read_back
                        ),
                        (false, true) => format!(
                            "- [{}] state machine {:?} lost its current-state relation; it was {}",
                            node.id, node.label, old.state_read_back
                        ),
                        (false, false) => format!(
                            "- [{}] state machine {:?} current-state relation changed to {}; it was {}",
                            node.id, node.label, node.state_read_back, old.state_read_back
                        ),
                        (true, true) => unreachable!("equal empty state read-backs do not change"),
                    });
                }
                // Containment first, and instead of the positional section
                // below. A card that moved because it was put inside something
                // has one thing to say, and saying it twice — once as a
                // structure change and once as new neighbours — buries which
                // of the two the reader is supposed to act on.
                if old.parent != node.parent {
                    let by = if node.touched_by.is_empty() {
                        "somebody".to_string()
                    } else {
                        node.touched_by.clone()
                    };
                    let depth_change = if old.depth == node.depth {
                        String::new()
                    } else {
                        let level = if node.depth == 1 { "level" } else { "levels" };
                        format!(
                            ", and it is now {} {level} deep, was {}",
                            node.depth, old.depth,
                        )
                    };
                    let gone = !old.parent.is_empty()
                        && !after.nodes.iter().any(|other| other.id == old.parent);
                    let old_machine = before.nodes.iter().find(|parent| {
                        parent.id == old.parent && parent.diagram_kind == "state_machine"
                    });
                    let new_machine = after.nodes.iter().find(|parent| {
                        parent.id == node.parent && parent.diagram_kind == "state_machine"
                    });
                    if !gone && (old_machine.is_some() || new_machine.is_some()) {
                        let parent_of = |id: &str| {
                            after
                                .nodes
                                .iter()
                                .find(|candidate| candidate.id == id)
                                .map_or("", |candidate| candidate.parent.as_str())
                        };
                        let crossing = after
                            .edges
                            .iter()
                            .filter(|edge| {
                                !edge.event.is_empty()
                                    && (edge.from == node.id || edge.to == node.id)
                                    && parent_of(&edge.from) != parent_of(&edge.to)
                            })
                            .count();
                        let crossing = match crossing {
                            0 => String::new(),
                            1 => ", and its 1 transition now crosses between machines".to_string(),
                            count => {
                                format!(", and its {count} transitions now cross between machines")
                            }
                        };
                        claims.push(match (old_machine, new_machine) {
                            (Some(old_machine), Some(new_machine)) => format!(
                                "- [{}] {:?} was moved into the machine {:?} by {by}; it was inside {:?} before{crossing}",
                                node.id,
                                node.label,
                                new_machine.label,
                                old_machine.label
                            ),
                            (None, Some(new_machine)) => format!(
                                "- [{}] {:?} was moved into the machine {:?} by {by}; it was outside a state machine before{crossing}",
                                node.id, node.label, new_machine.label
                            ),
                            (Some(old_machine), None) => format!(
                                "- [{}] {:?} was moved out of the machine {:?} by {by}; it is outside a state machine now{crossing}",
                                node.id, node.label, old_machine.label
                            ),
                            (None, None) => format!(
                                "- [{}] {:?} changed containers by {by}",
                                node.id, node.label
                            ),
                        });
                    } else {
                        claims.push(match (old.parent.is_empty(), node.parent.is_empty(), gone) {
                        // The orphan disposition: the container went away and
                        // this is where its contents landed. Stated as a
                        // consequence of the delete, because the reader who
                        // deleted it is owed the answer to "what happened to
                        // what was inside".
                        (_, true, true) => format!(
                            "- [{}] \"{}\" moved up to the top level after its container {} was deleted{depth_change}",
                            node.id,
                            node.label,
                            label_of(&old.parent)
                        ),
                        (_, false, true) => format!(
                            "- [{}] \"{}\" moved up into {} after its container {} was deleted{depth_change}",
                            node.id,
                            node.label,
                            label_of(&node.parent),
                            label_of(&old.parent)
                        ),
                        (true, false, false) => format!(
                            "- [{}] \"{}\" was put inside {} by {by}; it was at the top level before{depth_change}",
                            node.id,
                            node.label,
                            label_of(&node.parent)
                        ),
                        (false, true, false) => format!(
                            "- [{}] \"{}\" was taken out of {} by {by} and is now at the top level{depth_change}",
                            node.id,
                            node.label,
                            label_of(&old.parent)
                        ),
                        (false, false, false) => format!(
                            "- [{}] \"{}\" was moved into {} by {by}; it was inside {} before{depth_change}",
                            node.id,
                            node.label,
                            label_of(&node.parent),
                            label_of(&old.parent)
                        ),
                        (true, true, _) => unreachable!("parent changed but both ends are empty"),
                        });
                    }
                }
                if old.status != node.status {
                    claims.push(format!(
                        "- [{}] \"{}\" is now {} — it was {}",
                        node.id, node.label, node.status, old.status
                    ));
                }
                if old.label != node.label {
                    claims.push(format!(
                        "- [{}] renamed to \"{}\" — it was \"{}\"",
                        node.id, node.label, old.label
                    ));
                }
                if old.tone != node.tone {
                    claims.push(format!(
                        "- [{}] \"{}\" is now a {} — it was a {}",
                        node.id, node.label, node.tone, old.tone
                    ));
                }
                // Restyling is a claim about the picture, so it reads next to
                // a status flip rather than as geometry. What the form MEANS
                // stays whoever authored it to say: these lines report that
                // the weight or the colour changed and who changed it, and
                // never that amber is now risk.
                let restyled_by = if node.touched_by.is_empty() {
                    "somebody".to_string()
                } else {
                    node.touched_by.clone()
                };
                if old.emphasis != node.emphasis {
                    claims.push(if node.emphasis == DEFAULT_EMPHASIS {
                        format!(
                            "- [{}] \"{}\" is no longer emphasised by {restyled_by}; it was {} before",
                            node.id, node.label, old.emphasis
                        )
                    } else {
                        format!(
                            "- [{}] \"{}\" is emphasised {} by {restyled_by}; it was {} before",
                            node.id, node.label, node.emphasis, old.emphasis
                        )
                    });
                }
                if old.size != node.size {
                    claims.push(format!(
                        "- [{}] \"{}\" is now {} sized by {restyled_by}; it was {} before",
                        node.id, node.label, node.size, old.size
                    ));
                }
                if old.color != node.color {
                    claims.push(match (old.color.is_empty(), node.color.is_empty()) {
                        (_, true) => format!(
                            "- [{}] \"{}\" lost its {} colour, set by {restyled_by}",
                            node.id, node.label, old.color
                        ),
                        (true, false) => format!(
                            "- [{}] \"{}\" is now coloured {} by {restyled_by}; it had no colour before",
                            node.id, node.label, node.color
                        ),
                        (false, false) => format!(
                            "- [{}] \"{}\" is now coloured {} by {restyled_by}; it was {} before",
                            node.id, node.label, node.color, old.color
                        ),
                    });
                }
                if old.kind != node.kind {
                    claims.push(format!(
                        "- [{}] \"{}\" is now drawn as a {} by {restyled_by}; it was a {} before",
                        node.id, node.label, node.kind, old.kind
                    ));
                }
                if old.parent != node.parent {
                    continue;
                }
                let moved_enough = distance((old.x, old.y), (node.x, node.y)) > MOVED_ENOUGH;
                let now_overlaps = overlapping(node, &after.nodes);
                let was_overlaps = overlapping(old, &before.nodes);
                if moved_enough && node.touched_by == "human" && now_overlaps != was_overlaps {
                    moved.push(if now_overlaps.is_empty() {
                        format!(
                            "- [{}] \"{}\" no longer overlaps {}. The human pulled it out of that relation",
                            node.id,
                            node.label,
                            was_overlaps.join(" and ")
                        )
                    } else if was_overlaps.is_empty() {
                        format!(
                            "- [{}] \"{}\" now OVERLAPS {}. The human moved it into that relation",
                            node.id,
                            node.label,
                            now_overlaps.join(" and ")
                        )
                    } else {
                        format!(
                            "- [{}] \"{}\" now OVERLAPS {}. It previously overlapped {}",
                            node.id,
                            node.label,
                            now_overlaps.join(" and "),
                            was_overlaps.join(" and ")
                        )
                    });
                    continue;
                }
                // A card crossing into or out of a drawn region is the loudest
                // thing a rearrangement can say, so it is reported ahead of
                // the neighbours and instead of them.
                let entered: Vec<&String> = node
                    .enclosed_by
                    .iter()
                    .filter(|id| !old.enclosed_by.contains(id))
                    .collect();
                let left: Vec<&String> = old
                    .enclosed_by
                    .iter()
                    .filter(|id| !node.enclosed_by.contains(id))
                    .collect();
                if !entered.is_empty() || !left.is_empty() {
                    let mut said = Vec::new();
                    if !entered.is_empty() {
                        said.push(format!(
                            "into {}",
                            entered
                                .iter()
                                .map(|id| label_of(id))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    if !left.is_empty() {
                        said.push(format!(
                            "out of {}",
                            left.iter()
                                .map(|id| label_of(id))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    moved.push(format!(
                        "- [{}] \"{}\" was moved {}",
                        node.id,
                        node.label,
                        said.join(" and ")
                    ));
                } else if distance((old.x, old.y), (node.x, node.y)) > MOVED_ENOUGH {
                    let now = beside(node, &after.nodes);
                    let was = beside(old, &before.nodes);
                    // A card that moved and still sits with the same
                    // neighbours has not changed the arrangement, and saying
                    // so buries the moves that did — the same judgement the
                    // drawn register already makes about shapes. The one
                    // exception is a card that sat apart and still does:
                    // there is no arrangement to compare, so the move itself
                    // is the whole of the news.
                    let alone = now.is_empty() && was.is_empty();
                    if now != was || alone {
                        moved.push(match (now.is_empty(), was.is_empty()) {
                            (true, true) => format!(
                                "- [{}] \"{}\" moved to ({:.0},{:.0}) and still sits apart from everything",
                                node.id, node.label, node.x, node.y
                            ),
                            (true, false) => format!(
                                "- [{}] \"{}\" was pulled out on its own — it was {}",
                                node.id,
                                node.label,
                                was.join(" and ")
                            ),
                            (false, true) => format!(
                                "- [{}] \"{}\" was brought in {} — it sat apart before",
                                node.id,
                                node.label,
                                now.join(" and ")
                            ),
                            (false, false) => format!(
                                "- [{}] \"{}\" is now {} — it was {}",
                                node.id,
                                node.label,
                                now.join(" and "),
                                was.join(" and ")
                            ),
                        });
                    }
                }
            }
        }
    }
    for old in &before.nodes {
        if !after.nodes.iter().any(|node| node.id == old.id) {
            claims.push(format!("- GONE node [{}] \"{}\"", old.id, old.label));
        }
    }

    // Links. A relationship between two claims is a claim in its own right,
    // and until now the change channel could not express one at all.
    let transition_said = |edge: &EdgeDigest| {
        let guard = if edge.guard.is_empty() {
            String::new()
        } else {
            format!(" when {}", edge.guard)
        };
        format!(
            "from {} on {}{guard} -> {}",
            label_of(&edge.from),
            edge.event,
            label_of(&edge.to)
        )
    };
    for edge in &after.edges {
        let said = |edge: &EdgeDigest| {
            if edge.label.is_empty() {
                format!("{} -> {}", label_of(&edge.from), label_of(&edge.to))
            } else {
                format!(
                    "{} {} {}",
                    label_of(&edge.from),
                    edge.label,
                    label_of(&edge.to)
                )
            }
        };
        match before.edges.iter().find(|old| old.id == edge.id) {
            None if !edge.event.is_empty() => claims.push(format!(
                "- NEW TRANSITION [{}] {}; {} drew the relationship",
                edge.id,
                transition_said(edge),
                edge.created_by
            )),
            None => claims.push(format!(
                "- NEW LINK [{}] {} — {} drew the relationship",
                edge.id,
                said(edge),
                edge.created_by
            )),
            Some(old)
                if !edge.event.is_empty()
                    && (old.from != edge.from
                        || old.to != edge.to
                        || old.event != edge.event
                        || old.guard != edge.guard) =>
            {
                claims.push(format!(
                    "- [{}] the transition now reads {}; it was {}",
                    edge.id,
                    transition_said(edge),
                    transition_said(old)
                ));
            }
            Some(old) if old.label != edge.label => claims.push(format!(
                "- [{}] the link now reads {} — it was {}",
                edge.id,
                said(edge),
                said(old)
            )),
            Some(_) => {}
        }
    }
    for old in &before.edges {
        if !after.edges.iter().any(|edge| edge.id == old.id) {
            claims.push(if old.event.is_empty() {
                format!(
                    "- GONE LINK [{}] {} -> {} is no longer claimed",
                    old.id,
                    label_of(&old.from),
                    label_of(&old.to)
                )
            } else {
                format!(
                    "- GONE TRANSITION [{}] {} is no longer claimed",
                    old.id,
                    transition_said(old)
                )
            });
        }
    }

    for mark in &after.marks {
        let meaning = match mark.glyph.as_str() {
            "?" => "does not follow it",
            "!" => "thinks it is wrong",
            _ => "says it matters",
        };
        match before.marks.iter().find(|old| old.id == mark.id) {
            None => claims.push(format!(
                "- NEW MARK [{}] {} on {}; the human {meaning}: \"{}\"",
                mark.id,
                mark.glyph,
                label_of(&mark.target),
                mark.text
            )),
            Some(old) if old.answered && !mark.answered => claims.push(format!(
                "- [{}] your answer on {} was cleared",
                mark.id,
                label_of(&mark.target)
            )),
            // The human rewording an objection is them restating what they
            // meant, which is exactly the thing worth re-reading.
            Some(old) if old.text != mark.text => claims.push(format!(
                "- [{}] the {} on {} was reworded to \"{}\" — it was \"{}\"",
                mark.id,
                mark.glyph,
                label_of(&mark.target),
                mark.text,
                old.text
            )),
            Some(_) => {}
        }
    }
    for old in &before.marks {
        if !after.marks.iter().any(|mark| mark.id == old.id) {
            claims.push(format!(
                "- GONE MARK [{}] {} on {} was withdrawn",
                old.id,
                old.glyph,
                label_of(&old.target)
            ));
        }
    }

    let mut out = agent_ink::describe_changes(&before.variables, &after.variables);
    out.extend(constraint_changes);
    out.extend(drawn);
    out.extend(claims);
    out.extend(claims::changes(before, after));
    out.extend(moved);
    // Never silently truncated: a section that stops early while reading like
    // a complete list is worse than a long one.
    if out.len() > MAX_CHANGES {
        let dropped = out.len() - MAX_CHANGES;
        out.truncate(MAX_CHANGES);
        out.push(format!(
            "- …and {dropped} more change(s) not listed — read the sections above for the full picture"
        ));
    }
    out
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// The same section, with the reader's own writes already accounted for.
///
/// An agent that draws twelve cards and then reads was being handed those
/// twelve cards back as news, because its own writes never advanced what it
/// had been told. Refreshing the whole baseline would fix that by also
/// swallowing anything the human did in the same window, which is the more
/// expensive mistake. So the baseline is advanced per object instead: only
/// objects whose latest write is the reader's own are treated as already
/// seen. A card the reader created and the *human* then moved still reports,
/// because the last write to it was not the reader's.
pub fn describe_changes_for(before: &Digest, after: &Digest, reader: &str) -> String {
    describe_changes(&already_seen(before, after, reader), after)
}

/// [`changes`], with the reader's own writes already accounted for.
///
/// The browser calls this so the human's change list is computed by the same
/// code that computes the agent's, rather than by a second implementation that
/// would drift from it.
pub fn changes_for(before: &Digest, after: &Digest, reader: &str) -> Vec<String> {
    changes(&already_seen(before, after, reader), after)
}

/// `before`, with the reader's own objects advanced to their current state.
fn already_seen(before: &Digest, after: &Digest, reader: &str) -> Digest {
    if reader.is_empty() {
        return before.clone();
    }
    let mut baseline = before.clone();
    let mine = |byline: &str| byline == reader;
    for proposal in after.proposals.iter().filter(|p| mine(&p.created_by)) {
        if !baseline.proposals.iter().any(|p| p.id == proposal.id) {
            baseline.proposals.push(proposal.clone());
        }
    }
    let new_contexts = after
        .context
        .entries
        .iter()
        .filter(|entry| !before.context.entries.iter().any(|old| old.id == entry.id))
        .collect::<Vec<_>>();
    if !new_contexts.is_empty()
        && new_contexts.iter().all(|entry| mine(&entry.created_by))
        && before
            .context
            .entries
            .iter()
            .all(|old| after.context.entries.iter().any(|entry| entry.id == old.id))
    {
        // The person who just changed scope already saw that change in the
        // scope bar. Preserve alerts for any unseen change by somebody else.
        baseline.context = after.context.clone();
    }

    for node in after.nodes.iter().filter(|node| mine(&node.touched_by)) {
        match baseline.nodes.iter_mut().find(|old| old.id == node.id) {
            Some(old) => {
                // Containment that changed because a container was deleted is
                // a consequence, not a write, and it survives the discount for
                // the same reason a constraint's derived status does. The
                // reader who deleted the container is exactly the reader who
                // needs telling where its contents went; folding this in
                // because the member happens to carry that reader's byline
                // would answer "what did I just orphan" with silence.
                let orphaned = old.parent != node.parent
                    && !old.parent.is_empty()
                    && !after.nodes.iter().any(|now| now.id == old.parent);
                let was = std::mem::take(&mut old.parent);
                let was_depth = old.depth;
                let state_read_back = std::mem::take(&mut old.state_read_back);
                let state_touched_by = std::mem::take(&mut old.state_touched_by);
                let preserve_state_relation = !mine(&node.state_touched_by);
                *old = node.clone();
                if orphaned {
                    old.parent = was;
                    old.depth = was_depth;
                }
                if preserve_state_relation {
                    old.state_read_back = state_read_back;
                    old.state_touched_by = state_touched_by;
                }
            }
            None => baseline.nodes.push(node.clone()),
        }
    }
    for node in after
        .nodes
        .iter()
        .filter(|node| mine(&node.state_touched_by))
    {
        if let Some(old) = baseline.nodes.iter_mut().find(|old| old.id == node.id) {
            old.state_read_back.clone_from(&node.state_read_back);
            old.state_touched_by.clone_from(&node.state_touched_by);
        }
    }
    for edge in after.edges.iter().filter(|edge| {
        mine(if edge.touched_by.is_empty() {
            &edge.created_by
        } else {
            &edge.touched_by
        })
    }) {
        match baseline.edges.iter_mut().find(|old| old.id == edge.id) {
            Some(old) => *old = edge.clone(),
            None => baseline.edges.push(edge.clone()),
        }
    }
    for shape in after.shapes.iter().filter(|shape| mine(&shape.touched_by)) {
        match baseline.shapes.iter_mut().find(|old| old.id == shape.id) {
            Some(old) => *old = shape.clone(),
            None => baseline.shapes.push(shape.clone()),
        }
    }
    // Declared Agent Ink values are authored shared state just like nodes
    // and shapes. Measured variables remain observations: the browser that
    // refreshed one did not author the geometry it measured.
    for variable in after
        .variables
        .iter()
        .filter(|variable| variable.state != VarState::Measured && mine(&variable.touched_by))
    {
        match baseline
            .variables
            .iter_mut()
            .find(|old| old.name == variable.name)
        {
            Some(old) => *old = variable.clone(),
            None => baseline.variables.push(variable.clone()),
        }
    }
    // Constraints are the one kind whose reported change is not always
    // authored by anybody: `status` is derived, so a constraint the reader
    // declared can be broken by somebody else deleting a member. Creating and
    // removing my own is not news to me, because the tool already confirmed
    // it. A status flip always is, whoever declared it — so an existing
    // constraint is never overwritten in the baseline.
    for constraint in after
        .constraints
        .iter()
        .filter(|constraint| mine(&constraint.created_by))
    {
        if !baseline
            .constraints
            .iter()
            .any(|old| old.id == constraint.id)
        {
            baseline.constraints.push(constraint.clone());
        }
    }
    baseline.constraints.retain(|old| {
        !mine(&old.created_by) || after.constraints.iter().any(|now| now.id == old.id)
    });
    if after
        .explanation
        .as_ref()
        .is_some_and(|flow| mine(&flow.touched_by))
    {
        baseline.explanation = after.explanation.clone();
    }
    // Marks are refused to agents at dispatch, so a reader never authored one
    // and there is nothing of its own to forgive.
    baseline
}

/// The whole section, or nothing when the page is untouched.
///
/// The header used to say "what the human did while you were thinking" over
/// every list it printed, including lists made entirely of the agent's own
/// writes. An agent that draws and then reads was handed its own work back as
/// the human's, which is the one misreading this surface exists to prevent.
/// The heading now describes what is actually in the list.
pub fn describe_changes(before: &Digest, after: &Digest) -> String {
    let lines = changes(before, after);
    if lines.is_empty() {
        return String::new();
    }
    // Attribution comes from the objects that actually differ, not from the
    // page as a whole: a page full of the human's old cards says nothing about
    // who made the edits being listed right now.
    let mut bylines: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for node in &after.nodes {
        match before.nodes.iter().find(|old| old.id == node.id) {
            None => {
                if !node.touched_by.is_empty() {
                    bylines.insert(node.touched_by.as_str());
                }
            }
            Some(old) => {
                if old.state_read_back != node.state_read_back && !node.state_touched_by.is_empty()
                {
                    bylines.insert(node.state_touched_by.as_str());
                }
                let mut old_without_state = old.clone();
                old_without_state.state_read_back.clear();
                old_without_state.state_touched_by.clear();
                let mut node_without_state = node.clone();
                node_without_state.state_read_back.clear();
                node_without_state.state_touched_by.clear();
                if old_without_state != node_without_state && !node.touched_by.is_empty() {
                    bylines.insert(node.touched_by.as_str());
                }
            }
        }
    }
    for edge in &after.edges {
        if !before.edges.iter().any(|old| old == edge) {
            let byline = if edge.touched_by.is_empty() {
                &edge.created_by
            } else {
                &edge.touched_by
            };
            if !byline.is_empty() {
                bylines.insert(byline.as_str());
            }
        }
    }
    for shape in &after.shapes {
        if !before.shapes.iter().any(|old| old == shape) && !shape.touched_by.is_empty() {
            bylines.insert(shape.touched_by.as_str());
        }
    }
    for variable in &after.variables {
        let changed = match before
            .variables
            .iter()
            .find(|old| old.name == variable.name)
        {
            None => true,
            Some(old) => old != variable,
        };
        if changed && variable.state != VarState::Measured && !variable.touched_by.is_empty() {
            bylines.insert(variable.touched_by.as_str());
        }
    }
    if before.explanation != after.explanation {
        if let Some(flow) = after.explanation.as_ref() {
            if !flow.touched_by.is_empty() {
                bylines.insert(flow.touched_by.as_str());
            }
        }
    }
    // Marks are refused to agents at dispatch, so any mark change is the
    // human by construction.
    if after.marks != before.marks {
        bylines.insert("human");
    }
    for byline in claims::bylines(before, after) {
        bylines.insert(byline);
    }
    let human = bylines.remove("human");
    let others = !bylines.is_empty();
    let who = match (human, others) {
        (true, false) => "what the human did while you were thinking".to_string(),
        (false, true) => format!(
            "these are writes by {} — not the human",
            bylines.into_iter().collect::<Vec<_>>().join(", ")
        ),
        (true, true) => format!(
            "the human's edits, and writes by {}",
            bylines.into_iter().collect::<Vec<_>>().join(", ")
        ),
        (false, false) => "what changed on the page since your last read".to_string(),
    };
    format!(
        "\nCHANGED SINCE YOUR LAST READ ({}) — {who}\n{}\n",
        lines.len(),
        lines.join("\n")
    )
}

// ── validation ────────────────────────────────────────────────────────────

pub(crate) fn capacity(atlas: &Atlas) -> Result<(), AtlasError> {
    let total = atlas.nodes.len()
        + atlas.edges.len()
        + atlas.marks.len()
        + atlas.claims.len()
        + atlas.shapes.len()
        + atlas.constraints.len()
        + atlas.variables.len()
        + atlas.explanations.len()
        + atlas.relations.len();
    if total >= MAX_OBJECTS {
        return Err(format!(
            "the atlas is full ({MAX_OBJECTS} objects); remove something first"
        ));
    }
    Ok(())
}

pub(crate) fn text(value: &str, limit: usize, field: &str) -> Result<String, AtlasError> {
    let trimmed = value.trim();
    if trimmed.chars().count() > limit {
        return Err(format!("{field} is longer than {limit} characters"));
    }
    // The browser module builds every DOM node itself and only ever sets
    // textContent, but control characters would still corrupt the read-back
    // the agent parses, so they are rejected at the writer instead.
    if trimmed.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!("{field} contains control characters"));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn one_of(value: &str, allowed: &[&str], field: &str) -> Result<String, AtlasError> {
    let value = value.trim();
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "{field} must be one of {}; got {value:?}",
            allowed.join(", ")
        ))
    }
}

// ── diff: two documents, side by side ─────────────────────────────────────
//
// [`changes`] answers "what happened since I last looked", against a digest
// the reader kept. This answers a different question: two documents, which may
// never have shared a session, and what one says that the other does not. The
// output is sentences because the reader is a model reviewing its own work,
// and it comes with a receipt so a later reader can tell which two documents
// were compared without being handed both.

/// One object's comparable properties, in a fixed order.
type Fields = Vec<(&'static str, String)>;

/// Geometry is reported as movement and resizing rather than as a field
/// change, so these names never reach a `changed` sentence.
const GEOMETRY: &[&str] = &["x", "y", "w", "h", "points"];

/// Two decimals: enough to see a drag, stable across a round trip through
/// JSON, and never a different string for the same picture.
fn fixed(value: f64) -> String {
    format!("{value:.2}")
}

fn node_fields(node: &Node) -> Fields {
    vec![
        ("label", node.label.clone()),
        ("status", node.status.clone()),
        ("tone", node.tone.clone()),
        ("parent", node.parent.clone()),
        ("path", node.path.clone()),
        ("lines", node.lines.clone()),
        ("note", node.note.clone()),
        ("color", node.color.clone()),
        ("emphasis", node.emphasis.clone()),
        ("size", node.size.clone()),
        ("kind", node.kind.clone()),
        ("diagram kind", node.diagram_kind.clone()),
        ("x", fixed(node.x)),
        ("y", fixed(node.y)),
        ("w", fixed(node.w)),
        ("h", fixed(node.h)),
    ]
}

fn edge_fields(edge: &Edge) -> Fields {
    vec![
        ("from", edge.from.clone()),
        ("to", edge.to.clone()),
        ("label", edge.label.clone()),
        ("event", edge.event.clone()),
        ("guard", edge.guard.clone()),
    ]
}

fn claim_fields(claim: &Claim) -> Fields {
    vec![
        ("about", claim.about.clone()),
        ("text", claim.text.clone()),
        ("basis", claim.basis.clone()),
        ("source", claim.source_ref()),
        ("verdict", claim.verdict.clone()),
        ("withdrawn", claim.withdrawn.to_string()),
    ]
}

fn mark_fields(mark: &Mark) -> Fields {
    vec![
        ("target", mark.target.clone()),
        ("glyph", mark.glyph.clone()),
        ("text", mark.text.clone()),
        ("answer", mark.answer.clone()),
    ]
}

fn shape_fields(shape: &Shape) -> Fields {
    vec![
        ("form", shape.form.clone()),
        ("label", shape.label.clone()),
        ("ink", shape.ink.clone()),
        ("fill", shape.fill.clone()),
        ("from", shape.from.clone()),
        ("to", shape.to.clone()),
        ("x", fixed(shape.x)),
        ("y", fixed(shape.y)),
        ("w", fixed(shape.w)),
        ("h", fixed(shape.h)),
        (
            "points",
            shape
                .points
                .iter()
                .map(|(x, y)| format!("{},{}", fixed(*x), fixed(*y)))
                .collect::<Vec<String>>()
                .join(" "),
        ),
    ]
}

fn look_up(fields: &Fields, key: &str) -> f64 {
    fields
        .iter()
        .find(|(name, _)| *name == key)
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0.0)
}

/// `12 right and 4 up`, or `12 right`. `None` when nothing moved far enough
/// to be a move rather than a rounding difference.
fn movement(before: &Fields, after: &Fields) -> Option<String> {
    let dx = look_up(after, "x") - look_up(before, "x");
    let dy = look_up(after, "y") - look_up(before, "y");
    let mut parts = Vec::new();
    if dx.abs() >= MOVE_FLOOR {
        parts.push(format!(
            "{} {}",
            dx.abs().round(),
            if dx > 0.0 { "right" } else { "left" }
        ));
    }
    if dy.abs() >= MOVE_FLOOR {
        parts.push(format!(
            "{} {}",
            dy.abs().round(),
            if dy > 0.0 { "down" } else { "up" }
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" and "))
    }
}

/// `260x120 to 260x180`, or `None` when the box is the same size.
fn resizing(before: &Fields, after: &Fields) -> Option<String> {
    let (bw, bh) = (look_up(before, "w"), look_up(before, "h"));
    let (aw, ah) = (look_up(after, "w"), look_up(after, "h"));
    if (aw - bw).abs() < MOVE_FLOOR && (ah - bh).abs() < MOVE_FLOOR {
        return None;
    }
    Some(format!(
        "{}x{} to {}x{}",
        bw.round(),
        bh.round(),
        aw.round(),
        ah.round()
    ))
}

/// Below this a difference is a rounding artefact, not a gesture.
const MOVE_FLOOR: f64 = 0.5;

fn changed_fields(before: &Fields, after: &Fields) -> Vec<String> {
    let mut out = Vec::new();
    for (name, new) in after {
        if GEOMETRY.contains(name) {
            continue;
        }
        let old = before
            .iter()
            .find(|(other, _)| other == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        if old != new {
            out.push(format!("{name} {old:?} -> {new:?}"));
        }
    }
    out
}

/// Every object of one kind, by id, with its comparable fields and the words
/// that name it in a sentence.
struct Side {
    fields: std::collections::BTreeMap<String, Fields>,
    titles: std::collections::BTreeMap<String, String>,
}

fn side<T, F, N>(items: &[T], id_of: impl Fn(&T) -> String, fields: F, title: N) -> Side
where
    F: Fn(&T) -> Fields,
    N: Fn(&T) -> String,
{
    let mut out = Side {
        fields: std::collections::BTreeMap::new(),
        titles: std::collections::BTreeMap::new(),
    };
    for item in items {
        let id = id_of(item);
        out.fields.insert(id.clone(), fields(item));
        out.titles.insert(id, title(item));
    }
    out
}

fn compare(before: &Side, after: &Side, out: &mut Vec<String>) {
    for id in after.fields.keys() {
        if !before.fields.contains_key(id) {
            out.push(format!("added {}", after.titles[id]));
        }
    }
    for id in before.fields.keys() {
        if !after.fields.contains_key(id) {
            out.push(format!("removed {}", before.titles[id]));
        }
    }
    for (id, new) in &after.fields {
        let Some(old) = before.fields.get(id) else {
            continue;
        };
        let title = &after.titles[id];
        let changed = changed_fields(old, new);
        if !changed.is_empty() {
            out.push(format!("changed {title}: {}", changed.join("; ")));
        }
        if let Some(moved) = movement(old, new) {
            out.push(format!("moved {title} {moved}"));
        }
        if let Some(resized) = resizing(old, new) {
            out.push(format!("resized {title} {resized}"));
        }
        let points = |fields: &Fields| {
            fields
                .iter()
                .find(|(name, _)| *name == "points")
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        if points(old) != points(new) {
            out.push(format!("redrawn {title}"));
        }
    }
}

/// An edge names the two cards it runs between, not its own id alone.
fn edge_title(edge: &Edge, atlas: &Atlas, fallback: &Atlas) -> String {
    format!(
        "edge [{}] {} -> {}",
        edge.id,
        atlas.naming(&edge.from, fallback),
        atlas.naming(&edge.to, fallback)
    )
}

/// A claim names the node it is about.
fn claim_title(claim: &Claim, atlas: &Atlas, fallback: &Atlas) -> String {
    format!(
        "claim [{}] on {}",
        claim.id,
        atlas.naming(&claim.about, fallback)
    )
}

/// A mark names its glyph and whatever it is attached to.
fn mark_title(mark: &Mark, atlas: &Atlas, fallback: &Atlas) -> String {
    format!(
        "mark {} [{}] on {}",
        mark.glyph,
        mark.id,
        atlas.naming(&mark.target, fallback)
    )
}

impl Atlas {
    /// What one node is called, for a sentence about something attached to it.
    fn naming(&self, id: &str, fallback: &Atlas) -> String {
        self.node(id)
            .or_else(|| fallback.node(id))
            .map(|node| format!("\"{}\"", node.label))
            .unwrap_or_else(|| id.to_string())
    }

    /// A stable content fingerprint over everything [`Atlas::diff`] compares.
    ///
    /// Two documents with the same fingerprint produce an empty diff, and two
    /// documents that produce an empty diff have the same fingerprint. Nothing
    /// here depends on CRDT map iteration order, so the same picture hashes
    /// the same on every replica.
    pub fn fingerprint(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut push = |kind: &str, id: &str, fields: &Fields| {
            let body: Vec<String> = fields
                .iter()
                .map(|(name, value)| format!("{name}={value:?}"))
                .collect();
            lines.push(format!("{kind} {id} {}", body.join(" ")));
        };
        for node in &self.nodes {
            push(KIND_NODE, &node.id, &node_fields(node));
        }
        for edge in &self.edges {
            push(KIND_EDGE, &edge.id, &edge_fields(edge));
        }
        for claim in &self.claims {
            push(KIND_CLAIM, &claim.id, &claim_fields(claim));
        }
        for mark in &self.marks {
            push(KIND_MARK, &mark.id, &mark_fields(mark));
        }
        for shape in &self.shapes {
            push(KIND_SHAPE, &shape.id, &shape_fields(shape));
        }
        lines.sort();
        format!("{:x}", Sha256::digest(lines.join("\n").as_bytes()))
    }

    /// One sentence per difference between this document and a later one:
    /// added, removed, changed, moved, resized, redrawn. Empty when the two
    /// documents say the same thing.
    ///
    /// `self` is the earlier document and `other` the later one, so "added"
    /// means present in `other` only. Objects are matched by id, which is what
    /// makes this the right tool for two snapshots of one document and the
    /// wrong one for two documents that were drawn independently.
    pub fn diff(&self, other: &Atlas) -> Vec<String> {
        let mut out = Vec::new();
        compare(
            &side(
                &self.nodes,
                |node| node.id.clone(),
                node_fields,
                |node| format!("node \"{}\" [{}]", node.label, node.id),
            ),
            &side(
                &other.nodes,
                |node| node.id.clone(),
                node_fields,
                |node| format!("node \"{}\" [{}]", node.label, node.id),
            ),
            &mut out,
        );
        compare(
            &side(
                &self.edges,
                |edge| edge.id.clone(),
                edge_fields,
                |edge| edge_title(edge, self, other),
            ),
            &side(
                &other.edges,
                |edge| edge.id.clone(),
                edge_fields,
                |edge| edge_title(edge, other, self),
            ),
            &mut out,
        );
        compare(
            &side(
                &self.claims,
                |claim| claim.id.clone(),
                claim_fields,
                |claim| claim_title(claim, self, other),
            ),
            &side(
                &other.claims,
                |claim| claim.id.clone(),
                claim_fields,
                |claim| claim_title(claim, other, self),
            ),
            &mut out,
        );
        compare(
            &side(
                &self.marks,
                |mark| mark.id.clone(),
                mark_fields,
                |mark| mark_title(mark, self, other),
            ),
            &side(
                &other.marks,
                |mark| mark.id.clone(),
                mark_fields,
                |mark| mark_title(mark, other, self),
            ),
            &mut out,
        );
        let shape_title = |shape: &Shape| {
            if shape.label.is_empty() {
                format!("shape {} [{}]", shape.form, shape.id)
            } else {
                format!("shape {} [{}] \"{}\"", shape.form, shape.id, shape.label)
            }
        };
        compare(
            &side(
                &self.shapes,
                |shape| shape.id.clone(),
                shape_fields,
                shape_title,
            ),
            &side(
                &other.shapes,
                |shape| shape.id.clone(),
                shape_fields,
                shape_title,
            ),
            &mut out,
        );
        out
    }

    /// The diff plus a receipt: which two documents were compared, and what
    /// the comparison said. A later reader can recompute this from the same
    /// two documents and get the same three hashes, or find out that it
    /// cannot.
    pub fn diff_receipt(&self, other: &Atlas) -> DiffReceipt {
        let sentences = self.diff(other);
        let before = self.fingerprint();
        let after = other.fingerprint();
        let body = format!("{before}\n{after}\n{}", sentences.join("\n"));
        DiffReceipt {
            before,
            after,
            receipt: format!("{:x}", Sha256::digest(body.as_bytes())),
            sentences,
        }
    }
}

/// What [`Atlas::diff_receipt`] hands back: the sentences, the fingerprint of
/// each document, and one hash over all three so the reading can be quoted
/// without the documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffReceipt {
    /// Fingerprint of the earlier document.
    pub before: String,
    /// Fingerprint of the later document.
    pub after: String,
    /// sha256 over both fingerprints and every sentence.
    pub receipt: String,
    pub sentences: Vec<String>,
}

fn explanation_id(value: &str, field: &str) -> Result<String, AtlasError> {
    let value = text(value, 160, field)?;
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character))
    {
        return Err(format!(
            "{field} must use 1 to 160 ASCII letters, numbers, dot, underscore, colon, or hyphen"
        ));
    }
    Ok(value)
}

fn validate_explanation_structure(
    definition: &ExplanationFlowDefinition,
) -> Result<(), AtlasError> {
    if definition.schema != EXPLANATION_FLOW_SCHEMA {
        return Err(format!(
            "explanation schema must be {EXPLANATION_FLOW_SCHEMA:?}"
        ));
    }
    explanation_id(&definition.id, "explanation id")?;
    let unique: std::collections::BTreeSet<_> = definition.scene_ids.iter().collect();
    if unique.len() != definition.scene_ids.len() {
        return Err("scene_ids must not contain duplicate objects".to_string());
    }
    if definition.about.len() > 64 || definition.scene_ids.len() > 512 {
        return Err("exploration scope exceeds 64 subjects or 512 visual objects".to_string());
    }
    if text(&definition.title, MAX_LABEL, "explanation title")?.is_empty() {
        return Err("explanation title must not be empty".to_string());
    }
    if text(&definition.goal, MAX_NOTE, "explanation goal")?.is_empty() {
        return Err("explanation goal must not be empty".to_string());
    }
    explanation_id(&definition.start, "explanation start")?;
    if definition.beats.is_empty() || definition.beats.len() > MAX_EXPLANATION_BEATS {
        return Err(format!(
            "an explanation needs 1 to {MAX_EXPLANATION_BEATS} beats"
        ));
    }

    let mut beat_ids = std::collections::BTreeSet::new();
    for beat in &definition.beats {
        let beat_id = explanation_id(&beat.id, "beat id")?;
        if !beat_ids.insert(beat_id.clone()) {
            return Err(format!("duplicate explanation beat {beat_id:?}"));
        }
        if text(&beat.title, MAX_LABEL, "beat title")?.is_empty() {
            return Err(format!("beat {beat_id:?} title must not be empty"));
        }
        if text(&beat.intent, MAX_TEXT, "beat intent")?.is_empty() {
            return Err(format!("beat {beat_id:?} intent must not be empty"));
        }
        if text(&beat.cue, MAX_TEXT, "beat cue")?.is_empty() {
            return Err(format!("beat {beat_id:?} cue must not be empty"));
        }
        if beat.evidence.is_empty() || beat.evidence.len() > MAX_EXPLANATION_EVIDENCE {
            return Err(format!(
                "beat {beat_id:?} needs 1 to {MAX_EXPLANATION_EVIDENCE} evidence bindings"
            ));
        }
        if beat.actions.is_empty() || beat.actions.len() > MAX_EXPLANATION_ACTIONS {
            return Err(format!(
                "beat {beat_id:?} needs 1 to {MAX_EXPLANATION_ACTIONS} visual actions"
            ));
        }
        for evidence in &beat.evidence {
            explanation_id(&evidence.target_id, "evidence target id")?;
            if text(&evidence.detail, MAX_TEXT, "evidence detail")?.is_empty() {
                return Err(format!("beat {beat_id:?} has an empty evidence detail"));
            }
        }
        let cue_chars = beat.cue.chars().count();
        let mut action_keys = std::collections::BTreeSet::new();
        for action in &beat.actions {
            one_of(
                &action.kind,
                EXPLANATION_ACTION_KINDS,
                "explanation action kind",
            )?;
            explanation_id(&action.target_id, "action target id")?;
            if !action_keys.insert((action.kind.clone(), action.target_id.clone())) {
                return Err(format!(
                    "beat {beat_id:?} repeats action {} on {}",
                    action.kind, action.target_id
                ));
            }
            if let Some([start, end]) = action.cue_span {
                let (start, end) = (start as usize, end as usize);
                if start >= end || end > cue_chars {
                    return Err(format!(
                        "beat {beat_id:?} action cue_span must be inside its {cue_chars}-character cue"
                    ));
                }
            }
        }

        one_of(
            &beat.advance.mode,
            EXPLANATION_ADVANCE_MODES,
            "explanation advance mode",
        )?;
        text(&beat.advance.prompt, MAX_TEXT, "advance prompt")?;
        if beat.advance.transitions.len() > MAX_EXPLANATION_TRANSITIONS {
            return Err(format!(
                "beat {beat_id:?} may have at most {MAX_EXPLANATION_TRANSITIONS} transitions"
            ));
        }
        match beat.advance.mode.as_str() {
            "terminal" if !beat.advance.transitions.is_empty() => {
                return Err(format!(
                    "terminal beat {beat_id:?} must not have transitions"
                ));
            }
            "continue" if beat.advance.transitions.len() != 1 => {
                return Err(format!(
                    "continue beat {beat_id:?} needs exactly one transition"
                ));
            }
            "choice" if beat.advance.transitions.len() < 2 => {
                return Err(format!(
                    "choice beat {beat_id:?} needs at least two transitions"
                ));
            }
            "agent" if beat.advance.transitions.is_empty() => {
                return Err(format!(
                    "agent beat {beat_id:?} needs at least one transition"
                ));
            }
            _ => {}
        }
        let mut transition_ids = std::collections::BTreeSet::new();
        for transition in &beat.advance.transitions {
            let transition_id = explanation_id(&transition.id, "transition id")?;
            if !transition_ids.insert(transition_id.clone()) {
                return Err(format!(
                    "beat {beat_id:?} repeats transition {transition_id:?}"
                ));
            }
            if text(&transition.label, MAX_LABEL, "transition label")?.is_empty() {
                return Err(format!(
                    "transition {transition_id:?} in beat {beat_id:?} needs a label"
                ));
            }
            if let Some(control) = transition.control.as_deref() {
                one_of(control, &["button"], "explanation transition control")?;
            }
            if let Some(next) = &transition.next {
                explanation_id(next, "transition next beat")?;
            }
            if transition.target_ids.len() > 32 || transition.phrases.len() > 32 {
                return Err(format!(
                    "transition {transition_id:?} may name at most 32 targets and 32 phrases"
                ));
            }
            let mut targets = std::collections::BTreeSet::new();
            for target_id in &transition.target_ids {
                let target_id = explanation_id(target_id, "transition target id")?;
                if !targets.insert(target_id.clone()) {
                    return Err(format!(
                        "transition {transition_id:?} repeats target {target_id:?}"
                    ));
                }
            }
            for phrase in &transition.phrases {
                if text(phrase, MAX_LABEL, "transition phrase")?.is_empty() {
                    return Err(format!(
                        "transition {transition_id:?} contains an empty phrase"
                    ));
                }
            }
        }
    }

    if !beat_ids.contains(&definition.start) {
        return Err(format!(
            "explanation start {:?} is not a beat",
            definition.start
        ));
    }
    for beat in &definition.beats {
        for transition in &beat.advance.transitions {
            if let Some(next) = &transition.next {
                if !beat_ids.contains(next) {
                    return Err(format!(
                        "transition {:?} from beat {:?} points to missing beat {next:?}",
                        transition.id, beat.id
                    ));
                }
            }
        }
    }

    fn visit(
        id: &str,
        beats: &std::collections::BTreeMap<&str, &ExplanationBeat>,
        visiting: &mut std::collections::BTreeSet<String>,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), AtlasError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.to_string()) {
            return Err(format!(
                "explanation flow contains a cycle through beat {id:?}; v1 flows must be finite"
            ));
        }
        let beat = beats
            .get(id)
            .ok_or_else(|| format!("missing explanation beat {id:?}"))?;
        for next in beat
            .advance
            .transitions
            .iter()
            .filter_map(|transition| transition.next.as_deref())
        {
            visit(next, beats, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.to_string());
        Ok(())
    }

    let beats = definition
        .beats
        .iter()
        .map(|beat| (beat.id.as_str(), beat))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    visit(&definition.start, &beats, &mut visiting, &mut visited)?;
    if visited.len() != definition.beats.len() {
        let unreachable = definition
            .beats
            .iter()
            .filter(|beat| !visited.contains(&beat.id))
            .map(|beat| beat.id.clone())
            .collect::<Vec<_>>();
        return Err(format!(
            "explanation has unreachable beats: {}",
            unreachable.join(", ")
        ));
    }
    Ok(())
}

fn validate_explanation_state(
    definition: &ExplanationFlowDefinition,
    state: &ExplanationFlowState,
) -> Result<(), AtlasError> {
    if !definition
        .beats
        .iter()
        .any(|beat| beat.id == state.current_beat)
    {
        return Err(format!(
            "explanation state points to missing beat {:?}",
            state.current_beat
        ));
    }
    one_of(&state.status, EXPLANATION_STATUSES, "explanation status")?;
    if state.revision == 0 {
        return Err("explanation revision must be greater than zero".to_string());
    }
    if state.entered_revision == 0 || state.entered_revision > state.revision {
        return Err(
            "explanation entered_revision must be between one and its revision".to_string(),
        );
    }
    if state.last_selected_target_ids.len() > 32 {
        return Err("explanation state may record at most 32 selected targets".to_string());
    }
    text(&state.last_response, MAX_TEXT, "explanation last response")?;
    Ok(())
}

fn validate_explanation_targets(
    atlas: &Atlas,
    definition: &ExplanationFlowDefinition,
) -> Result<(), AtlasError> {
    for id in definition.about.iter().chain(&definition.scene_ids) {
        if !atlas.semantic_target_exists(id) {
            return Err(format!(
                "exploration scope target {id:?} is not on the atlas"
            ));
        }
    }
    for id in &definition.scene_ids {
        if let Some(shape) = atlas.shape(id) {
            if !shape.segment_parent_id.is_empty()
                && !definition.scene_ids.contains(&shape.segment_parent_id)
            {
                return Err(format!(
                    "exploration object {id:?} requires its segment parent in scene_ids"
                ));
            }
        }
        if definition.about.contains(id) {
            return Err(format!(
                "subject {id:?} cannot also be owned by its exploration"
            ));
        }
        if atlas.explanations.iter().any(|flow| {
            flow.definition.id != definition.id && flow.definition.scene_ids.contains(id)
        }) {
            return Err(format!(
                "visual object {id:?} already belongs to another exploration"
            ));
        }
    }
    for beat in &definition.beats {
        for evidence in &beat.evidence {
            if !atlas.semantic_target_exists(&evidence.target_id) {
                return Err(format!(
                    "beat {:?} evidence target {:?} is not on the atlas",
                    beat.id, evidence.target_id
                ));
            }
        }
        for action in &beat.actions {
            if !atlas.semantic_target_exists(&action.target_id) {
                return Err(format!(
                    "beat {:?} action target {:?} is not on the atlas",
                    beat.id, action.target_id
                ));
            }
            if !definition.scene_ids.is_empty() && !definition.scene_ids.contains(&action.target_id)
            {
                return Err(format!(
                    "beat {:?} action target {:?} must be visible in scene_ids",
                    beat.id, action.target_id
                ));
            }
            if action.kind == "trace-path"
                && !atlas
                    .shape(&action.target_id)
                    .is_some_and(|shape| matches!(shape.form.as_str(), "arrow" | "line"))
            {
                return Err(format!(
                    "beat {:?} trace-path target {:?} must be a drawn arrow or line",
                    beat.id, action.target_id
                ));
            }
            if action.kind == "replay-motion" {
                let segment = atlas
                    .shape(&action.target_id)
                    .filter(|shape| shape.form == "segment")
                    .ok_or_else(|| {
                        format!(
                            "beat {:?} replay-motion target {:?} is not a segment",
                            beat.id, action.target_id
                        )
                    })?;
                if segment.segment_motion.is_null() {
                    return Err(format!(
                        "beat {:?} replay-motion target {:?} has no stored motion program",
                        beat.id, action.target_id
                    ));
                }
            }
        }
        for transition in &beat.advance.transitions {
            for target_id in &transition.target_ids {
                if !atlas.semantic_target_exists(target_id) {
                    return Err(format!(
                        "transition {:?} target {:?} is not on the atlas",
                        transition.id, target_id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn explanation_status_for_beat(beat: &ExplanationBeat) -> String {
    if beat.advance.mode == "terminal" {
        "completed".to_string()
    } else {
        "active".to_string()
    }
}

/// Place human ink in a scoped exploration without resetting its walkthrough.
pub fn place_exploration_shape(
    scene: &mut Scene,
    flow_id: &str,
    patch: &ShapePatch,
    author: &Author,
) -> Result<String, AtlasError> {
    place_exploration_object(scene, flow_id, patch.id.as_deref(), |scene| {
        place_shape(scene, patch, author)
    })
}

pub fn place_exploration_node(
    scene: &mut Scene,
    flow_id: &str,
    patch: &NodePatch,
    author: &Author,
) -> Result<String, AtlasError> {
    place_exploration_object(scene, flow_id, patch.id.as_deref(), |scene| {
        place_node(scene, patch, author)
    })
}

fn place_exploration_object(
    scene: &mut Scene,
    flow_id: &str,
    existing_id: Option<&str>,
    place: impl FnOnce(&mut Scene) -> Result<String, AtlasError>,
) -> Result<String, AtlasError> {
    let atlas = read(scene)?;
    let flow = atlas
        .explanations
        .iter()
        .find(|flow| flow.definition.id == flow_id)
        .ok_or_else(|| format!("exploration {flow_id:?} is missing"))?;
    if flow.definition.scene_ids.is_empty() {
        return place(scene);
    }
    if let Some(id) = existing_id {
        if !flow.definition.scene_ids.iter().any(|member| member == id) {
            return Err("cannot edit an object outside this exploration".to_string());
        }
        return place(scene);
    }
    if flow.definition.scene_ids.len() >= 512 {
        return Err("exploration has reached its 512 visual object limit".to_string());
    }
    let id = place(scene)?;
    scene
        .set_props(
            &ObjectId::from(id.clone()),
            &[("exploration_id", PropValue::Str(flow_id.to_string()))],
        )
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Define or replace a named explanation flow after validating the
/// complete graph and every stable target before any CRDT write begins.
pub fn define_explanation_flow(
    scene: &mut Scene,
    definition: &ExplanationFlowDefinition,
    author: &Author,
) -> Result<ExplanationFlow, AtlasError> {
    validate_explanation_structure(definition)?;
    let atlas = read(scene)?;
    validate_explanation_targets(&atlas, definition)?;
    let start = definition
        .beats
        .iter()
        .find(|beat| beat.id == definition.start)
        .ok_or_else(|| format!("missing start beat {:?}", definition.start))?;
    let existing = atlas
        .explanations
        .iter()
        .find(|flow| flow.definition.id == definition.id);
    let revision = existing.map_or(Ok(1), |flow| {
        flow.state
            .revision
            .checked_add(1)
            .ok_or_else(|| "explanation revision is exhausted".to_string())
    })?;
    let state = ExplanationFlowState {
        current_beat: definition.start.clone(),
        status: explanation_status_for_beat(start),
        revision,
        entered_revision: revision,
        last_transition: String::new(),
        last_response: String::new(),
        last_selected_target_ids: Vec::new(),
        touched_by: author.as_str().to_string(),
    };
    let encoded_definition = serde_json::to_string(definition)
        .map_err(|error| format!("could not encode explanation definition: {error}"))?;
    let encoded_state = serde_json::to_string(&state)
        .map_err(|error| format!("could not encode explanation state: {error}"))?;
    let (object_id, created_by) = if let Some(existing) = existing {
        scene
            .set_props(
                &ObjectId::from(existing.object_id.clone()),
                &[
                    ("definition", PropValue::Str(encoded_definition)),
                    ("state", PropValue::Str(encoded_state)),
                    ("touched_by", PropValue::Str(author.as_str().to_string())),
                ],
            )
            .map_err(|error| format!("could not replace explanation flow: {error}"))?;
        (existing.object_id.clone(), existing.created_by.clone())
    } else {
        capacity(&atlas)?;
        let object_id = scene
            .create_object_with_props(
                KIND_EXPLANATION_FLOW,
                author.clone(),
                &[
                    ("definition", PropValue::Str(encoded_definition)),
                    ("state", PropValue::Str(encoded_state)),
                    ("touched_by", PropValue::Str(author.as_str().to_string())),
                ],
            )
            .map(ObjectId::into_string)
            .map_err(|error| format!("could not create explanation flow: {error}"))?;
        (object_id, author.as_str().to_string())
    };
    Ok(ExplanationFlow {
        object_id,
        definition: definition.clone(),
        state,
        created_by,
    })
}

/// Advance one exact transition from one exact observed revision.
pub fn advance_explanation_flow(
    scene: &Scene,
    flow_id: &str,
    expected_revision: u32,
    transition_id: &str,
    response: &str,
    selected_target_ids: &[String],
    author: &Author,
) -> Result<ExplanationFlowState, AtlasError> {
    let atlas = read(scene)?;
    let flow = atlas
        .explanations
        .iter()
        .find(|flow| flow.definition.id == flow_id)
        .ok_or_else(|| format!("explanation {flow_id:?} is not on the atlas"))?;
    if flow.definition.id != flow_id {
        return Err(format!(
            "active explanation is {:?}, not {flow_id:?}",
            flow.definition.id
        ));
    }
    if flow.state.revision != expected_revision {
        return Err(format!(
            "explanation revision changed: expected {expected_revision}, current is {}",
            flow.state.revision
        ));
    }
    if flow.state.status != "active" {
        return Err(format!(
            "explanation is {}; resume or restart it before advancing",
            flow.state.status
        ));
    }
    let beat = flow
        .definition
        .beats
        .iter()
        .find(|beat| beat.id == flow.state.current_beat)
        .ok_or_else(|| format!("current beat {:?} is missing", flow.state.current_beat))?;
    let transition = beat
        .advance
        .transitions
        .iter()
        .find(|transition| transition.id == transition_id)
        .ok_or_else(|| format!("beat {:?} has no transition {transition_id:?}", beat.id))?;
    if selected_target_ids.len() > 32 {
        return Err("at most 32 selected targets may accompany a transition".to_string());
    }
    let mut selected = Vec::new();
    for target_id in selected_target_ids {
        let target_id = explanation_id(target_id, "selected target id")?;
        if !atlas.semantic_target_exists(&target_id) {
            return Err(format!("selected target {target_id:?} is not on the atlas"));
        }
        if !selected.contains(&target_id) {
            selected.push(target_id);
        }
    }
    if !selected.is_empty()
        && !transition.target_ids.is_empty()
        && !selected
            .iter()
            .any(|target| transition.target_ids.contains(target))
    {
        return Err(format!(
            "selection [{}] does not support transition {:?}; expected one of [{}]",
            selected.join(", "),
            transition.id,
            transition.target_ids.join(", ")
        ));
    }
    let next = transition
        .next
        .as_ref()
        .and_then(|id| flow.definition.beats.iter().find(|beat| beat.id == *id));
    if let Some(next) = next {
        for target in next
            .evidence
            .iter()
            .map(|evidence| evidence.target_id.as_str())
            .chain(next.actions.iter().map(|action| action.target_id.as_str()))
        {
            if !atlas.semantic_target_exists(target) {
                return Err(format!(
                    "next beat {:?} depends on missing target {target:?}",
                    next.id
                ));
            }
        }
    }
    let revision = flow
        .state
        .revision
        .checked_add(1)
        .ok_or_else(|| "explanation revision is exhausted".to_string())?;
    let state = ExplanationFlowState {
        current_beat: transition
            .next
            .clone()
            .unwrap_or_else(|| flow.state.current_beat.clone()),
        status: next.map_or_else(|| "completed".to_string(), explanation_status_for_beat),
        revision,
        entered_revision: revision,
        last_transition: transition.id.clone(),
        last_response: text(response, MAX_TEXT, "explanation response")?,
        last_selected_target_ids: selected,
        touched_by: author.as_str().to_string(),
    };
    validate_explanation_state(&flow.definition, &state)?;
    let encoded = serde_json::to_string(&state)
        .map_err(|error| format!("could not encode explanation state: {error}"))?;
    scene
        .set_props(
            &ObjectId::from(flow.object_id.clone()),
            &[
                ("state", PropValue::Str(encoded)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not advance explanation flow: {error}"))?;
    Ok(state)
}

/// Pause, resume, restart, or stop the current flow without changing its
/// definition.
pub fn control_explanation_flow(
    scene: &Scene,
    flow_id: &str,
    expected_revision: u32,
    command: &str,
    author: &Author,
) -> Result<ExplanationFlowState, AtlasError> {
    let atlas = read(scene)?;
    let flow = atlas
        .explanations
        .iter()
        .find(|flow| flow.definition.id == flow_id)
        .ok_or_else(|| format!("explanation {flow_id:?} is not on the atlas"))?;
    if flow.definition.id != flow_id {
        return Err(format!(
            "active explanation is {:?}, not {flow_id:?}",
            flow.definition.id
        ));
    }
    if flow.state.revision != expected_revision {
        return Err(format!(
            "explanation revision changed: expected {expected_revision}, current is {}",
            flow.state.revision
        ));
    }
    let mut state = flow.state.clone();
    let reenter = command == "restart";
    match command {
        "pause" if state.status == "active" => state.status = "paused".to_string(),
        "resume" if state.status == "paused" => state.status = "active".to_string(),
        "restart" => {
            state.current_beat = flow.definition.start.clone();
            let start = flow
                .definition
                .beats
                .iter()
                .find(|beat| beat.id == flow.definition.start)
                .ok_or_else(|| format!("missing start beat {:?}", flow.definition.start))?;
            state.status = explanation_status_for_beat(start);
            state.last_transition.clear();
            state.last_response.clear();
            state.last_selected_target_ids.clear();
        }
        "stop" if matches!(state.status.as_str(), "active" | "paused") => {
            state.status = "stopped".to_string();
        }
        "pause" | "resume" | "stop" => {
            return Err(format!(
                "cannot {command} an explanation whose status is {}",
                state.status
            ));
        }
        _ => {
            return Err("explanation command must be pause, resume, restart, or stop".to_string());
        }
    }
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or_else(|| "explanation revision is exhausted".to_string())?;
    if reenter {
        state.entered_revision = state.revision;
    }
    state.touched_by = author.as_str().to_string();
    validate_explanation_state(&flow.definition, &state)?;
    let encoded = serde_json::to_string(&state)
        .map_err(|error| format!("could not encode explanation state: {error}"))?;
    scene
        .set_props(
            &ObjectId::from(flow.object_id.clone()),
            &[
                ("state", PropValue::Str(encoded)),
                ("touched_by", PropValue::Str(author.as_str().to_string())),
            ],
        )
        .map_err(|error| format!("could not control explanation flow: {error}"))?;
    Ok(state)
}

fn coord(value: f64, field: &str) -> Result<f64, AtlasError> {
    if !value.is_finite() {
        return Err(format!("{field} must be a finite number"));
    }
    Ok(value.clamp(-WORLD_LIMIT, WORLD_LIMIT))
}

/// Repository-relative, no traversal, no absolute paths. The host resolves
/// this against a single root before reading anything; this is the first of
/// the two checks, not the only one.
pub(crate) fn source_path(value: &str) -> Result<String, AtlasError> {
    let path = value.trim();
    if path.is_empty() {
        return Ok(String::new());
    }
    if path.chars().count() > MAX_PATH {
        return Err(format!("path is longer than {MAX_PATH} characters"));
    }
    if path.starts_with('/') || path.starts_with('~') || path.contains(':') {
        return Err(format!("path {path:?} must be repository-relative"));
    }
    if path
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return Err(format!("path {path:?} must not contain . or .. segments"));
    }
    if path.contains('\\') || path.chars().any(|c| c.is_control()) {
        return Err(format!("path {path:?} contains unsupported characters"));
    }
    Ok(path.to_string())
}

/// `"12-48"` or `"12"`.
pub(crate) fn line_range(value: &str) -> Result<String, AtlasError> {
    let range = value.trim();
    if range.is_empty() {
        return Ok(String::new());
    }
    let (start, end) = match range.split_once('-') {
        Some((start, end)) => (start, end),
        None => (range, range),
    };
    let start: usize = start
        .trim()
        .parse()
        .map_err(|_| format!("line range {range:?} is not N or N-M"))?;
    let end: usize = end
        .trim()
        .parse()
        .map_err(|_| format!("line range {range:?} is not N or N-M"))?;
    if start == 0 || end < start {
        return Err(format!(
            "line range {range:?} is not a forward 1-based range"
        ));
    }
    Ok(if start == end {
        format!("{start}")
    } else {
        format!("{start}-{end}")
    })
}

/// A full commit hash, or empty.
///
/// Forty hex characters and nothing else. An abbreviation is refused rather
/// than expanded: the core cannot see a repository, so the only honest thing
/// it can check is the shape, and a short hash that resolves today can become
/// ambiguous tomorrow.
pub(crate) fn revision_hash(value: &str) -> Result<String, AtlasError> {
    let revision = value.trim();
    if revision.is_empty() {
        return Ok(String::new());
    }
    if revision.len() != REVISION_LENGTH || !revision.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "revision {revision:?} must be {REVISION_LENGTH} hexadecimal characters, the full commit hash"
        ));
    }
    Ok(revision.to_ascii_lowercase())
}

// ── layout: what the page actually looks like ─────────────────────────────
//
// The agent picks x/y and then gets x/y back. That is a write-only channel:
// it never learns that the note it added made a card tall enough to swallow
// the one below, or that the link it drew runs straight under a third node.
// Everything here exists so the geometry it cannot see comes back as text it
// can act on — measured where possible, and labelled as an estimate where not.

/// CRDT key holding the renderer's measured height.
const K_HEIGHT: &str = "h";

/// Rough card metrics, only used until a real renderer reports back.
const LINE_HEIGHT: f64 = 19.0;
const CHARS_PER_LINE_AT_DEFAULT_WIDTH: f64 = 34.0;
const CARD_CHROME: f64 = 58.0;

/// Guess how tall a card will render. Deliberately crude — its job is to keep
/// the agent from placing rows 60px apart before any browser has opened, not
/// to be right to the pixel. [`measure_node`] replaces it with the truth.
pub fn estimate_height(node: &Node) -> f64 {
    let size_scale = match node.size.as_str() {
        "hero" => 18.0 / 13.5,
        "primary" => 15.5 / 13.5,
        _ => 1.0,
    };
    let per_line =
        (CHARS_PER_LINE_AT_DEFAULT_WIDTH * node.w / DEFAULT_WIDTH / size_scale).max(12.0);
    let wrapped = |value: &str| -> f64 {
        if value.is_empty() {
            0.0
        } else {
            value
                .lines()
                .map(|line| (line.chars().count() as f64 / per_line).ceil().max(1.0))
                .sum()
        }
    };
    let label_height = wrapped(&node.label).max(1.0) * LINE_HEIGHT * size_scale;
    let detail_height = (wrapped(&node.note)
        + if node.path.is_empty() {
            0.0
        } else {
            wrapped(&node.source_ref())
        })
        * LINE_HEIGHT;
    CARD_CHROME + label_height + detail_height
}

/// Atlas card height used by semantic diagram packing before a browser has
/// rendered the card. It is the same estimator [`read`] uses for the projected
/// node, including explicit line breaks and the authored size class, so the
/// pack is tested against the geometry it will actually publish rather than a
/// smaller label-only rectangle from the shared diagram engine.
pub fn estimate_diagram_height(label: &str, width: f64, size: &str) -> f64 {
    estimate_source_backed_diagram_height(label, "", "", "", width, size)
}

/// Atlas card height used when a semantic diagram node carries evidence in
/// the same authoring call.
///
/// Source and note rows are visible card content, so packing a label-only box
/// and attaching them afterward is not a harmless refinement: it changes the
/// rectangle after links and siblings were placed around it. Keep this beside
/// [`estimate_diagram_height`] so both paths use the projection's one height
/// estimator rather than inventing a second approximation in the host.
pub fn estimate_source_backed_diagram_height(
    label: &str,
    path: &str,
    lines: &str,
    note: &str,
    width: f64,
    size: &str,
) -> f64 {
    estimate_height(&Node {
        id: String::new(),
        label: label.to_string(),
        path: path.to_string(),
        lines: lines.to_string(),
        note: note.to_string(),
        tone: "concept".to_string(),
        status: "open".to_string(),
        cemented: String::new(),
        x: 0.0,
        y: 0.0,
        w: width,
        h: 0.0,
        measured: false,
        created_by: String::new(),
        touched_by: String::new(),
        parent: String::new(),
        diagram_kind: String::new(),
        state_initial: false,
        state_terminal: false,
        state_variable: String::new(),
        state_previous_value: None,
        color: String::new(),
        emphasis: DEFAULT_EMPHASIS.to_string(),
        size: size.to_string(),
        kind: DEFAULT_KIND.to_string(),
    })
}

/// Default wrap width for a `text`, in world units.
pub const TEXT_WIDTH: f64 = 240.0;
const TEXT_LINE_HEIGHT: f64 = 17.0;
/// The font size `static/styles.css` gives `.shape-text` when none is set.
const TEXT_DEFAULT_FONT_SIZE: f64 = 13.0;
const TEXT_CHARS_PER_LINE_AT_DEFAULT_WIDTH: f64 = 38.0;

/// Guess how tall a text will render, on the same terms and for the same
/// reason as [`estimate_height`]: so the geometry is roughly right before any
/// browser has laid the words out, and honestly labelled until one has.
pub fn estimate_text_height(shape: &Shape) -> f64 {
    // The renderer's default face is 13px; a set size scales both how many
    // characters fit a line and how tall each line is.
    let scale = if shape.font_size > 0.0 {
        shape.font_size / TEXT_DEFAULT_FONT_SIZE
    } else {
        1.0
    };
    let per_line = (TEXT_CHARS_PER_LINE_AT_DEFAULT_WIDTH * shape.w / TEXT_WIDTH / scale).max(4.0);
    let lines = (shape.label.chars().count() as f64 / per_line)
        .ceil()
        .max(1.0);
    lines * TEXT_LINE_HEIGHT * scale
}

/// How tall a box's label will wrap to inside the box, on the same crude
/// terms as [`estimate_text_height`]: the renderer's box label is 14px and
/// wraps at the box width minus its padding.
pub const BOX_LABEL_FONT_SIZE: f64 = 14.0;
const BOX_LABEL_PADDING: f64 = 16.0;
pub fn estimate_box_label_height(shape: &Shape) -> f64 {
    let font = if shape.font_size > 0.0 {
        shape.font_size
    } else {
        BOX_LABEL_FONT_SIZE
    };
    let inner = (shape.w - BOX_LABEL_PADDING).max(1.0);
    let per_line = (inner / (font * 0.5)).max(4.0);
    let lines = shape
        .label
        .lines()
        .map(|line| (line.chars().count() as f64 / per_line).ceil().max(1.0))
        .sum::<f64>()
        .max(1.0);
    lines * font * 1.3 + BOX_LABEL_PADDING
}

/// Record what the renderer measured.
///
/// A third category of write, and worth naming: not an agent claim and not a
/// human gesture, but a derived fact only the thing doing layout can know. It
/// deliberately does NOT stamp `touched_by` — a browser reporting "this card
/// is 188px tall" must not make every node read `LAST-EDITED-BY=human` on the
/// agent's next turn and drown the signal that actually matters.
pub fn measure_node(scene: &Scene, id: &str, height: f64) -> Result<(), AtlasError> {
    if !height.is_finite() || height <= 0.0 {
        return Err(format!("measured height {height} is not a positive number"));
    }
    scene
        .set_prop(
            &ObjectId::from(id.to_string()),
            K_HEIGHT,
            PropValue::Num(height.clamp(1.0, 4_000.0)),
        )
        .map_err(|error| format!("could not record a measurement: {error}"))
}

/// The same write for a `text` shape, whose height is the one thing about it
/// nobody authors. Also does not stamp `touched_by`, for the same reason.
pub fn measure_shape(scene: &Scene, id: &str, height: f64) -> Result<(), AtlasError> {
    measure_node(scene, id, height)
}

/// One thing wrong with the way the page is laid out.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Problem {
    /// Stable machine-readable diagnostic identity.
    pub code: &'static str,
    /// Objects involved, so the agent can act without re-deriving them.
    pub ids: Vec<String>,
    pub detail: String,
}

/// Everything geometric the agent cannot see for itself.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Layout {
    /// `(left, top, right, bottom)` covering everything on the page — cards
    /// *and* what is drawn on them. Shapes belong in it: a stroke off to the
    /// right is part of how far the page reaches, and reporting a span that
    /// stops at the last card tells the agent the page is smaller than it is.
    pub extent: Option<(f64, f64, f64, f64)>,
    pub problems: Vec<Problem>,
    /// Nodes whose height is still a guess.
    pub unmeasured: usize,
}

/// Overlaps below this are rounding, not a layout problem.
const OVERLAP_SLOP: f64 = 4.0;

/// The first few words of a label, for a PROBLEMS line that names two texts
/// without repeating both paragraphs.
fn clipped(label: &str) -> String {
    let mut out: String = label.chars().take(40).collect();
    if label.chars().count() > 40 {
        out.push_str("...");
    }
    out
}
/// How far from the pack a node has to sit before it is worth mentioning.
const OUTLIER_FACTOR: f64 = 1.6;

pub fn layout(atlas: &Atlas) -> Layout {
    let mut report = Layout {
        unmeasured: atlas.nodes.iter().filter(|node| !node.measured).count(),
        ..Layout::default()
    };
    // The drawn layer is checked even on a page with no cards on it yet:
    // "there is nothing to lay out" and "there is nothing wrong" are not the
    // same answer, and only one of them is true when two strokes are stacked.
    report.problems.extend(drawing_problems(atlas));
    report.problems.extend(claims::problems(atlas));
    report.problems.extend(
        atlas
            .state_machine_views()
            .into_iter()
            .flat_map(|machine| machine.problems()),
    );

    if atlas.nodes.is_empty() && atlas.shapes.is_empty() {
        return report;
    }

    let boxes = atlas
        .nodes
        .iter()
        .map(Node::bounds)
        .chain(atlas.shapes.iter().map(Shape::bounds));
    report.extent = Some(boxes.fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |acc, (l, t, r, b)| (acc.0.min(l), acc.1.min(t), acc.2.max(r), acc.3.max(b)),
    ));

    if atlas.nodes.is_empty() {
        return report;
    }

    // Cards sitting on top of each other. This is the one the agent causes
    // most often: it writes a note, the card grows downwards, and the node it
    // carefully placed 168px below is now underneath it.
    for (index, a) in atlas.nodes.iter().enumerate() {
        for b in &atlas.nodes[index + 1..] {
            // A card inside its own container shares area with it by design.
            // Calling that "one is hiding the other" would make every
            // hierarchy report itself as broken.
            if atlas.related_by_containment(&a.id, &b.id) {
                continue;
            }
            let (al, at, ar, ab) = a.bounds();
            let (bl, bt, br, bb) = b.bounds();
            let across = ar.min(br) - al.max(bl);
            let down = ab.min(bb) - at.max(bt);
            if across > OVERLAP_SLOP && down > OVERLAP_SLOP {
                report.problems.push(Problem { code: "card_overlap",
                    ids: vec![a.id.clone(), b.id.clone()],
                    detail: format!(
                        "\"{}\" and \"{}\" OVERLAP by {across:.0}x{down:.0}px — one is hiding the other",
                        a.label, b.label
                    ),
                });
            }
        }
    }

    // Read the same route the renderer receives, not a superseded center line.
    let painting = atlas.painting();
    for edge in &atlas.edges {
        let (Some(from), Some(to)) = (atlas.node(&edge.from), atlas.node(&edge.to)) else {
            continue;
        };
        let Some(painted) = painting.links.iter().find(|link| link.id == edge.id) else {
            continue;
        };
        let route = if painted.path.is_empty() {
            report.problems.push(Problem { code: "route_unavailable", ids: vec![edge.id.clone()], detail: format!("route unavailable for link \"{}\" -> \"{}\"; measured cards overlap or exceed routing capacity", from.label, to.label) });
            painted.ends.to_vec()
        } else {
            painted.path.clone()
        };
        for node in &atlas.nodes {
            if node.id == edge.from
                || node.id == edge.to
                || atlas.nodes.iter().any(|child| child.parent == node.id)
            {
                continue;
            }
            // A link between two cards in the same container necessarily
            // crosses that container's box. It is not passing under an
            // unrelated card; it is inside the thing that owns both ends.
            if atlas.contains_node(&node.id, &edge.from) || atlas.contains_node(&node.id, &edge.to)
            {
                continue;
            }
            if route
                .windows(2)
                .any(|segment| segment_hits_box(segment[0], segment[1], node.bounds()))
            {
                report.problems.push(Problem {
                    code: "route_obstructed",
                    ids: vec![edge.id.clone(), node.id.clone()],
                    detail: format!(
                        "the link \"{}\" -> \"{}\" passes under \"{}\"",
                        from.label, to.label, node.label
                    ),
                });
            }
        }
    }

    let ordinary_links: Vec<_> = painting
        .links
        .iter()
        .filter(|link| {
            atlas
                .edges
                .iter()
                .any(|edge| edge.id == link.id && edge.event.is_empty())
        })
        .collect();
    for (index, a) in ordinary_links.iter().enumerate() {
        for b in ordinary_links.iter().skip(index + 1) {
            let merged = a.path.windows(2).any(|x| {
                b.path
                    .windows(2)
                    .any(|y| routing::shared_segment(x[0], x[1], y[0], y[1]))
            });
            if merged {
                report.problems.push(Problem { code: "shared_route",
                    ids: vec![a.id.clone(), b.id.clone()],
                    detail: format!("links {} and {} share a route segment; separate relationships need separate lanes", a.id, b.id),
                });
            }
        }
    }

    // Compare siblings, not every card at every hierarchy level. A child near
    // one edge of its parent is not stranded from root containers elsewhere
    // on the page, and mixing those coordinate populations makes a compact
    // hierarchy accuse its own first or last child of being off on its own.
    let mut sibling_sets: Vec<(&str, Vec<&Node>)> = Vec::new();
    for node in &atlas.nodes {
        let index = sibling_sets
            .iter()
            .position(|(parent, _)| *parent == node.parent)
            .unwrap_or_else(|| {
                sibling_sets.push((&node.parent, Vec::new()));
                sibling_sets.len() - 1
            });
        sibling_sets[index].1.push(node);
    }
    for (_, siblings) in sibling_sets
        .into_iter()
        .filter(|(_, siblings)| siblings.len() > 2)
    {
        let (sum_x, sum_y) = siblings.iter().fold((0.0, 0.0), |acc, node| {
            let (cx, cy) = node.center();
            (acc.0 + cx, acc.1 + cy)
        });
        let count = siblings.len() as f64;
        let centroid = (sum_x / count, sum_y / count);
        let distances = siblings
            .iter()
            .map(|node| {
                let (cx, cy) = node.center();
                ((cx - centroid.0).powi(2) + (cy - centroid.1).powi(2)).sqrt()
            })
            .collect::<Vec<_>>();
        let mean = distances.iter().sum::<f64>() / count;
        for (node, distance) in siblings.iter().zip(&distances) {
            if mean > 1.0 && *distance > mean * OUTLIER_FACTOR {
                report.problems.push(Problem { code: "stranded_node",
                    ids: vec![node.id.clone()],
                    detail: format!(
                        "\"{}\" sits {:.0}px from the middle of its level while the average is {mean:.0}px; it is off on its own",
                        node.label, distance
                    ),
                });
            }
        }
    }

    report
}

/// How much of the smaller of two strokes has to be inside the other's box
/// before they are stacked rather than adjacent.
const STROKE_STACK_FRACTION: f64 = 0.6;

/// What is wrong with the *drawn* layer.
///
/// Overlap detection used to cover cards only, which left the drawing as the
/// one part of the page the agent could not be told was going wrong. A stroke
/// lying across a card it does not mean anything about, two scribbles on top
/// of each other, a loop that half-covers something — each is invisible from
/// a coordinate list and obvious from three feet away.
fn drawing_problems(atlas: &Atlas) -> Vec<Problem> {
    let mut problems = Vec::new();

    for reading in readings(atlas) {
        let Some(shape) = atlas.shapes.iter().find(|s| s.id == reading.shape) else {
            continue;
        };

        // Half-covered cards, named by the shape that half-covers them.
        for id in &reading.partial {
            problems.push(Problem { code: "partial_group",
                ids: vec![shape.id.clone(), id.clone()],
                detail: format!(
                    "{}'s {} covers only part of \"{}\" — it reads as neither in nor out of the group; move the card clear or redraw the region around it",
                    shape.created_by,
                    shape.form,
                    atlas.label_of(id)
                ),
            });
        }

        // A stroke drawn over a card that it turns out to say nothing about.
        // On screen it is scribbled across text nobody can now read; in the
        // read-back it is decoration. Both halves of that are worth saying.
        if !reading.relation_touches_nodes() && shape.is_overlay() {
            for node in &atlas.nodes {
                if overlaps(shape.bounds(), node.bounds(), OVERLAP_SLOP) {
                    problems.push(Problem { code: "ink_obscures_card",
                        ids: vec![shape.id.clone(), node.id.clone()],
                        detail: format!(
                            "{}'s {} lies across \"{}\" but reads as touching no node — it is covering that card up without meaning anything about it",
                            shape.created_by, shape.form, node.label
                        ),
                    });
                }
            }
        }
    }

    // Two freehand strokes stacked on the same spot. Ink inside a grouping box
    // is the box working, so this is deliberately ink-against-ink only.
    let strokes: Vec<&Shape> = atlas
        .shapes
        .iter()
        .filter(|shape| shape.form == "ink")
        .collect();
    for (index, a) in strokes.iter().enumerate() {
        for b in &strokes[index + 1..] {
            let (al, at, ar, ab) = a.bounds();
            let (bl, bt, br, bb) = b.bounds();
            let across = ar.min(br) - al.max(bl);
            let down = ab.min(bb) - at.max(bt);
            if across <= 0.0 || down <= 0.0 {
                continue;
            }
            let smaller = (((ar - al) * (ab - at)).min((br - bl) * (bb - bt))).max(1.0);
            if across * down / smaller >= STROKE_STACK_FRACTION {
                problems.push(Problem { code: "stacked_ink",
                    ids: vec![a.id.clone(), b.id.clone()],
                    detail: format!(
                        "{}'s stroke and {}'s stroke are drawn on top of each other — whatever the lower one meant is unreadable on screen",
                        a.created_by, b.created_by
                    ),
                });
            }
        }
    }

    // Two box shapes (rect/ellipse/diamond) colliding on screen. This is
    // deliberately restricted to boxes: a text label sitting over the card it
    // names, an arrow crossing a shape it connects, or ink annotating a card
    // are the ordinary way the drawing layer is used, and flagging those
    // would make the read-back noise nobody reads. Two boxes overlapping
    // each other is not ordinary. It is the same "one is hiding the other"
    // collision node-node overlap already reports, just drawn instead of
    // placed. Full containment is excluded because that is a grouping box
    // doing its job (see the "encloses" reading), not a collision.
    let boxes: Vec<&Shape> = atlas.shapes.iter().filter(|shape| shape.is_box()).collect();
    for (index, a) in boxes.iter().enumerate() {
        for b in &boxes[index + 1..] {
            if !overlaps(a.bounds(), b.bounds(), OVERLAP_SLOP) {
                continue;
            }
            if a.covers_bounds(b.bounds()) == Coverage::Inside
                || b.covers_bounds(a.bounds()) == Coverage::Inside
            {
                continue;
            }
            let (al, at, ar, ab) = a.bounds();
            let (bl, bt, br, bb) = b.bounds();
            let across = ar.min(br) - al.max(bl);
            let down = ab.min(bb) - at.max(bt);
            problems.push(Problem { code: "shape_overlap",
                ids: vec![a.id.clone(), b.id.clone()],
                detail: format!(
                    "{}'s {} and {}'s {} OVERLAP by {across:.0}x{down:.0}px. One is hiding the other",
                    a.created_by, a.form, b.created_by, b.form
                ),
            });
        }
    }

    // A box whose label cannot fit inside it. The renderer wraps a box's
    // words inside the box, so a long label in a small box spills out of the
    // bottom on screen; the estimate is crude on purpose, the same way card
    // heights are, and only fires when the words clearly do not fit.
    for shape in atlas
        .shapes
        .iter()
        .filter(|shape| shape.is_box() && !shape.label.is_empty())
    {
        let needed = estimate_box_label_height(shape);
        if needed > shape.h + OVERLAP_SLOP {
            problems.push(Problem { code: "label_overflow",
                ids: vec![shape.id.clone()],
                detail: format!(
                    "{}'s {} label \"{}\" needs about {needed:.0}px of height at {:.0}px wide and the box is {:.0}px tall. The words spill out of it; give the box room or shorten the label",
                    shape.created_by,
                    shape.form,
                    clipped(&shape.label),
                    shape.w,
                    shape.h
                ),
            });
        }
    }

    // Two texts stacked on each other. A heading that wrapped into the
    // caption under it is the commonest way a drawn page stops being
    // readable, and neither text is a box, so the loop above never sees it.
    let texts: Vec<&Shape> = atlas
        .shapes
        .iter()
        .filter(|shape| shape.form == "text")
        .collect();
    for (index, a) in texts.iter().enumerate() {
        for b in &texts[index + 1..] {
            if !overlaps(a.bounds(), b.bounds(), OVERLAP_SLOP) {
                continue;
            }
            let (al, at, ar, ab) = a.bounds();
            let (bl, bt, br, bb) = b.bounds();
            let across = ar.min(br) - al.max(bl);
            let down = ab.min(bb) - at.max(bt);
            problems.push(Problem { code: "text_overlap",
                ids: vec![a.id.clone(), b.id.clone()],
                detail: format!(
                    "{}'s text \"{}\" and {}'s text \"{}\" OVERLAP by {across:.0}x{down:.0}px. Neither is readable where they cross",
                    a.created_by,
                    clipped(&a.label),
                    b.created_by,
                    clipped(&b.label)
                ),
            });
        }
    }

    // The same collision between a box shape and a card. Full containment
    // (the box grouping the card) and partial containment (already reported
    // above as "covers only part of") are both excluded here so this only
    // fires for the residue: a real pixel collision that the area-fraction
    // coverage test called `Outside` because the card is much bigger than
    // the sliver of it the box actually crosses. Non-box shapes are left to
    // the overlay check above, which already covers a stroke or text lying
    // across a card it says nothing about.
    for shape in &boxes {
        for node in &atlas.nodes {
            if !matches!(shape.covers(node), Coverage::Outside) {
                continue;
            }
            if overlaps(shape.bounds(), node.bounds(), OVERLAP_SLOP) {
                let (sl, st, sr, sb) = shape.bounds();
                let (nl, nt, nr, nb) = node.bounds();
                let across = sr.min(nr) - sl.max(nl);
                let down = sb.min(nb) - st.max(nt);
                problems.push(Problem {
                    code: "shape_obscures_card",
                    ids: vec![shape.id.clone(), node.id.clone()],
                    detail: format!(
                        "{}'s {} clips \"{}\" by {across:.0}x{down:.0}px. One is hiding the other",
                        shape.created_by, shape.form, node.label
                    ),
                });
            }
        }
    }

    problems
}

impl Reading {
    /// Whether this reading makes any claim about a node at all.
    fn relation_touches_nodes(&self) -> bool {
        !self.targets.is_empty()
    }
}

/// Does the segment `a`-`b` intersect the axis-aligned box?
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

/// First grid slot no existing node is sitting on.
fn free_slot(atlas: &Atlas) -> (f64, f64) {
    for index in 0..MAX_OBJECTS {
        let x = GRID_ORIGIN.0 + (index % GRID_COLUMNS) as f64 * GRID_STEP.0;
        let y = GRID_ORIGIN.1 + (index / GRID_COLUMNS) as f64 * GRID_STEP.1;
        let occupied = atlas
            .nodes
            .iter()
            .any(|node| (node.x - x).abs() < 40.0 && (node.y - y).abs() < 40.0);
        if !occupied {
            return (x, y);
        }
    }
    (GRID_ORIGIN.0, GRID_ORIGIN.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(scene: &mut Scene, label: &str, author: Author) -> String {
        place_node(
            scene,
            &NodePatch {
                label: Some(label.to_string()),
                ..Default::default()
            },
            &author,
        )
        .expect("place node")
    }

    fn draft(reference: &str, label: &str) -> NodeDraft {
        NodeDraft {
            reference: Some(reference.to_string()),
            patch: NodePatch {
                label: Some(label.to_string()),
                ..Default::default()
            },
        }
    }

    fn edge(from: &str, to: &str) -> LinkDraft {
        LinkDraft {
            from: from.to_string(),
            to: to.to_string(),
            label: "depends on".to_string(),
        }
    }

    // ── diff: two documents, side by side ─────────────────────────────────

    #[test]
    fn a_diff_names_what_was_added_removed_changed_and_moved() {
        let mut scene = Scene::new();
        let host = node(&mut scene, "Atlas host", Author::Agent);
        let persistence = node(&mut scene, "Persistence", Author::Agent);
        let doomed = node(&mut scene, "Scratch", Author::Agent);
        let edge = link(&mut scene, &host, &persistence, "owns", &Author::Agent).unwrap();
        let before = read(&scene).unwrap();

        place_node(
            &mut scene,
            &NodePatch {
                id: Some(persistence.clone()),
                x: Some(before.node(&persistence).unwrap().x + 40.0),
                y: Some(before.node(&persistence).unwrap().y - 12.0),
                status: Some("disputed".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        remove(&scene, &doomed).unwrap();
        let arrived = node(&mut scene, "Activity journal", Author::Agent);
        let claim = claims::claim(
            &mut scene,
            &persistence,
            "The journal writes before acknowledging",
            "inferred",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        link(&mut scene, &persistence, &arrived, "owns", &Author::Agent).unwrap();
        let after = read(&scene).unwrap();

        let sentences = before.diff(&after);
        let has = |wanted: &str| {
            assert!(
                sentences.iter().any(|line| line == wanted),
                "wanted {wanted:?} in:\n{}",
                sentences.join("\n")
            )
        };
        has(&format!("added node \"Activity journal\" [{arrived}]"));
        has(&format!("removed node \"Scratch\" [{doomed}]"));
        has(&format!(
            "changed node \"Persistence\" [{persistence}]: status \"open\" -> \"disputed\""
        ));
        has(&format!(
            "moved node \"Persistence\" [{persistence}] 40 right and 12 up"
        ));
        has(&format!("added claim [{claim}] on \"Persistence\""));
        assert!(
            sentences
                .iter()
                .any(|line| line.starts_with("added edge") && line.contains("\"Activity journal\"")),
            "{sentences:?}"
        );
        assert!(
            !sentences.iter().any(|line| line.contains(&edge)),
            "an untouched edge says nothing: {sentences:?}"
        );

        // The diff is directional: read the other way round, the same two
        // documents swap added for removed.
        let back = after.diff(&before);
        assert!(back
            .iter()
            .any(|line| line == &format!("removed node \"Activity journal\" [{arrived}]")));
        assert!(back
            .iter()
            .any(|line| line == &format!("added node \"Scratch\" [{doomed}]")));
        assert!(back.iter().any(|line| line
            == &format!("moved node \"Persistence\" [{persistence}] 40 left and 12 down")));
    }

    #[test]
    fn a_diff_reports_a_resized_card_and_a_redrawn_stroke() {
        let mut scene = Scene::new();
        let card = node(&mut scene, "Persistence", Author::Agent);
        let stroke = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("ink".to_string()),
                points: Some("0,0 10,10".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        let before = read(&scene).unwrap();
        measure_node(&scene, &card, before.node(&card).unwrap().h + 60.0).unwrap();
        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(stroke.clone()),
                points: Some("0,0 10,10 30,4".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .unwrap();
        let after = read(&scene).unwrap();
        let sentences = before.diff(&after);
        assert!(
            sentences
                .iter()
                .any(|line| line.starts_with(&format!("resized node \"Persistence\" [{card}] "))),
            "{sentences:?}"
        );
        assert!(
            sentences
                .iter()
                .any(|line| *line == format!("redrawn shape ink [{stroke}]")),
            "{sentences:?}"
        );
    }

    #[test]
    fn two_documents_that_say_the_same_thing_diff_to_nothing() {
        let mut scene = Scene::new();
        let card = node(&mut scene, "Persistence", Author::Agent);
        claims::claim(
            &mut scene,
            &card,
            "writes first",
            "assumed",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        let before = read(&scene).unwrap();
        let again = read(&scene).unwrap();
        assert!(before.diff(&again).is_empty());
        assert_eq!(before.fingerprint(), again.fingerprint());

        // Reading the same scene twice is the easy case. The real claim is
        // that the fingerprint follows the content: one changed word and the
        // two documents stop matching.
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(card),
                label: Some("Persistence layer".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .unwrap();
        let after = read(&scene).unwrap();
        assert_ne!(before.fingerprint(), after.fingerprint());
        assert!(!before.diff(&after).is_empty());
    }

    #[test]
    fn a_diff_receipt_is_stable_over_the_same_two_documents() {
        let mut scene = Scene::new();
        let card = node(&mut scene, "Persistence", Author::Agent);
        let before = read(&scene).unwrap();
        claims::claim(
            &mut scene,
            &card,
            "writes first",
            "assumed",
            "",
            "",
            &Author::Agent,
        )
        .unwrap();
        let after = read(&scene).unwrap();

        let receipt = before.diff_receipt(&after);
        assert_eq!(receipt.sentences, before.diff(&after));
        assert_eq!(receipt.before, before.fingerprint());
        assert_eq!(receipt.after, after.fingerprint());
        assert_eq!(receipt, before.diff_receipt(&after), "recomputes the same");
        assert_eq!(receipt.receipt.len(), 64, "a sha256 in hex");

        let swapped = after.diff_receipt(&before);
        assert_ne!(
            receipt.receipt, swapped.receipt,
            "which document came first is part of what was signed"
        );
        assert_eq!(receipt.before, swapped.after);
    }

    // ── the style register ────────────────────────────────────────────────

    fn styled(scene: &mut Scene, label: &str, patch: NodePatch) -> String {
        place_node(
            scene,
            &NodePatch {
                label: Some(label.to_string()),
                ..patch
            },
            &Author::Agent,
        )
        .expect("place styled node")
    }

    #[test]
    fn a_styled_node_survives_a_write_and_read_round_trip() {
        // Also the guard on the two prefixed CRDT keys: `color` is reserved
        // numeric by the scene and `kind` is the object discriminator, so
        // renaming either back to its bare form fails right here rather than
        // silently on a live page.
        let mut scene = Scene::new();
        let id = styled(
            &mut scene,
            "Silent drift",
            NodePatch {
                color: Some("red".to_string()),
                emphasis: Some("strong".to_string()),
                size: Some("hero".to_string()),
                kind: Some("diamond".to_string()),
                ..Default::default()
            },
        );
        let atlas = read(&scene).expect("read");
        let drift = atlas.node(&id).expect("node");
        assert_eq!(drift.color, "red");
        assert_eq!(drift.emphasis, "strong");
        assert_eq!(drift.size, "hero");
        assert_eq!(drift.kind, "diamond");
        // The card is still a card: styling did not displace anything.
        assert_eq!(drift.label, "Silent drift");
        assert_eq!(drift.tone, "concept");
        assert_eq!(drift.status, "open");

        // Defaults for a card that chose nothing.
        let plain_id = node(&mut scene, "Plain", Author::Agent);
        let atlas = read(&scene).expect("read");
        let plain = atlas.node(&plain_id).expect("plain");
        assert_eq!(plain.color, "");
        assert_eq!(plain.emphasis, DEFAULT_EMPHASIS);
        assert_eq!(plain.size, DEFAULT_SIZE);
        assert_eq!(plain.kind, DEFAULT_KIND);
    }

    #[test]
    fn only_styles_that_carry_meaning_reach_the_read_back() {
        let mut scene = Scene::new();
        let plain = node(&mut scene, "Plain", Author::Agent);
        let loud = styled(
            &mut scene,
            "Loud",
            NodePatch {
                emphasis: Some("strong".to_string()),
                color: Some("amber".to_string()),
                ..Default::default()
            },
        );
        // A style set explicitly TO its default is still not a claim: the
        // picture looks identical to a card that said nothing.
        let defaulted = styled(
            &mut scene,
            "Defaulted",
            NodePatch {
                emphasis: Some(DEFAULT_EMPHASIS.to_string()),
                size: Some(DEFAULT_SIZE.to_string()),
                kind: Some(DEFAULT_KIND.to_string()),
                ..Default::default()
            },
        );
        let atlas = read(&scene).expect("read");
        assert!(atlas.node(&plain).expect("plain").style_words().is_empty());
        assert!(
            atlas
                .node(&defaulted)
                .expect("defaulted")
                .style_words()
                .is_empty(),
            "an explicitly defaulted style is not a claim"
        );
        assert_eq!(
            atlas.node(&loud).expect("loud").style_words(),
            vec!["emphasis=strong".to_string(), "color=amber".to_string()]
        );

        let described = atlas.describe();
        let line = |label: &str| {
            described
                .lines()
                .find(|line| line.contains(&format!("\"{label}\"")))
                .unwrap_or_else(|| panic!("no line for {label}: {described}"))
        };
        assert!(line("Loud").contains("emphasis=strong"), "{described}");
        assert!(line("Loud").contains("color=amber"), "{described}");
        assert!(!line("Plain").contains("emphasis"), "{described}");
        assert!(!line("Plain").contains("kind="), "{described}");
        assert!(!line("Defaulted").contains("emphasis"), "{described}");
        // The palette name comes back as the word, never as hex: a model that
        // reads back `#f87171` cannot see its own taxonomy.
        assert!(!described.contains('#'), "{described}");
    }

    #[test]
    fn a_style_outside_the_vocabulary_is_refused_and_stores_nothing() {
        let mut scene = Scene::new();
        let id = node(&mut scene, "Card", Author::Agent);
        let refuse = |patch: NodePatch, scene: &mut Scene| -> String {
            place_node(
                scene,
                &NodePatch {
                    id: Some(id.clone()),
                    ..patch
                },
                &Author::Agent,
            )
            .expect_err("an unknown style word must be refused")
        };
        let error = refuse(
            NodePatch {
                emphasis: Some("shouty".to_string()),
                ..Default::default()
            },
            &mut scene,
        );
        assert!(error.contains("emphasis"), "{error}");
        let error = refuse(
            NodePatch {
                size: Some("enormous".to_string()),
                ..Default::default()
            },
            &mut scene,
        );
        assert!(error.contains("size"), "{error}");
        let error = refuse(
            NodePatch {
                kind: Some("hexagon".to_string()),
                ..Default::default()
            },
            &mut scene,
        );
        assert!(error.contains("kind"), "{error}");
        // A hex colour is refused at the model: the palette is closed, and
        // snapping an arbitrary colour is the authoring tool's job so the
        // caller is told what it got.
        let error = refuse(
            NodePatch {
                color: Some("#ff0000".to_string()),
                ..Default::default()
            },
            &mut scene,
        );
        assert!(error.contains("color"), "{error}");

        let atlas = read(&scene).expect("read");
        let node = atlas.node(&id).expect("node");
        assert_eq!(node.emphasis, DEFAULT_EMPHASIS);
        assert_eq!(node.size, DEFAULT_SIZE);
        assert_eq!(node.kind, DEFAULT_KIND);
        assert_eq!(node.color, "");
    }

    #[test]
    fn an_empty_colour_takes_a_card_back_out_of_its_category() {
        let mut scene = Scene::new();
        let id = styled(
            &mut scene,
            "Card",
            NodePatch {
                color: Some("green".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(
            read(&scene).expect("read").node(&id).expect("node").color,
            "green"
        );

        // A patch that never mentions colour leaves it alone.
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id.clone()),
                note: Some("still green".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("note only");
        assert_eq!(
            read(&scene).expect("read").node(&id).expect("node").color,
            "green"
        );

        // An empty one clears it.
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id.clone()),
                color: Some(String::new()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("clear colour");
        assert_eq!(
            read(&scene).expect("read").node(&id).expect("node").color,
            ""
        );
    }

    #[test]
    fn a_style_word_this_build_does_not_know_falls_back_to_the_default() {
        // A replica on a newer vocabulary is the realistic source of this. An
        // unknown emphasis draws as nothing, so reading it back as a claim
        // would report an emphasis the picture is not making.
        let mut node = Node {
            id: "a".to_string(),
            label: "A".to_string(),
            emphasis: "thunderous".to_string(),
            kind: "hexagon".to_string(),
            color: "chartreuse".to_string(),
            size: "colossal".to_string(),
            ..blank_node()
        };
        node.emphasis = one_of_or(node.emphasis.clone(), EMPHASES, DEFAULT_EMPHASIS);
        node.kind = one_of_or(node.kind.clone(), NODE_KINDS, DEFAULT_KIND);
        node.size = one_of_or(node.size.clone(), SIZES, DEFAULT_SIZE);
        node.color = one_of_or(node.color.clone(), INKS, "");
        assert_eq!(node.emphasis, DEFAULT_EMPHASIS);
        assert_eq!(node.kind, DEFAULT_KIND);
        assert_eq!(node.size, DEFAULT_SIZE);
        assert_eq!(node.color, "");
        assert!(node.style_words().is_empty());
    }

    #[test]
    fn a_restyle_reads_as_its_own_change_naming_who_and_what_moved() {
        let mut scene = Scene::new();
        let id = node(&mut scene, "Silent drift", Author::Agent);

        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id.clone()),
                emphasis: Some("strong".to_string()),
                color: Some("red".to_string()),
                size: Some("hero".to_string()),
                kind: Some("diamond".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("restyle");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        let said = lines.join("\n");
        for expected in [
            "is emphasised strong by agent; it was normal before",
            "is now coloured red by agent; it had no colour before",
            "is now hero sized by agent; it was normal before",
            "is now drawn as a diamond by agent; it was a rect before",
        ] {
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("Silent drift") && line.contains(expected)),
                "missing {expected:?}: {said}"
            );
        }

        // The human taking the emphasis back off is attributed to the human.
        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id.clone()),
                emphasis: Some("normal".to_string()),
                color: Some("blue".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("unstyle");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        let said = lines.join("\n");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("is no longer emphasised by human; it was strong before")),
            "{said}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("is now coloured blue by human; it was red before")),
            "{said}"
        );

        // And losing the colour altogether says so.
        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id.clone()),
                color: Some(String::new()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("clear colour");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(
            lines
                .iter()
                .any(|line| line.contains("lost its blue colour, set by human")),
            "{}",
            lines.join("\n")
        );
    }

    #[test]
    fn a_restyle_that_changes_nothing_is_not_a_change() {
        let mut scene = Scene::new();
        let id = styled(
            &mut scene,
            "Card",
            NodePatch {
                emphasis: Some("strong".to_string()),
                ..Default::default()
            },
        );
        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id),
                emphasis: Some("strong".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("restate the same style");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(
            !lines.iter().any(|line| line.contains("emphasis")),
            "restating a style reported a change: {}",
            lines.join("\n")
        );
    }

    // ── containment ───────────────────────────────────────────────────────

    fn contained(scene: &mut Scene, label: &str, parent: &str) -> String {
        place_node(
            scene,
            &NodePatch {
                label: Some(label.to_string()),
                parent: Some(parent.to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("place contained node")
    }

    #[test]
    fn a_container_can_own_a_container() {
        let mut scene = Scene::new();
        let runtime = node(&mut scene, "Runtime", Author::Agent);
        let agent_ink = contained(&mut scene, "Agent ink", &runtime);
        let solve = contained(&mut scene, "Solve", &agent_ink);
        let worker = contained(&mut scene, "Run provider", &runtime);
        let loose = node(&mut scene, "Client", Author::Agent);
        set_node_diagram_kind(&scene, &runtime, "hierarchy", &Author::Agent)
            .expect("mark hierarchy");

        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.node(&agent_ink).expect("agent ink").parent, runtime);
        assert_eq!(atlas.node(&solve).expect("solve").parent, agent_ink);
        assert_eq!(atlas.node(&runtime).expect("runtime").parent, "");
        assert_eq!(atlas.children_of(&runtime).len(), 2);
        assert_eq!(atlas.depth(&worker), 1);
        assert_eq!(atlas.depth(&solve), 2);
        assert_eq!(atlas.depth(&runtime), 0);
        assert_eq!(atlas.deepest_containment(), 2);
        assert!(atlas.contains_node(&runtime, &solve));
        assert!(!atlas.contains_node(&worker, &runtime));
        assert_eq!(atlas.roots().len(), 2);

        let described = atlas.describe();
        assert!(
            described.contains("NODES (deepest containment: 2 of 6)"),
            "{described}"
        );
        let runtime_line = described
            .lines()
            .find(|line| line.contains("\"Runtime\""))
            .expect("runtime line");
        assert!(runtime_line.starts_with("- ["), "{described}");
        assert!(runtime_line.contains("CONTAINS 2"), "{described}");
        assert!(
            runtime_line.contains("diagram_kind=hierarchy"),
            "{described}"
        );
        let agent_ink_line = described
            .lines()
            .find(|line| line.contains("\"Agent ink\""))
            .expect("agent ink line");
        assert!(agent_ink_line.starts_with("  - ["), "{described}");
        assert!(agent_ink_line.contains("CONTAINS 1"), "{described}");
        let solve_line = described
            .lines()
            .find(|line| line.contains("\"Solve\""))
            .expect("solve line");
        assert!(solve_line.starts_with("    - ["), "{described}");
        let client_line = described
            .lines()
            .find(|line| line.contains("\"Client\""))
            .expect("client line");
        assert!(client_line.starts_with("- ["), "{described}");
        assert!(!client_line.contains("CONTAINS"), "{described}");
        assert_eq!(loose, atlas.node(&loose).expect("loose").id);
    }

    #[test]
    fn a_link_across_two_levels_names_both_containers() {
        let mut scene = Scene::new();
        let input = node(&mut scene, "Input", Author::Agent);
        let runtime = node(&mut scene, "Runtime", Author::Agent);
        let client = contained(&mut scene, "Client", &input);
        let policy = contained(&mut scene, "Check policy", &runtime);
        link(&mut scene, &client, &policy, "authorize", &Author::Agent).expect("link");

        let described = read(&scene).expect("read").describe();
        assert!(
            described.contains(
                "\"Client\" (inside \"Input\") -> \"Check policy\" (inside \"Runtime\") : authorize"
            ),
            "{described}"
        );
    }

    #[test]
    fn a_containment_cycle_is_refused() {
        let mut scene = Scene::new();
        let outer = node(&mut scene, "Outer", Author::Agent);
        let middle = contained(&mut scene, "Middle", &outer);
        let inner = contained(&mut scene, "Inner", &middle);

        // Direct: a node inside its own child.
        let error = place_node(
            &mut scene,
            &NodePatch {
                id: Some(outer.clone()),
                parent: Some(middle.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("a one-step cycle must be refused");
        assert!(error.contains("its own container"), "{error}");
        assert!(
            error.contains("Outer") && error.contains("Middle"),
            "{error}"
        );

        // Two steps up, which is the case a naive parent != id check misses.
        let error = place_node(
            &mut scene,
            &NodePatch {
                id: Some(outer.clone()),
                parent: Some(inner.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("a two-step cycle must be refused");
        assert!(error.contains("has to stay a tree"), "{error}");

        // Its own parent.
        let error = place_node(
            &mut scene,
            &NodePatch {
                id: Some(middle.clone()),
                parent: Some(middle.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("self-containment must be refused");
        assert!(error.contains("cannot contain itself"), "{error}");

        // A parent that is not on the atlas at all.
        let error = place_node(
            &mut scene,
            &NodePatch {
                id: Some(middle.clone()),
                parent: Some("no-such-node".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("a missing parent must be refused");
        assert!(error.contains("no node \"no-such-node\""), "{error}");

        // Nothing above was stored: the tree is exactly as it was.
        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.node(&outer).expect("outer").parent, "");
        assert_eq!(atlas.node(&middle).expect("middle").parent, outer);
        assert_eq!(atlas.node(&inner).expect("inner").parent, middle);
    }

    #[test]
    fn depth_past_the_limit_is_refused() {
        let mut scene = Scene::new();
        let root = node(&mut scene, "Root", Author::Agent);
        let mut parent = root;
        let mut levels = Vec::new();
        for depth in 1..=HIERARCHY_MAX_DEPTH {
            parent = contained(&mut scene, &format!("Level {depth}"), &parent);
            levels.push(parent.clone());
        }

        let before = read(&scene).expect("read before overflow");
        assert_eq!(before.deepest_containment(), HIERARCHY_MAX_DEPTH);
        assert!(
            before
                .describe()
                .contains("NODES (deepest containment: 6 of 6)"),
            "{}",
            before.describe()
        );
        let count = before.nodes.len();

        let error = place_node(
            &mut scene,
            &NodePatch {
                label: Some("Too deep".to_string()),
                parent: Some(parent),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("depth seven must be refused");
        assert!(error.contains("HIERARCHY_MAX_DEPTH 6"), "{error}");
        assert!(error.contains("attempted depth 7"), "{error}");
        assert!(
            error.contains("\"Root\" -> \"Level 1\"")
                && error.ends_with("\"Level 6\" -> \"Too deep\""),
            "{error}"
        );
        assert_eq!(
            read(&scene).expect("read after refusal").nodes.len(),
            count,
            "a refused depth write stored a node"
        );

        // Reparenting checks the deepest member of the moved subtree, not
        // only the container itself.
        let movable = node(&mut scene, "Movable", Author::Agent);
        let leaf = contained(&mut scene, "Leaf", &movable);
        let error = place_node(
            &mut scene,
            &NodePatch {
                id: Some(movable.clone()),
                parent: Some(levels[4].clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("a subtree reaching depth seven must be refused");
        assert!(error.contains("attempted depth 7"), "{error}");
        assert!(error.ends_with("\"Movable\" -> \"Leaf\""), "{error}");
        let after = read(&scene).expect("read after subtree refusal");
        assert_eq!(after.node(&movable).expect("movable").parent, "");
        assert_eq!(after.node(&leaf).expect("leaf").parent, movable);
    }

    #[test]
    fn removing_a_container_lifts_its_members_one_level() {
        let mut scene = Scene::new();
        let outer = node(&mut scene, "Platform", Author::Agent);
        let middle = contained(&mut scene, "Runtime", &outer);
        let queue = contained(&mut scene, "Queue turn", &middle);
        let worker = contained(&mut scene, "Run provider", &middle);
        let top = node(&mut scene, "Client", Author::Agent);
        let orphan_parent = contained(&mut scene, "Detached", &top);

        let before = read(&scene).expect("read").digest();

        // A container in the middle: its children take its own parent.
        let removed = remove(&scene, &middle).expect("remove middle container");
        assert_eq!(removed, 1, "only the container itself is deleted");
        let after = read(&scene).expect("read");
        assert_eq!(after.node(&queue).expect("queue").parent, outer);
        assert_eq!(after.node(&worker).expect("worker").parent, outer);
        assert_eq!(after.children_of(&outer).len(), 2);

        let lines = changes(&before, &after.digest());
        let said = lines.join("\n");
        assert!(
            lines.iter().any(|line| line.contains("Queue turn")
                && line.contains("moved up into")
                && line.contains("\"Platform\"")
                && line.contains("its container \"Runtime\" was deleted")
                && line.contains("it is now 1 level deep, was 2")),
            "{said}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Run provider") && line.contains("moved up into")),
            "{said}"
        );

        // A top-level container: its children land at the top level.
        let before = after.digest();
        remove(&scene, &top).expect("remove top container");
        let after = read(&scene).expect("read");
        assert_eq!(after.node(&orphan_parent).expect("detached").parent, "");
        let lines = changes(&before, &after.digest());
        let said = lines.join("\n");
        assert!(
            lines.iter().any(|line| line.contains("Detached")
                && line.contains("moved up to the top level")
                && line.contains("its container \"Client\" was deleted")
                && line.contains("it is now 0 levels deep, was 1")),
            "{said}"
        );
    }

    #[test]
    fn a_reparent_is_reported_as_the_relation_that_changed() {
        let mut scene = Scene::new();
        let platform = node(&mut scene, "Platform", Author::Agent);
        let input = contained(&mut scene, "Input", &platform);
        let runtime = node(&mut scene, "Runtime", Author::Agent);
        let browser = contained(&mut scene, "Browser", &runtime);
        let browser_peer = contained(&mut scene, "Browser peer", &browser);
        let card = contained(&mut scene, "Agent ink", &input);
        let solve = contained(&mut scene, "Solve", &card);

        // Agent moves a container between containers at different depths.
        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(card.clone()),
                parent: Some(browser_peer.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("reparent");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        let said = lines.join("\n");
        assert!(
            lines.iter().any(|line| line.contains("Agent ink")
                && line.contains("was moved into \"Browser peer\" by agent")
                && line.contains("it was inside \"Input\" before")
                && line.contains("it is now 3 levels deep, was 2")),
            "{said}"
        );
        let atlas = read(&scene).expect("read moved container");
        assert_eq!(atlas.depth(&card), 3);
        assert_eq!(atlas.depth(&solve), 4);

        // Human takes it out to the top level.
        let before = atlas.digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(card.clone()),
                parent: Some(String::new()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("detach");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        let said = lines.join("\n");
        assert!(
            lines.iter().any(|line| line.contains("Agent ink")
                && line.contains("was taken out of \"Browser peer\" by human")
                && line.contains("now at the top level")
                && line.contains("it is now 0 levels deep, was 3")),
            "{said}"
        );

        // Human puts it back inside one.
        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(card.clone()),
                parent: Some(input.clone()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("attach");
        let lines = changes(&before, &read(&scene).expect("read").digest());
        let said = lines.join("\n");
        assert!(
            lines.iter().any(|line| line.contains("Agent ink")
                && line.contains("was put inside \"Input\" by human")
                && line.contains("it was at the top level")
                && line.contains("it is now 2 levels deep, was 0")),
            "{said}"
        );
    }

    #[test]
    fn a_patch_that_does_not_mention_the_parent_leaves_containment_alone() {
        let mut scene = Scene::new();
        let runtime = node(&mut scene, "Runtime", Author::Agent);
        let card = contained(&mut scene, "Queue turn", &runtime);
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(card.clone()),
                note: Some("holds the turn until a worker is free".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("note only");
        assert_eq!(
            read(&scene)
                .expect("read")
                .node(&card)
                .expect("card")
                .parent,
            runtime,
            "an unrelated field write emptied the containment"
        );
    }

    #[test]
    fn a_batch_can_create_a_container_and_fill_it_in_one_call() {
        let mut scene = Scene::new();
        let drawn = draw(
            &mut scene,
            &[
                draft("runtime", "Runtime"),
                NodeDraft {
                    reference: Some("queue".to_string()),
                    patch: NodePatch {
                        label: Some("Queue turn".to_string()),
                        parent: Some("runtime".to_string()),
                        ..Default::default()
                    },
                },
            ],
            &[],
            &Author::Agent,
        )
        .expect("batch with containment");
        let container = drawn.resolve("runtime").expect("container id").to_string();
        let child = drawn.resolve("queue").expect("child id").to_string();
        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.node(&child).expect("child").parent, container);

        // Ordered the other way round it is refused before anything lands,
        // rather than silently dropping the containment.
        let mut fresh = Scene::new();
        let error = draw(
            &mut fresh,
            &[
                NodeDraft {
                    reference: Some("queue".to_string()),
                    patch: NodePatch {
                        label: Some("Queue turn".to_string()),
                        parent: Some("runtime".to_string()),
                        ..Default::default()
                    },
                },
                draft("runtime", "Runtime"),
            ],
            &[],
            &Author::Agent,
        )
        .expect_err("a container listed after its members must be refused");
        assert!(error.contains("list a container before"), "{error}");
        assert!(read(&fresh).expect("read").nodes.is_empty(), "{error}");

        let error = draw(
            &mut Scene::new(),
            &[NodeDraft {
                reference: Some("queue".to_string()),
                patch: NodePatch {
                    label: Some("Queue turn".to_string()),
                    parent: Some("nowhere".to_string()),
                    ..Default::default()
                },
            }],
            &[],
            &Author::Agent,
        )
        .expect_err("an unknown parent must be refused");
        assert!(error.contains("neither a node on the atlas"), "{error}");
    }

    #[test]
    fn a_merged_containment_cycle_is_healed_to_the_top_level_at_projection() {
        // Two replicas reparenting concurrently is the one way a ring reaches
        // the document without any single write being refusable, so the
        // projection has to survive it rather than the writer preventing it.
        let mut nodes = vec![
            Node {
                id: "a".to_string(),
                label: "A".to_string(),
                parent: "b".to_string(),
                ..blank_node()
            },
            Node {
                id: "b".to_string(),
                label: "B".to_string(),
                parent: "a".to_string(),
                ..blank_node()
            },
            Node {
                id: "c".to_string(),
                label: "C".to_string(),
                parent: "b".to_string(),
                ..blank_node()
            },
            Node {
                id: "d".to_string(),
                label: "D".to_string(),
                parent: "vanished".to_string(),
                ..blank_node()
            },
        ];
        heal_containment(&mut nodes);
        assert_eq!(nodes[0].parent, "", "the ring must be broken");
        assert_eq!(nodes[1].parent, "", "the ring must be broken");
        assert_eq!(
            nodes[2].parent, "b",
            "a node hanging off a ring keeps its parent once the ring is gone"
        );
        assert_eq!(nodes[3].parent, "", "a dangling parent must be healed");

        let atlas = Atlas {
            nodes,
            ..Atlas::default()
        };
        assert_eq!(atlas.ancestors("c"), vec!["b".to_string()]);
        assert_eq!(atlas.roots().len(), 3);
    }

    fn blank_node() -> Node {
        Node {
            id: String::new(),
            label: String::new(),
            path: String::new(),
            lines: String::new(),
            note: String::new(),
            tone: "concept".to_string(),
            status: "open".to_string(),
            cemented: String::new(),
            x: 0.0,
            y: 0.0,
            w: DEFAULT_WIDTH,
            h: 80.0,
            measured: true,
            created_by: "agent".to_string(),
            touched_by: "agent".to_string(),
            parent: String::new(),
            diagram_kind: String::new(),
            state_initial: false,
            state_terminal: false,
            state_variable: String::new(),
            state_previous_value: None,
            color: String::new(),
            emphasis: DEFAULT_EMPHASIS.to_string(),
            size: DEFAULT_SIZE.to_string(),
            kind: DEFAULT_KIND.to_string(),
        }
    }

    #[test]
    fn a_card_inside_its_container_is_not_reported_as_an_overlap() {
        let mut scene = Scene::new();
        let container = place_node(
            &mut scene,
            &NodePatch {
                label: Some("Runtime".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                w: Some(600.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("container");
        measure_node(&scene, &container, 400.0).expect("measure container");
        let inside = place_node(
            &mut scene,
            &NodePatch {
                label: Some("Queue turn".to_string()),
                parent: Some(container.clone()),
                x: Some(140.0),
                y: Some(160.0),
                w: Some(200.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("inside");
        measure_node(&scene, &inside, 100.0).expect("measure inside");

        let atlas = read(&scene).expect("read");
        let report = layout(&atlas);
        assert!(
            !report
                .problems
                .iter()
                .any(|problem| problem.detail.contains("OVERLAP")),
            "containment reported as a collision: {:?}",
            report.problems
        );

        // The same two boxes with no containment between them still collide,
        // so the suppression is about the relation and not about the geometry.
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(inside.clone()),
                parent: Some(String::new()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("detach");
        let report = layout(&read(&scene).expect("read"));
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem.detail.contains("OVERLAP")),
            "the same overlap vanished without containment: {:?}",
            report.problems
        );
    }

    /// A node with a known box, so a test can reason about what a stroke
    /// drawn over it should mean.
    fn node_at(scene: &mut Scene, label: &str, x: f64, y: f64) -> String {
        let id = place_node(
            scene,
            &NodePatch {
                label: Some(label.to_string()),
                x: Some(x),
                y: Some(y),
                w: Some(200.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("place node");
        measure_node(scene, &id, 100.0).expect("measure");
        id
    }

    fn text_at(scene: &mut Scene, label: &str, x: f64, y: f64) -> String {
        place_shape(
            scene,
            &ShapePatch {
                form: Some("text".to_string()),
                label: Some(label.to_string()),
                x: Some(x),
                y: Some(y),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place text")
    }

    #[test]
    fn viewport_reading_separates_fully_visible_partial_and_outside_nodes() {
        let mut scene = Scene::new();
        let inside = node_at(&mut scene, "fully visible", 100.0, 100.0);
        let partial = node_at(&mut scene, "clipped by the edge", 350.0, 100.0);
        let outside = node_at(&mut scene, "off screen", 900.0, 100.0);
        let atlas = read(&scene).expect("read");

        let reading = viewport_reading(
            &atlas,
            ViewportRect::new(50.0, 50.0, 350.0, 200.0).expect("viewport"),
        );

        assert_eq!(reading.inside, vec![inside]);
        assert_eq!(reading.partial, vec![partial]);
        assert!(!reading.inside.contains(&outside));
        assert!(!reading.partial.contains(&outside));
    }

    fn stroke(scene: &mut Scene, points: &[(f64, f64)], author: Author) -> String {
        place_shape(
            scene,
            &ShapePatch {
                form: Some("ink".to_string()),
                points: Some(format_points(points)),
                ..Default::default()
            },
            &author,
        )
        .expect("place shape")
    }

    /// A closed ring of `count` points around a centre.
    fn ring(center: (f64, f64), rx: f64, ry: f64, count: usize) -> Vec<(f64, f64)> {
        (0..=count)
            .map(|step| {
                let angle = std::f64::consts::TAU * step as f64 / count as f64;
                (center.0 + angle.cos() * rx, center.1 + angle.sin() * ry)
            })
            .collect()
    }

    #[test]
    fn a_circle_around_three_nodes_reads_as_enclosing_them() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "tools/list", 100.0, 100.0);
        let b = node_at(&mut scene, "tools/call", 100.0, 260.0);
        let c = node_at(&mut scene, "audience", 100.0, 420.0);
        let far = node_at(&mut scene, "unrelated", 1400.0, 100.0);

        // One gesture: a loop drawn around the left-hand column.
        let lasso = stroke(
            &mut scene,
            &ring((200.0, 320.0), 420.0, 460.0, 48),
            Author::Human,
        );

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == lasso)
            .expect("a reading for the stroke");

        assert_eq!(reading.relation, "encloses");
        for id in [&a, &b, &c] {
            assert!(
                reading.targets.contains(id),
                "expected the loop to enclose {id}, got {:?}",
                reading.targets
            );
        }
        assert!(
            !reading.targets.contains(&far),
            "a node outside the loop must not be swept in"
        );

        // And the agent is told the claim, not the coordinates.
        let described = atlas.describe();
        assert!(described.contains("ENCLOSES"), "{described}");
        assert!(described.contains("\"tools/list\""), "{described}");
    }

    #[test]
    fn an_open_squiggle_encircles_nothing() {
        let mut scene = Scene::new();
        node_at(&mut scene, "tools/list", 100.0, 100.0);
        // A long open arc that passes around the node but never closes. The
        // temptation is to treat "the node is inside the convex hull" as
        // enclosure; that invents an intent the human did not express.
        let arc: Vec<(f64, f64)> = (0..40)
            .map(|step| {
                let angle = std::f64::consts::PI * 1.05 * step as f64 / 39.0;
                (200.0 + angle.cos() * 400.0, 150.0 + angle.sin() * 400.0)
            })
            .collect();
        let id = stroke(&mut scene, &arc, Author::Human);

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == id)
            .expect("a reading");
        assert_ne!(
            reading.relation, "encloses",
            "an unclosed stroke must not be reported as grouping anything"
        );
    }

    #[test]
    fn a_scribble_over_one_node_reads_as_crossing_it_out() {
        let mut scene = Scene::new();
        let target = node_at(&mut scene, "ActionAudience", 100.0, 100.0);
        node_at(&mut scene, "elsewhere", 900.0, 100.0);

        // Back-and-forth across the card, staying inside it.
        let mut zigzag = Vec::new();
        for step in 0..12 {
            let t = step as f64 / 11.0;
            zigzag.push((110.0 + t * 180.0, 110.0 + (step % 2) as f64 * 70.0));
        }
        let id = stroke(&mut scene, &zigzag, Author::Human);

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == id)
            .expect("a reading");
        assert_eq!(reading.relation, "crosses-out");
        assert_eq!(reading.targets, vec![target]);
        assert!(atlas.describe().contains("CROSSES-OUT"));
    }

    fn rect_at(scene: &mut Scene, label: &str, x: f64, y: f64, w: f64, h: f64) -> String {
        place_shape(
            scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                label: (!label.is_empty()).then(|| label.to_string()),
                x: Some(x),
                y: Some(y),
                w: Some(w),
                h: Some(h),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place rect")
    }

    // ── shape-shape relations: the drawn layer reads against itself ────────

    #[test]
    fn a_circle_around_sketches_reads_as_enclosing_the_ink() {
        let mut scene = Scene::new();
        let sketch = rect_at(&mut scene, "", 150.0, 150.0, 120.0, 80.0);
        let words = text_at(&mut scene, "the hinge", 160.0, 260.0);
        let far = rect_at(&mut scene, "", 1400.0, 100.0, 120.0, 80.0);

        let lasso = stroke(
            &mut scene,
            &ring((210.0, 220.0), 260.0, 260.0, 48),
            Author::Human,
        );

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == lasso)
            .expect("a reading for the loop");
        assert_eq!(
            reading.relation, "encloses",
            "targets {:?}",
            reading.targets
        );
        assert!(reading.targets.contains(&sketch), "{:?}", reading.targets);
        assert!(reading.targets.contains(&words), "{:?}", reading.targets);
        assert!(!reading.targets.contains(&far), "{:?}", reading.targets);

        let described = atlas.describe();
        assert!(described.contains("ENCLOSES"), "{described}");
        assert!(described.contains("\"the hinge\""), "{described}");
    }

    #[test]
    fn a_circle_around_a_card_and_its_sketch_groups_both_with_the_card_first() {
        let mut scene = Scene::new();
        let card = node_at(&mut scene, "runtime kernel", 120.0, 120.0);
        let sketch = rect_at(&mut scene, "", 140.0, 260.0, 120.0, 60.0);

        let lasso = stroke(
            &mut scene,
            &ring((220.0, 240.0), 300.0, 300.0, 48),
            Author::Human,
        );

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == lasso)
            .expect("a reading for the loop");
        assert_eq!(reading.relation, "encloses");
        assert_eq!(
            reading.targets.first(),
            Some(&card),
            "cards keep first position in a mixed group: {:?}",
            reading.targets
        );
        assert!(reading.targets.contains(&sketch), "{:?}", reading.targets);
    }

    #[test]
    fn a_caption_beside_a_sketch_reads_as_near_it() {
        let mut scene = Scene::new();
        let sketch = rect_at(&mut scene, "", 100.0, 100.0, 200.0, 100.0);
        let caption = text_at(&mut scene, "start here", 320.0, 120.0);

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == caption)
            .expect("a reading for the caption");
        assert_eq!(reading.relation, "says");
        assert_eq!(reading.targets, vec![sketch]);
        let described = atlas.describe();
        assert!(
            described.contains("SAYS \"start here\" NEAR"),
            "{described}"
        );
    }

    #[test]
    fn a_caption_near_a_card_and_a_sketch_reads_the_card_only() {
        let mut scene = Scene::new();
        let card = node_at(&mut scene, "runtime kernel", 100.0, 100.0);
        rect_at(&mut scene, "", 340.0, 90.0, 200.0, 100.0);
        let caption = text_at(&mut scene, "check this", 310.0, 120.0);

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == caption)
            .expect("a reading for the caption");
        assert_eq!(
            reading.targets,
            vec![card],
            "the card register keeps priority over ink"
        );
    }

    #[test]
    fn a_scribble_over_one_sketch_reads_as_crossing_it_out() {
        let mut scene = Scene::new();
        let sketch = rect_at(&mut scene, "old plan", 100.0, 100.0, 200.0, 100.0);
        rect_at(&mut scene, "", 900.0, 100.0, 200.0, 100.0);

        let mut zigzag = Vec::new();
        for step in 0..12 {
            let t = step as f64 / 11.0;
            zigzag.push((110.0 + t * 180.0, 110.0 + (step % 2) as f64 * 70.0));
        }
        let id = stroke(&mut scene, &zigzag, Author::Human);

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == id)
            .expect("a reading");
        assert_eq!(reading.relation, "crosses-out");
        assert_eq!(reading.targets, vec![sketch]);
        assert!(atlas.describe().contains("CROSSES-OUT \"old plan\""));
    }

    #[test]
    fn an_arrow_tip_at_a_sketch_reads_as_pointing_at_it() {
        let mut scene = Scene::new();
        let sketch = rect_at(&mut scene, "the fix", 400.0, 100.0, 200.0, 100.0);

        let id = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(150.0),
                y: Some(150.0),
                points: Some(format_points(&[(0.0, 0.0), (230.0, 0.0)])),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place arrow");

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == id)
            .expect("a reading");
        assert_eq!(reading.relation, "points-at");
        assert_eq!(reading.targets, vec![sketch]);
    }

    // ── lifting: the human's move between registers ────────────────────────

    #[test]
    fn lifting_a_labeled_shape_moves_it_into_the_node_register() {
        let mut scene = Scene::new();
        let card = node_at(&mut scene, "runtime kernel", 900.0, 100.0);
        let sketch = rect_at(&mut scene, "cold start path", 100.0, 100.0, 260.0, 120.0);
        let flag = mark(&mut scene, &sketch, "?", "is this real?", &Author::Human).expect("mark");
        let arrow = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(400.0),
                y: Some(150.0),
                points: Some(format_points(&[(0.0, 0.0), (100.0, 0.0)])),
                from: Some(sketch.clone()),
                to: Some(card.clone()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("bound arrow");
        let caption = create_constraint(
            &mut scene,
            "labels",
            std::slice::from_ref(&sketch),
            None,
            Some("measured, not guessed"),
            None,
            &Author::Human,
        )
        .expect("labels constraint");

        let lifted = lift_shape(&mut scene, &sketch, &Author::Human).expect("lift");

        let atlas = read(&scene).expect("read");
        let node = atlas.node(&lifted).expect("the lifted node");
        assert_eq!(node.label, "cold start path");
        assert_eq!(node.x, 100.0);
        assert_eq!(node.y, 100.0);
        assert_eq!(node.created_by, Author::Human.as_str());
        assert!(
            atlas.shape(&sketch).is_none(),
            "the lifted shape leaves the drawn layer"
        );
        let flag = atlas
            .marks
            .iter()
            .find(|mark| mark.id == flag)
            .expect("the mark survives");
        assert_eq!(flag.target, lifted, "the mark follows the promotion");
        let arrow = atlas.shape(&arrow).expect("the arrow survives");
        assert_eq!(arrow.from, lifted, "the binding follows the promotion");
        let caption = atlas.constraint(&caption).expect("the constraint survives");
        assert_eq!(caption.members, vec![lifted.clone()]);
        assert_eq!(caption.status, "sat", "{:?}", caption);
    }

    #[test]
    fn lifting_a_region_consumes_the_single_text_inside_as_its_title() {
        let mut scene = Scene::new();
        let sketch = rect_at(&mut scene, "", 100.0, 100.0, 400.0, 200.0);
        let words = text_at(&mut scene, "manifest cache", 150.0, 150.0);

        let lifted = lift_shape(&mut scene, &sketch, &Author::Human).expect("lift");

        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.node(&lifted).expect("node").label, "manifest cache");
        assert!(atlas.shape(&sketch).is_none());
        assert!(
            atlas.shape(&words).is_none(),
            "the consumed title leaves the drawn layer too"
        );
    }

    #[test]
    fn lifting_a_region_with_two_texts_refuses_the_ambiguity() {
        let mut scene = Scene::new();
        let sketch = rect_at(&mut scene, "", 100.0, 100.0, 400.0, 200.0);
        text_at(&mut scene, "one", 150.0, 150.0);
        text_at(&mut scene, "two", 150.0, 220.0);

        let error = lift_shape(&mut scene, &sketch, &Author::Human).expect_err("ambiguous");
        assert!(error.contains("2 texts"), "{error}");
        let atlas = read(&scene).expect("read");
        assert!(
            atlas.shape(&sketch).is_some(),
            "a refused lift changes nothing"
        );
    }

    #[test]
    fn lifting_a_connector_or_wordless_shape_is_refused() {
        let mut scene = Scene::new();
        let arrow = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                points: Some(format_points(&[(0.0, 0.0), (100.0, 0.0)])),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("arrow");
        let error = lift_shape(&mut scene, &arrow, &Author::Human).expect_err("no arrow lift");
        assert!(error.contains("relates things"), "{error}");

        let squiggle = stroke(
            &mut scene,
            &[(0.0, 0.0), (40.0, 30.0), (90.0, 5.0)],
            Author::Human,
        );
        let error = lift_shape(&mut scene, &squiggle, &Author::Human).expect_err("no words");
        assert!(error.contains("no words"), "{error}");
    }

    #[test]
    fn lifting_a_text_carries_its_words_onto_the_card() {
        let mut scene = Scene::new();
        let words = text_at(
            &mut scene,
            "the whole cold start is the manifest walk",
            100.0,
            100.0,
        );

        let lifted = lift_shape(&mut scene, &words, &Author::Human).expect("lift");

        let atlas = read(&scene).expect("read");
        let node = atlas.node(&lifted).expect("node");
        assert_eq!(node.label, "the whole cold start is the manifest walk");
        assert!(atlas.shape(&words).is_none());
    }

    #[test]
    fn a_bound_arrow_follows_the_node_the_human_drags() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "left", 100.0, 100.0);
        let b = node_at(&mut scene, "right", 700.0, 100.0);
        let arrow = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                from: Some(a.clone()),
                to: Some(b.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("bind an arrow");

        let before = {
            let atlas = read(&scene).expect("read");
            let shape = atlas.shapes.iter().find(|s| s.id == arrow).expect("shape");
            atlas.shape_endpoints(shape).expect("endpoints")
        };

        // The human drags the right-hand card down.
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(b.clone()),
                y: Some(600.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("drag");

        let atlas = read(&scene).expect("read");
        let shape = atlas.shapes.iter().find(|s| s.id == arrow).expect("shape");
        let after = atlas.shape_endpoints(shape).expect("endpoints");
        assert_ne!(
            before.1, after.1,
            "a bound endpoint must be recomputed from the node's live position"
        );
        // It stops at the card's edge, not at its centre, or the head would be
        // hidden under the box it is pointing at.
        let target = atlas.node(&b).expect("node");
        let (l, t, r, bottom) = target.bounds();
        assert!(
            after.1 .0 >= l - 1.0
                && after.1 .0 <= r + 1.0
                && after.1 .1 >= t - 1.0
                && after.1 .1 <= bottom + 1.0,
            "endpoint {:?} should sit on the box {:?}",
            after.1,
            (l, t, r, bottom)
        );

        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == arrow)
            .expect("a reading");
        assert_eq!(reading.relation, "connects");
        assert_eq!(reading.targets, vec![a, b]);
    }

    #[test]
    fn deleting_a_node_unbinds_a_drawn_arrow_but_keeps_the_drawing() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "left", 100.0, 100.0);
        let b = node_at(&mut scene, "right", 700.0, 100.0);

        // One arrow that is nothing but two bindings, and one the human drew
        // by hand that happens to be bound at its tail.
        let derived = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                from: Some(a.clone()),
                to: Some(b.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("derived arrow");
        let drawn = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                from: Some(b.clone()),
                points: Some("0,0 40,40 90,10".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("hand-drawn arrow");

        remove(&scene, &b).expect("remove the node");

        let atlas = read(&scene).expect("read");
        assert!(
            !atlas.shapes.iter().any(|shape| shape.id == derived),
            "an arrow that was only two node references has no geometry left"
        );
        let kept = atlas
            .shapes
            .iter()
            .find(|shape| shape.id == drawn)
            .expect("the hand-drawn arrow must survive");
        assert!(
            kept.from.is_empty(),
            "it should lose the binding, not the points"
        );
        assert_eq!(kept.points.len(), 3);
    }

    #[test]
    fn a_stroke_is_simplified_without_losing_its_shape() {
        // 300 samples along a right-angle path: a pointer's worth of noise
        // describing something a handful of points can say.
        let mut dense = Vec::new();
        for step in 0..150 {
            dense.push((step as f64 * 2.0, 0.0));
        }
        for step in 0..150 {
            dense.push((300.0, step as f64 * 2.0));
        }
        let simplified = simplify(&dense, SIMPLIFY_TOLERANCE);
        assert!(
            simplified.len() < 12,
            "expected a handful of points, got {}",
            simplified.len()
        );
        // The corner survives: every original point is still near the result.
        for point in &dense {
            let nearest = simplified
                .windows(2)
                .map(|pair| point_to_segment(*point, pair[0], pair[1]))
                .fold(f64::MAX, f64::min);
            assert!(nearest <= SIMPLIFY_TOLERANCE + 1e-6, "dropped {point:?}");
        }
    }

    #[test]
    fn a_stroke_outline_is_a_closed_band_around_the_path() {
        let path: Vec<(f64, f64)> = (0..20).map(|step| (step as f64 * 10.0, 0.0)).collect();
        let polygon = outline(&path, 8.0);
        assert!(
            polygon.len() > path.len(),
            "an outline has two sides and caps"
        );
        // Every vertex sits within a stroke-width of the centre line: the band
        // hugs the path instead of ballooning.
        for vertex in &polygon {
            let nearest = path
                .windows(2)
                .map(|pair| point_to_segment(*vertex, pair[0], pair[1]))
                .fold(f64::MAX, f64::min);
            assert!(
                nearest <= 8.0,
                "vertex {vertex:?} is {nearest} from the path"
            );
        }
    }

    #[test]
    fn concurrent_human_stroke_and_agent_node_both_survive() {
        // The property the whole example exists to prove, extended to the
        // drawn layer: two replicas edit blind to each other and neither edit
        // is lost.
        let mut left = Scene::new();
        let seed = node_at(&mut left, "shared", 100.0, 100.0);
        let mut right = Scene::from_state(&left.encode_full().expect("encode")).expect("clone");

        stroke(
            &mut left,
            &ring((200.0, 150.0), 300.0, 300.0, 24),
            Author::Human,
        );
        place_node(
            &mut right,
            &NodePatch {
                id: Some(seed.clone()),
                note: Some("the agent's claim".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("agent note");

        let (a, b) = (
            left.encode_full().expect("encode"),
            right.encode_full().expect("encode"),
        );
        left.apply_update(&b).expect("merge");
        right.apply_update(&a).expect("merge");

        for (side, scene) in [("left", &left), ("right", &right)] {
            let atlas = read(scene).expect("read");
            assert_eq!(atlas.shapes.len(), 1, "{side} lost the stroke");
            assert_eq!(
                atlas.node(&seed).expect("node").note,
                "the agent's claim",
                "{side} lost the note"
            );
        }
    }

    #[test]
    fn a_shape_is_described_as_a_claim_not_as_coordinates() {
        let mut scene = Scene::new();
        node_at(&mut scene, "target", 100.0, 100.0);
        stroke(
            &mut scene,
            &ring((200.0, 150.0), 300.0, 300.0, 24),
            Author::Human,
        );

        let described = read(&scene).expect("read").describe();
        assert!(described.contains("DRAWING"), "{described}");
        // The read-back must never degrade into a point dump: that is the
        // failure mode this whole section exists to prevent.
        assert!(
            !described.contains("points"),
            "the read-back leaked raw geometry:\n{described}"
        );
    }

    #[test]
    fn a_shape_only_diagram_is_readable_instead_of_reported_empty() {
        let mut scene = Scene::new();
        place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("diamond".to_string()),
                x: Some(120.0),
                y: Some(80.0),
                w: Some(180.0),
                h: Some(100.0),
                label: Some("approval gate".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("draw shape");

        let described = read(&scene).expect("read").describe();
        assert!(described.contains("1 shapes"), "{described}");
        assert!(described.contains("approval gate"), "{described}");
        assert!(!described.contains("atlas is empty"), "{described}");
    }

    #[test]
    fn a_shape_rejects_a_colour_it_cannot_render() {
        let mut scene = Scene::new();
        let error = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                ink: Some("#ff0000".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("an open colour vocabulary is a styling escape hatch");
        assert!(error.contains("ink must be one of"), "{error}");
    }

    #[test]
    fn binding_to_a_node_that_does_not_exist_is_rejected() {
        let mut scene = Scene::new();
        let error = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                from: Some("n_nope".to_string()),
                to: Some("n_also_nope".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("a dangling binding renders as nothing at all");
        assert!(error.contains("no bindable node or shape"), "{error}");
    }

    #[test]
    fn one_draw_places_a_whole_subgraph_and_links_it_by_reference() {
        let mut scene = Scene::new();
        let drawn = draw(
            &mut scene,
            &[
                draft("runtime", "ag-ui-surface"),
                draft("core", "ag-ui-core"),
            ],
            &[edge("runtime", "core")],
            &Author::Agent,
        )
        .expect("draw the subgraph");

        assert_eq!(drawn.nodes.len(), 2);
        assert_eq!(drawn.edges.len(), 1);

        let atlas = read(&scene).expect("read back");
        assert_eq!(atlas.nodes.len(), 2);
        assert_eq!(atlas.edges.len(), 1);
        // The link resolved to the ids the same call generated, not to the
        // caller's private names.
        let runtime = drawn.resolve("runtime").expect("runtime id");
        let core = drawn.resolve("core").expect("core id");
        assert_eq!(atlas.edges[0].from, runtime);
        assert_eq!(atlas.edges[0].to, core);
    }

    #[test]
    fn a_draw_can_link_a_new_node_to_one_already_on_the_atlas() {
        let mut scene = Scene::new();
        let existing = node(&mut scene, "already here", Author::Human);
        let drawn = draw(
            &mut scene,
            &[draft("fresh", "new claim")],
            &[edge("fresh", &existing)],
            &Author::Agent,
        )
        .expect("draw against an existing node");

        let atlas = read(&scene).expect("read back");
        assert_eq!(atlas.edges.len(), 1);
        assert_eq!(atlas.edges[0].to, existing);
        assert_eq!(atlas.edges[0].from, drawn.resolve("fresh").unwrap());
    }

    #[test]
    fn a_link_that_points_at_nothing_is_rejected_before_any_node_is_written() {
        let mut scene = Scene::new();
        let error = draw(
            &mut scene,
            &[draft("a", "first"), draft("b", "second")],
            &[edge("a", "typo-nobody-declared")],
            &Author::Agent,
        )
        .expect_err("an unresolvable endpoint must fail");

        assert!(error.contains("typo-nobody-declared"), "{error}");
        assert!(
            read(&scene).expect("read back").nodes.is_empty(),
            "nothing may land when the batch is rejected up front"
        );
    }

    #[test]
    fn duplicate_references_are_rejected_rather_than_silently_shadowing() {
        let mut scene = Scene::new();
        let error = draw(
            &mut scene,
            &[draft("same", "first"), draft("same", "second")],
            &[],
            &Author::Agent,
        )
        .expect_err("ambiguous references must fail");

        assert!(error.contains("two nodes"), "{error}");
        assert!(read(&scene).expect("read back").nodes.is_empty());
    }

    #[test]
    fn a_batch_that_would_overflow_the_atlas_is_refused_whole() {
        let mut scene = Scene::new();
        let oversized: Vec<NodeDraft> = (0..=MAX_OBJECTS)
            .map(|index| draft(&format!("n{index}"), "filler"))
            .collect();
        let error = draw(&mut scene, &oversized, &[], &Author::Agent)
            .expect_err("an oversized batch must fail");

        assert!(error.contains("holds"), "{error}");
        assert!(
            read(&scene).expect("read back").nodes.is_empty(),
            "capacity is checked for the whole batch, not per node"
        );
    }

    #[test]
    fn a_mid_batch_failure_says_what_already_landed() {
        let mut scene = Scene::new();
        // The second node names an id that does not exist, which `place_node`
        // only discovers while writing — after the first node has landed.
        let error = draw(
            &mut scene,
            &[
                draft("first", "this one lands"),
                NodeDraft {
                    reference: Some("second".to_string()),
                    patch: NodePatch {
                        id: Some("no-such-node".to_string()),
                        label: Some("this one cannot".to_string()),
                        ..Default::default()
                    },
                },
            ],
            &[],
            &Author::Agent,
        )
        .expect_err("updating a missing node must fail");

        assert!(
            error.contains("already on the atlas and were not undone"),
            "a partial write must not be described as no write: {error}"
        );
        assert_eq!(
            read(&scene).expect("read back").nodes.len(),
            1,
            "the read-back is authoritative and shows what really landed"
        );
    }

    #[test]
    fn an_empty_draw_is_an_error_rather_than_a_silent_no_op() {
        let mut scene = Scene::new();
        assert!(draw(&mut scene, &[], &[], &Author::Agent).is_err());
    }

    #[test]
    fn places_reads_and_describes() {
        let mut scene = Scene::new();
        let id = place_node(
            &mut scene,
            &NodePatch {
                label: Some("ag-ui-core".into()),
                path: Some("crates/ag-ui-core/src/event.rs".into()),
                lines: Some("1-40".into()),
                tone: Some("crate".into()),
                note: Some("the protocol event enum".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("place");

        let atlas = read(&scene).expect("read");
        let placed = atlas.node(&id).expect("node exists");
        assert_eq!(placed.label, "ag-ui-core");
        assert_eq!(placed.source_ref(), "crates/ag-ui-core/src/event.rs:1-40");
        assert_eq!(placed.tone, "crate");
        assert_eq!(placed.status, "open");
        assert_eq!(placed.created_by, "agent");
        assert!(atlas.describe().contains("ag-ui-core"));
    }

    #[test]
    fn auto_layout_does_not_stack_nodes() {
        let mut scene = Scene::new();
        for index in 0..6 {
            node(&mut scene, &format!("n{index}"), Author::Agent);
        }
        let atlas = read(&scene).expect("read");
        for (index, left) in atlas.nodes.iter().enumerate() {
            for right in atlas.nodes.iter().skip(index + 1) {
                assert!(
                    (left.x - right.x).abs() >= 40.0 || (left.y - right.y).abs() >= 40.0,
                    "{} and {} overlap",
                    left.label,
                    right.label
                );
            }
        }
    }

    #[test]
    fn a_container_extent_is_the_frame_around_its_content() {
        let mut scene = Scene::new();
        let container = boxed(&mut scene, "Runtime", 900.0, 900.0, 232.0, 80.0);
        let first = boxed(&mut scene, "Queue turn", 120.0, 180.0, 220.0, 90.0);
        let second = boxed(&mut scene, "Run provider", 380.0, 330.0, 260.0, 110.0);
        for child in [&first, &second] {
            place_node(
                &mut scene,
                &NodePatch {
                    id: Some(child.clone()),
                    parent: Some(container.clone()),
                    ..NodePatch::default()
                },
                &Author::Agent,
            )
            .expect("put child inside container");
        }

        let atlas = read(&scene).expect("read");
        let parent = atlas.node(&container).expect("container");
        assert_eq!(
            parent.bounds(),
            (98.0, 132.0, 662.0, 462.0),
            "the model extent must be the same FRAME_PAD + FRAME_TITLE_H box the renderer draws"
        );
        for child in [&first, &second] {
            let child = atlas.node(child).expect("child");
            let (pl, pt, pr, pb) = parent.bounds();
            let (cl, ct, cr, cb) = child.bounds();
            assert!(
                cl >= pl && ct >= pt && cr <= pr && cb <= pb,
                "{} escaped its parent: {}",
                child.label,
                atlas.describe()
            );
        }

        // Parent extent is derived from current content, not latched when the
        // hierarchy was authored. A later real renderer measurement expands
        // the frame and the model together.
        measure_node(&scene, &second, 170.0).expect("measure taller child");
        let atlas = read(&scene).expect("read after measurement");
        assert_eq!(
            atlas.node(&container).expect("container").bounds(),
            (98.0, 132.0, 662.0, 522.0)
        );
    }

    /// Place a node at an exact box, with its height measured rather than
    /// guessed, so a layout assertion is about the geometry and not about how
    /// good the estimator happens to be.
    fn boxed(scene: &mut Scene, label: &str, x: f64, y: f64, w: f64, h: f64) -> String {
        let id = place_node(
            scene,
            &NodePatch {
                label: Some(label.into()),
                x: Some(x),
                y: Some(y),
                w: Some(w),
                ..NodePatch::default()
            },
            &Author::Agent,
        )
        .expect("place");
        measure_node(scene, &id, h).expect("measure");
        id
    }

    #[test]
    fn an_arrow_binds_to_a_box_drawn_earlier_in_the_same_sketch() {
        let mut scene = Scene::new();
        let drawn = sketch(
            &mut scene,
            &[
                ShapePatch {
                    form: Some("rect".to_string()),
                    label: Some("initialize".to_string()),
                    x: Some(100.0),
                    y: Some(100.0),
                    w: Some(200.0),
                    h: Some(80.0),
                    reference: Some("a".to_string()),
                    ..ShapePatch::default()
                },
                ShapePatch {
                    form: Some("rect".to_string()),
                    label: Some("mint the seat".to_string()),
                    x: Some(500.0),
                    y: Some(100.0),
                    w: Some(200.0),
                    h: Some(80.0),
                    reference: Some("b".to_string()),
                    ..ShapePatch::default()
                },
                ShapePatch {
                    form: Some("arrow".to_string()),
                    from: Some("a".to_string()),
                    to: Some("b".to_string()),
                    ..ShapePatch::default()
                },
            ],
            &Author::Agent,
        )
        .expect("one call, bound arrow");
        let atlas = read(&scene).expect("read");
        let arrow = atlas.shape(&drawn[2]).expect("arrow");
        assert_eq!(arrow.from, drawn[0]);
        assert_eq!(arrow.to, drawn[1]);
        let (start, end) = atlas.shape_endpoints(arrow).expect("ends");
        assert!(start.0 >= 299.0 && start.0 <= 301.0, "{start:?}");
        assert!(end.0 >= 499.0 && end.0 <= 501.0, "{end:?}");

        // A frame named by ref holds the box drawn after it in the same call.
        let framed = sketch(
            &mut scene,
            &[
                ShapePatch {
                    form: Some("frame".to_string()),
                    label: Some("stage".to_string()),
                    x: Some(1000.0),
                    y: Some(1000.0),
                    w: Some(400.0),
                    h: Some(300.0),
                    reference: Some("f".to_string()),
                    ..ShapePatch::default()
                },
                ShapePatch {
                    form: Some("rect".to_string()),
                    label: Some("inside".to_string()),
                    x: Some(1050.0),
                    y: Some(1050.0),
                    w: Some(100.0),
                    h: Some(60.0),
                    frame: Some("f".to_string()),
                    ..ShapePatch::default()
                },
            ],
            &Author::Agent,
        )
        .expect("frame by ref");
        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.shape(&framed[1]).expect("box").frame, framed[0]);

        // A ref used before it is drawn is the ordinary unknown-target error.
        let error = sketch(
            &mut scene,
            &[
                ShapePatch {
                    form: Some("arrow".to_string()),
                    from: Some("c".to_string()),
                    to: Some("a".to_string()),
                    ..ShapePatch::default()
                },
                ShapePatch {
                    form: Some("rect".to_string()),
                    label: Some("late".to_string()),
                    x: Some(900.0),
                    y: Some(100.0),
                    w: Some(100.0),
                    h: Some(80.0),
                    reference: Some("c".to_string()),
                    ..ShapePatch::default()
                },
            ],
            &Author::Agent,
        )
        .expect_err("forward ref");
        assert!(error.contains("no bindable"), "{error}");
    }

    #[test]
    fn a_label_that_cannot_fit_its_box_is_a_problem() {
        let mut scene = Scene::new();
        let ids = sketch(
            &mut scene,
            &[
                ShapePatch {
                    form: Some("rect".to_string()),
                    label: Some("terminal agent: POST /mcp initialize with Authorization: Bearer <token> and clientInfo.name, which is a claim not an identity".to_string()),
                    x: Some(100.0),
                    y: Some(100.0),
                    w: Some(200.0),
                    h: Some(60.0),
                    ..ShapePatch::default()
                },
                ShapePatch {
                    form: Some("rect".to_string()),
                    label: Some("POST initialize".to_string()),
                    x: Some(500.0),
                    y: Some(100.0),
                    w: Some(200.0),
                    h: Some(60.0),
                    ..ShapePatch::default()
                },
            ],
            &Author::Agent,
        )
        .expect("two boxes");
        let problems = layout(&read(&scene).expect("read")).problems;
        let spill = problems
            .iter()
            .find(|problem| problem.ids == vec![ids[0].clone()])
            .unwrap_or_else(|| panic!("no spill problem in {problems:?}"));
        assert!(spill.detail.contains("spill"), "{}", spill.detail);
        assert!(
            !problems
                .iter()
                .any(|problem| problem.ids == vec![ids[1].clone()]),
            "a short label fits: {problems:?}"
        );
    }

    #[test]
    fn a_heading_that_wraps_into_the_caption_under_it_is_a_problem() {
        let mut scene = Scene::new();
        let heading = sketch(
            &mut scene,
            &[ShapePatch {
                form: Some("text".to_string()),
                label: Some(
                    "How a terminal agent attaches, and how its call is gated and signed"
                        .to_string(),
                ),
                x: Some(100.0),
                y: Some(100.0),
                w: Some(300.0),
                font_size: Some(36.0),
                ..ShapePatch::default()
            }],
            &Author::Agent,
        )
        .expect("heading")[0]
            .clone();
        let caption = sketch(
            &mut scene,
            &[ShapePatch {
                form: Some("text".to_string()),
                label: Some("blue = crosses the wire, amber = a gate".to_string()),
                x: Some(100.0),
                y: Some(130.0),
                w: Some(300.0),
                ..ShapePatch::default()
            }],
            &Author::Agent,
        )
        .expect("caption")[0]
            .clone();
        let atlas = read(&scene).expect("read");
        let problems = layout(&atlas).problems;
        let stacked = problems
            .iter()
            .find(|problem| problem.ids.contains(&heading) && problem.ids.contains(&caption))
            .unwrap_or_else(|| panic!("no text-on-text problem in {problems:?}"));
        assert!(stacked.detail.contains("OVERLAP"), "{}", stacked.detail);
        assert!(
            stacked.detail.contains("How a terminal agent"),
            "{}",
            stacked.detail
        );

        // Moved clear, the problem goes away rather than lingering.
        sketch(
            &mut scene,
            &[ShapePatch {
                id: Some(caption.clone()),
                y: Some(400.0),
                ..ShapePatch::default()
            }],
            &Author::Agent,
        )
        .expect("move");
        let atlas = read(&scene).expect("read");
        assert!(
            !layout(&atlas)
                .problems
                .iter()
                .any(|problem| problem.ids.contains(&heading) && problem.ids.contains(&caption)),
            "{:?}",
            layout(&atlas).problems
        );
    }

    #[test]
    fn a_note_that_makes_a_card_tall_is_reported_as_an_overlap() {
        let mut scene = Scene::new();
        // Two rows 168px apart — the spacing the grid uses — then the top card
        // grows to 200px tall because someone wrote a paragraph on it.
        let top = boxed(&mut scene, "top", 0.0, 0.0, 232.0, 84.0);
        boxed(&mut scene, "bottom", 0.0, 168.0, 232.0, 84.0);
        assert!(
            layout(&read(&scene).expect("read")).problems.is_empty(),
            "168px apart and 84px tall is not an overlap"
        );

        measure_node(&scene, &top, 200.0).expect("measure");
        let report = layout(&read(&scene).expect("read"));
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        assert!(report.problems[0].detail.contains("OVERLAP"));
        assert!(report.problems[0].ids.contains(&top));
    }

    #[test]
    fn a_routed_link_avoids_an_unrelated_node_and_reports_a_blocked_port() {
        let mut scene = Scene::new();
        let left = boxed(&mut scene, "left", 0.0, 0.0, 100.0, 100.0);
        let right = boxed(&mut scene, "right", 600.0, 0.0, 100.0, 100.0);
        let middle = boxed(&mut scene, "middle", 300.0, 0.0, 100.0, 100.0);
        link(&mut scene, &left, &right, "spans", &Author::Agent).expect("link");

        let atlas = read(&scene).expect("read");
        assert!(atlas.painting().links[0].path.len() > 2);
        assert!(!layout(&atlas)
            .problems
            .iter()
            .any(|problem| problem.detail.contains("passes under")));
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(middle.clone()),
                x: Some(90.0),
                ..NodePatch::default()
            },
            &Author::Agent,
        )
        .expect("block port");
        let blocked = layout(&read(&scene).expect("read"));
        assert!(blocked
            .problems
            .iter()
            .any(|problem| problem.detail.contains("route unavailable")));

        // Move the obstacle out of the way and the complaint goes with it.
        place_node(
            &mut scene,
            &NodePatch {
                id: Some(middle),
                y: Some(500.0),
                ..NodePatch::default()
            },
            &Author::Agent,
        )
        .expect("move");
        assert!(!layout(&read(&scene).expect("read"))
            .problems
            .iter()
            .any(|problem| problem.detail.contains("passes under")));
    }

    #[test]
    fn transition_curves_do_not_reserve_ordinary_link_ports_or_lanes() {
        let mut scene = Scene::new();
        let left = boxed(&mut scene, "left", 0.0, 0.0, 280.0, 150.0);
        let right = boxed(&mut scene, "right", 600.0, 0.0, 280.0, 150.0);
        link(&mut scene, &left, &right, "ordinary", &Author::Agent).unwrap();
        let mut atlas = read(&scene).unwrap();
        let before = atlas.painting().links[0].path.clone();
        let mut transition = atlas.edges[0].clone();
        transition.id = "first-transition".into();
        transition.event = "go".into();
        atlas.edges.insert(0, transition);
        assert_eq!(
            atlas
                .painting()
                .links
                .iter()
                .find(|e| e.label == "ordinary" && e.id != "first-transition")
                .unwrap()
                .path,
            before
        );
        assert!(!layout(&atlas)
            .problems
            .iter()
            .any(|p| p.detail.contains("share a route segment")));
    }

    #[test]
    fn painting_keeps_incoming_and_outgoing_links_on_separate_ports() {
        let mut scene = Scene::new();
        let upper = boxed(&mut scene, "upper", 0.0, 0.0, 280.0, 150.0);
        let lower = boxed(&mut scene, "lower", 0.0, 400.0, 280.0, 150.0);
        let right = boxed(&mut scene, "right", 600.0, 200.0, 280.0, 150.0);
        link(&mut scene, &lower, &right, "in", &Author::Agent).unwrap();
        link(&mut scene, &right, &upper, "out", &Author::Agent).unwrap();
        let atlas = read(&scene).unwrap();
        let painting = atlas.painting();
        let incoming = painting.links.iter().find(|p| p.label == "in").unwrap();
        let outgoing = painting.links.iter().find(|p| p.label == "out").unwrap();
        assert_eq!(incoming.ends[1].0, 600.0);
        assert_eq!(outgoing.ends[0].0, 600.0);
        assert_ne!(incoming.ends[1], outgoing.ends[0]);
        for link in &painting.links {
            assert_eq!(link.path.first(), Some(&link.ends[0]));
            assert_eq!(link.path.last(), Some(&link.ends[1]));
        }
        assert!(!incoming.path.windows(2).any(|a| outgoing
            .path
            .windows(2)
            .any(|b| routing::shared_segment(a[0], a[1], b[0], b[1]))));
        assert!(layout(&atlas).problems.is_empty());
    }

    #[test]
    fn a_measurement_does_not_look_like_a_human_edit() {
        // The whole "the human moved my node" signal rides on touched_by. A
        // browser reporting a card's height must not set it, or every node
        // reads LAST-EDITED-BY=human on the agent's next turn.
        let mut scene = Scene::new();
        let id = boxed(&mut scene, "drawn by the agent", 0.0, 0.0, 232.0, 120.0);
        let atlas = read(&scene).expect("read");
        let node = atlas.node(&id).expect("node");
        assert_eq!(node.touched_by, "agent");
        assert_eq!(node.h, 120.0);
        assert!(node.measured);
        assert!(!atlas.describe().contains("LAST-EDITED-BY"));
    }

    #[test]
    fn an_unmeasured_node_is_estimated_and_says_so() {
        let mut scene = Scene::new();
        let id = place_node(
            &mut scene,
            &NodePatch {
                label: Some("bare".into()),
                ..NodePatch::default()
            },
            &Author::Agent,
        )
        .expect("place");
        let atlas = read(&scene).expect("read");
        let node = atlas.node(&id).expect("node");
        assert!(!node.measured);
        assert!(node.h > 0.0, "an unmeasured node still needs a height");
        assert!(atlas.describe().contains(" est)"));
        assert_eq!(layout(&atlas).unmeasured, 1);

        measure_node(&scene, &id, 91.0).expect("measure");
        let atlas = read(&scene).expect("read");
        assert!(atlas.node(&id).expect("node").measured);
        assert!(!atlas.describe().contains(" est)"));
        assert_eq!(layout(&atlas).unmeasured, 0);
    }

    #[test]
    fn a_tidy_page_reports_no_problems() {
        let mut scene = Scene::new();
        for index in 0..4 {
            boxed(
                &mut scene,
                &format!("row {index}"),
                0.0,
                index as f64 * 220.0,
                232.0,
                120.0,
            );
        }
        let atlas = read(&scene).expect("read");
        let report = layout(&atlas);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.extent, Some((0.0, 0.0, 232.0, 780.0)));
        assert!(atlas.describe().contains("No layout problems detected"));
    }

    #[test]
    fn a_stranded_node_is_called_out() {
        let mut scene = Scene::new();
        for index in 0..4 {
            boxed(
                &mut scene,
                &format!("pack {index}"),
                index as f64 * 260.0,
                0.0,
                232.0,
                120.0,
            );
        }
        assert!(layout(&read(&scene).expect("read")).problems.is_empty());

        let stray = boxed(&mut scene, "way out there", 9_000.0, 9_000.0, 232.0, 120.0);
        let report = layout(&read(&scene).expect("read"));
        let stranded = report
            .problems
            .iter()
            .find(|problem| problem.detail.contains("off on its own"))
            .expect("the outlier is reported");
        assert_eq!(stranded.ids, vec![stray]);
    }

    // ── shape-only overlaps: the bug a live user found ──────────────────────
    //
    // The document was 0 nodes and 5 drawn shapes. Two of the human's shapes
    // visibly overlapped two of the agent's, and `describe_layout` still
    // printed "No overlaps, no links crossing under a node, nothing
    // stranded." This is a claim about work `layout` never did, because the
    // overlap loop it fed from iterated `atlas.nodes` alone and returned
    // early on a node-less document. These pin the fix: real coordinates
    // from that report, reproduced with no nodes on the atlas at all.

    #[test]
    fn a_shape_only_document_with_real_overlaps_is_not_given_an_all_clear() {
        let mut scene = Scene::new();
        // The exact three rects from the false all-clear: "Hello there"
        // overlaps both "barrier" and "gets through"; "barrier" and "gets
        // through" do not touch each other.
        let hello = rect_at(&mut scene, "Hello there", 190.0, 245.0, 380.0, 159.0);
        let barrier = rect_at(&mut scene, "barrier", 380.0, 165.0, 78.0, 334.0);
        let through = rect_at(&mut scene, "gets through", 477.0, 370.0, 495.0, 120.0);

        let atlas = read(&scene).expect("read");
        assert!(
            atlas.nodes.is_empty(),
            "the bug only reproduces with 0 nodes"
        );
        let report = layout(&atlas);

        assert!(
            report
                .problems
                .iter()
                .any(|p| p.ids.contains(&hello) && p.ids.contains(&barrier)),
            "\"Hello there\" x \"barrier\" overlap must be reported: {:?}",
            report.problems
        );
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.ids.contains(&hello) && p.ids.contains(&through)),
            "\"Hello there\" x \"gets through\" overlap must be reported: {:?}",
            report.problems
        );
        assert!(
            !report
                .problems
                .iter()
                .any(|p| p.ids.contains(&barrier) && p.ids.contains(&through)),
            "barrier and gets through do not actually overlap"
        );

        let description = atlas.describe();
        assert!(
            !description.contains("No overlaps"),
            "a document with two real overlaps must not read clean: {description}"
        );
    }

    #[test]
    fn a_genuinely_clean_shape_only_document_still_reads_clean() {
        let mut scene = Scene::new();
        rect_at(&mut scene, "left", 0.0, 0.0, 200.0, 100.0);
        rect_at(&mut scene, "right", 400.0, 0.0, 200.0, 100.0);
        let atlas = read(&scene).expect("read");
        assert!(atlas.nodes.is_empty());

        let report = layout(&atlas);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert!(atlas.describe().contains("No layout problems detected"));
    }

    #[test]
    fn labels_arrows_and_ink_drawn_over_a_box_are_not_reported_as_overlaps() {
        // The judgment call: a text label sitting over the box it names, an
        // arrow crossing a shape it connects, and an ink stroke annotating a
        // card are the ordinary way the drawing layer is used, not a
        // collision. Flagging them would make PROBLEMS noise nobody reads.
        let mut scene = Scene::new();
        let box_id = rect_at(&mut scene, "container", 0.0, 0.0, 400.0, 300.0);

        // A caption sitting inside the box it names.
        text_at(&mut scene, "container", 20.0, 20.0);

        // An arrow that starts inside the box and points out of it. This is the
        // ordinary way a connector is drawn across a grouping region.
        place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                points: Some(format_points(&[(0.0, 0.0), (500.0, 0.0)])),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place arrow");

        // A single ink stroke drawn across the box, annotating it.
        stroke(
            &mut scene,
            &[(50.0, 50.0), (150.0, 150.0), (250.0, 50.0)],
            Author::Human,
        );

        let atlas = read(&scene).expect("read");
        let report = layout(&atlas);
        assert!(
            report.problems.is_empty(),
            "a label, an arrow, and a single ink stroke over a box are ordinary usage, not overlaps: {:?}",
            report.problems
        );
        assert!(atlas.describe().contains("No layout problems detected"));
        let _ = box_id;
    }

    #[test]
    fn node_to_node_overlap_detection_is_unchanged_by_the_shape_overlap_fix() {
        // Same shape as `a_note_that_makes_a_card_tall_is_reported_as_an_overlap`,
        // but with an unrelated, non-overlapping shape also on the atlas.
        // proving the new shape-overlap code does not touch, duplicate, or
        // suppress the pre-existing node-vs-node check.
        let mut scene = Scene::new();
        let top = boxed(&mut scene, "top", 0.0, 0.0, 232.0, 84.0);
        boxed(&mut scene, "bottom", 0.0, 168.0, 232.0, 84.0);
        rect_at(&mut scene, "elsewhere", 2000.0, 2000.0, 100.0, 100.0);
        assert!(
            layout(&read(&scene).expect("read")).problems.is_empty(),
            "168px apart and 84px tall is not an overlap"
        );

        measure_node(&scene, &top, 200.0).expect("measure");
        let report = layout(&read(&scene).expect("read"));
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        assert!(report.problems[0].detail.contains("OVERLAP"));
        assert!(report.problems[0].ids.contains(&top));
    }

    #[test]
    fn rejects_paths_that_escape_the_repository() {
        let mut scene = Scene::new();
        for bad in ["/etc/passwd", "../secrets", "~/keys", "a/../../b"] {
            let error = place_node(
                &mut scene,
                &NodePatch {
                    label: Some("x".into()),
                    path: Some(bad.into()),
                    ..Default::default()
                },
                &Author::Agent,
            )
            .expect_err("must reject");
            assert!(error.contains("path"), "{error}");
        }
    }

    #[test]
    fn rejects_unknown_tone_and_glyph() {
        let mut scene = Scene::new();
        assert!(place_node(
            &mut scene,
            &NodePatch {
                label: Some("x".into()),
                tone: Some("rainbow".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .is_err());
        let id = node(&mut scene, "x", Author::Agent);
        assert!(mark(&mut scene, &id, "%", "hm", &Author::Human).is_err());
    }

    #[test]
    fn a_human_mark_on_an_agent_drawn_shape_is_unanswered_in_the_read_back() {
        let mut scene = Scene::new();
        let shape = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                w: Some(240.0),
                h: Some(120.0),
                label: Some("stale trust boundary".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("agent draws shape");
        let before_mark = read(&scene).expect("read before mark").digest();

        mark(
            &mut scene,
            &shape,
            "?",
            "why is this still trusted?",
            &Author::Human,
        )
        .expect("human marks agent-drawn shape");

        let described = read(&scene).expect("read").describe();
        assert!(described.contains("MARKS (what the human flagged on the map)"));
        assert!(described.contains("stale trust boundary"), "{described}");
        assert!(described.contains("does not follow this"), "{described}");
        assert!(
            described.contains("why is this still trusted?"),
            "{described}"
        );
        assert!(described.contains("UNANSWERED"), "{described}");

        let after_mark = read(&scene).expect("read after mark").digest();
        let changed = describe_changes(&before_mark, &after_mark);
        assert!(changed.contains("NEW MARK"), "{changed}");
        assert!(changed.contains("stale trust boundary"), "{changed}");
        assert!(!changed.contains("<gone"), "{changed}");
    }

    #[test]
    fn an_agent_cannot_create_an_atlas_mark() {
        let mut scene = Scene::new();
        let target = node(&mut scene, "human authority", Author::Agent);
        let error = mark(
            &mut scene,
            &target,
            "!",
            "agent-authored objection",
            &Author::Agent,
        )
        .expect_err("agent mark creation must be refused");
        assert_eq!(error, "only the human can create atlas marks");
        assert!(read(&scene).expect("read").marks.is_empty());
    }

    #[test]
    fn removing_a_node_takes_its_edges_and_marks() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        link(&mut scene, &a, &b, "uses", &Author::Agent).expect("link");
        mark(&mut scene, &a, "?", "why", &Author::Human).expect("mark");

        let removed = remove(&scene, &a).expect("remove");
        assert_eq!(removed, 3);
        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.nodes.len(), 1);
        assert!(atlas.edges.is_empty());
        assert!(atlas.marks.is_empty());
    }

    #[test]
    fn removing_a_shape_takes_its_attached_marks() {
        let mut scene = Scene::new();
        let shape = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("ellipse".to_string()),
                x: Some(20.0),
                y: Some(30.0),
                w: Some(180.0),
                h: Some(90.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("agent draws shape");
        mark(&mut scene, &shape, "!", "remove this", &Author::Human).expect("human marks");

        assert_eq!(remove(&scene, &shape).expect("remove shape"), 2);
        let atlas = read(&scene).expect("read");
        assert!(atlas.shapes.is_empty());
        assert!(atlas.marks.is_empty());
    }

    /// The property this whole example exists to demonstrate: the human drags
    /// a node at the same moment the agent annotates it, neither replica has
    /// seen the other, and after the exchange BOTH edits are on the page.
    #[test]
    fn concurrent_human_drag_and_agent_note_both_survive() {
        let mut host = Scene::new();
        let id = place_node(
            &mut host,
            &NodePatch {
                label: Some("ag-ui-surface".into()),
                x: Some(100.0),
                y: Some(100.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("place");

        // Browser replica joins and catches up.
        let browser = Scene::from_state(&host.encode_full().expect("encode")).expect("replica");

        // Concurrent, offline-from-each-other edits.
        place_node(
            &mut host,
            &NodePatch {
                id: Some(id.clone()),
                note: Some("the runtime shell".into()),
                status: Some("agreed".into()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("agent annotates");
        let mut browser = browser;
        place_node(
            &mut browser,
            &NodePatch {
                id: Some(id.clone()),
                x: Some(640.0),
                y: Some(320.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("human drags");

        // Exchange diffs both ways.
        let host_diff = host
            .encode_diff_v1(&browser.state_vector_v1().expect("sv"))
            .expect("host diff");
        let browser_diff = browser
            .encode_diff_v1(&host.state_vector_v1().expect("sv"))
            .expect("browser diff");
        browser.apply_update(&host_diff).expect("apply host");
        host.apply_update(&browser_diff).expect("apply browser");

        for (who, scene) in [("host", &host), ("browser", &browser)] {
            let node = read(scene).expect("read").node(&id).cloned().expect("node");
            assert_eq!(node.x, 640.0, "{who} lost the human's drag");
            assert_eq!(node.y, 320.0, "{who} lost the human's drag");
            assert_eq!(
                node.note, "the runtime shell",
                "{who} lost the agent's note"
            );
            assert_eq!(node.status, "agreed", "{who} lost the agent's status");
            // `touched_by` is the one field BOTH writers set, so it is the one
            // field that genuinely conflicts: per-key LWW picks a deterministic
            // winner (both replicas agree, and it is not "whoever was later in
            // wall-clock time"). That is precisely why the interesting fields
            // are separate keys — a single blob of node JSON would have made
            // the drag and the note fight, and one of them would be gone.
            assert!(
                node.touched_by == "human" || node.touched_by == "agent",
                "{who} has an impossible last writer {}",
                node.touched_by
            );
        }
    }

    // ── the drawn layer, read honestly ──────────────────────────────────

    /// A box drawn so it swallows one card and clips another.
    ///
    /// The centre test called both of them "inside" — one number for two very
    /// different drawings — which is exactly the failure a human notices from
    /// three feet away and the agent could not see at all.
    #[test]
    fn a_region_that_half_covers_a_card_says_so_instead_of_guessing() {
        let mut scene = Scene::new();
        // node_at makes a 200x100 card at (x, y).
        let inside = node_at(&mut scene, "fully in", 120.0, 120.0);
        let clipped = node_at(&mut scene, "half in", 420.0, 120.0);

        // Left edge at 100 comfortably contains the first card; the right edge
        // at 520 cuts the second one down the middle.
        let region = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                w: Some(420.0),
                h: Some(140.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place region");

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == region)
            .expect("a reading for the region");

        assert_eq!(reading.relation, "encloses");
        assert_eq!(reading.targets, vec![inside.clone()]);
        assert_eq!(
            reading.partial,
            vec![clipped.clone()],
            "a card the region only half covers must be reported, not silently included or dropped"
        );

        // And it reaches the agent in both places it would look.
        let described = atlas.describe();
        assert!(
            described.contains("only partly covers \"half in\""),
            "the read-back hid the ambiguity:\n{described}"
        );
        let problems = layout(&atlas).problems;
        assert!(
            problems
                .iter()
                .any(|problem| problem.ids.contains(&region) && problem.ids.contains(&clipped)),
            "PROBLEMS said nothing about the half-covered card: {problems:?}"
        );
    }

    #[test]
    fn a_stroke_lying_across_a_card_it_means_nothing_about_is_a_problem() {
        let mut scene = Scene::new();
        let node = node_at(&mut scene, "buried", 200.0, 200.0);
        // A long open line that crosses the card and keeps going, so it is far
        // too big to read as striking it out and never closes into a loop.
        let scrawl = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("ink".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                points: Some(format_points(&[
                    (120.0, 210.0),
                    (900.0, 250.0),
                    (1400.0, 230.0),
                ])),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place stroke");

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == scrawl)
            .expect("a reading");
        assert_eq!(
            reading.relation, "free",
            "this stroke really does say nothing about the card"
        );

        let problems = layout(&atlas).problems;
        assert!(
            problems
                .iter()
                .any(|problem| problem.ids.contains(&scrawl) && problem.ids.contains(&node)),
            "a stroke drawn over a card without meaning anything about it went unreported: {problems:?}"
        );
    }

    #[test]
    fn two_strokes_on_the_same_spot_are_reported() {
        let mut scene = Scene::new();
        let first = stroke(
            &mut scene,
            &ring((400.0, 400.0), 60.0, 60.0, 16),
            Author::Human,
        );
        let second = stroke(
            &mut scene,
            &ring((410.0, 405.0), 58.0, 58.0, 16),
            Author::Agent,
        );

        let atlas = read(&scene).expect("read");
        let problems = layout(&atlas).problems;
        assert!(
            problems
                .iter()
                .any(|problem| problem.ids.contains(&first) && problem.ids.contains(&second)),
            "two strokes stacked on the same spot went unreported: {problems:?}"
        );
    }

    #[test]
    fn a_tidy_drawing_still_reports_no_problems() {
        let mut scene = Scene::new();
        node_at(&mut scene, "a", 100.0, 100.0);
        node_at(&mut scene, "b", 100.0, 300.0);
        // A loop that cleanly contains both cards, far from anything else.
        place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(60.0),
                y: Some(60.0),
                w: Some(300.0),
                h: Some(400.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place region");

        let problems = layout(&read(&scene).expect("read")).problems;
        assert!(
            problems.is_empty(),
            "a clean grouping should not be nagged about: {problems:?}"
        );
    }

    // ── text ────────────────────────────────────────────────────────────

    #[test]
    fn a_text_reaches_the_agent_as_the_words_it_says() {
        let mut scene = Scene::new();
        node_at(&mut scene, "the runtime", 100.0, 100.0);
        text_at(&mut scene, "start here", 600.0, 80.0);

        let atlas = read(&scene).expect("read");
        let described = atlas.describe();
        assert!(
            described.contains("SAYS \"start here\""),
            "a text has to reach the agent as its words:\n{described}"
        );
        assert!(
            described.contains("touching no node"),
            "far-away text must not imply a relationship:\n{described}"
        );
        // A text groups nothing and points at nothing; it is not a region.
        let reading = &readings(&atlas)[0];
        assert_eq!(reading.relation, "says");
        assert!(reading.targets.is_empty());
    }

    #[test]
    fn text_near_exactly_one_node_names_that_node() {
        let mut scene = Scene::new();
        let nearby = node_at(&mut scene, "runtime kernel", 100.0, 100.0);
        text_at(&mut scene, "start here", 310.0, 120.0);

        let atlas = read(&scene).expect("read");
        let reading = &readings(&atlas)[0];
        assert_eq!(reading.relation, "says");
        assert_eq!(reading.targets, vec![nearby]);
        assert!(
            atlas
                .describe()
                .contains("SAYS \"start here\" NEAR \"runtime kernel\""),
            "single-node proximity must be unambiguous:\n{}",
            atlas.describe()
        );
    }

    #[test]
    fn text_near_multiple_nodes_reports_the_ambiguity() {
        let mut scene = Scene::new();
        let first = node_at(&mut scene, "runtime kernel", 100.0, 100.0);
        let second = node_at(&mut scene, "browser replica", 100.0, 300.0);
        text_at(&mut scene, "check this", 310.0, 210.0);

        let atlas = read(&scene).expect("read");
        let reading = &readings(&atlas)[0];
        assert_eq!(reading.relation, "says");
        assert_eq!(reading.targets, vec![first, second]);
        assert!(
            atlas.describe().contains(
                "SAYS \"check this\" near 2 nodes: \"runtime kernel\", \"browser replica\""
            ),
            "multi-node proximity must stay explicitly ambiguous:\n{}",
            atlas.describe()
        );
    }

    #[test]
    fn a_text_is_as_tall_as_its_words_until_a_browser_measures_it() {
        let mut scene = Scene::new();
        let id = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("text".to_string()),
                label: Some("one line".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place text");

        let before = &read(&scene).expect("read").shapes[0];
        assert!(!before.measured, "nothing has laid this text out yet");
        assert!(before.h > 0.0, "an unmeasured text still needs a box");

        // Height is the one property nobody authors, exactly as for a card.
        let refused = place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(id.clone()),
                h: Some(400.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("authoring a text's height must be refused");
        assert!(refused.contains("as tall as its own words"), "{refused}");

        measure_shape(&scene, &id, 34.0).expect("measure");
        let after = &read(&scene).expect("read").shapes[0];
        assert!(after.measured);
        assert_eq!(after.h, 34.0);
        assert_eq!(
            after.touched_by, "human",
            "a measurement must not read as somebody editing the text"
        );
    }

    #[test]
    fn a_text_with_no_words_is_refused_rather_than_left_invisible() {
        let mut scene = Scene::new();
        let error = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("text".to_string()),
                x: Some(10.0),
                y: Some(10.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("an empty text must be rejected");
        assert!(error.contains("that is the whole shape"), "{error}");
    }

    // ── .excalidraw ─────────────────────────────────────────────────────

    #[test]
    fn every_form_survives_an_excalidraw_round_trip() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "left", 100.0, 100.0);
        let b = node_at(&mut scene, "right", 600.0, 100.0);
        link(&mut scene, &a, &b, "calls", &Author::Agent).expect("link");

        for patch in [
            ShapePatch {
                form: Some("rect".to_string()),
                x: Some(60.0),
                y: Some(60.0),
                w: Some(300.0),
                h: Some(220.0),
                ink: Some("blue".to_string()),
                fill: Some("blue".to_string()),
                label: Some("the group".to_string()),
                ..Default::default()
            },
            ShapePatch {
                form: Some("ellipse".to_string()),
                x: Some(700.0),
                y: Some(400.0),
                w: Some(120.0),
                h: Some(90.0),
                ink: Some("green".to_string()),
                ..Default::default()
            },
            ShapePatch {
                form: Some("diamond".to_string()),
                x: Some(900.0),
                y: Some(400.0),
                w: Some(120.0),
                h: Some(90.0),
                ink: Some("red".to_string()),
                ..Default::default()
            },
            ShapePatch {
                form: Some("ink".to_string()),
                x: Some(200.0),
                y: Some(600.0),
                points: Some(format_points(&[(0.0, 0.0), (40.0, 30.0), (90.0, 10.0)])),
                ink: Some("amber".to_string()),
                ..Default::default()
            },
            ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(300.0),
                y: Some(700.0),
                points: Some(format_points(&[(0.0, 0.0), (120.0, 40.0)])),
                ink: Some("violet".to_string()),
                ..Default::default()
            },
            ShapePatch {
                form: Some("line".to_string()),
                x: Some(500.0),
                y: Some(700.0),
                points: Some(format_points(&[(0.0, 0.0), (60.0, 60.0)])),
                ..Default::default()
            },
            ShapePatch {
                form: Some("text".to_string()),
                x: Some(820.0),
                y: Some(60.0),
                label: Some("a caption".to_string()),
                ink: Some("amber".to_string()),
                ..Default::default()
            },
        ] {
            place_shape(&mut scene, &patch, &Author::Human).expect("place shape");
        }

        let before = read(&scene).expect("read");
        let document = excalidraw::to_excalidraw(&before);
        let parsed: serde_json::Value = serde_json::from_str(&document).expect("valid JSON");
        assert_eq!(parsed["type"], "excalidraw");
        assert_eq!(parsed["version"], 2);

        // Land it on a fresh atlas, exactly the way an import does.
        let mut landed = Scene::new();
        let import = excalidraw::from_excalidraw(&document).expect("import");
        let landed_import = excalidraw::land_import(&mut landed, import, &Author::Human, 0.0, 0.0);
        assert!(
            landed_import.skipped.is_empty(),
            "{:?}",
            landed_import.skipped
        );
        let after = read(&landed).expect("read back");

        let forms = |atlas: &Atlas| {
            let mut forms: Vec<String> = atlas
                .shapes
                .iter()
                .map(|shape| shape.form.clone())
                .collect();
            forms.sort();
            forms
        };
        // Every drawn form comes back. The two cards arrive as the rectangles
        // they were exported as, plus their words as text — lossy on purpose,
        // and the extra forms below are that loss made visible rather than
        // hidden.
        for form in ["ink", "ellipse", "diamond", "line", "arrow", "text"] {
            assert!(
                forms(&after).contains(&form.to_string()),
                "{form} did not survive the round trip: {:?}",
                forms(&after)
            );
        }

        let ellipse = after
            .shapes
            .iter()
            .find(|shape| shape.form == "ellipse")
            .expect("the ellipse");
        assert_eq!(ellipse.ink, "green", "colour did not survive");
        assert_eq!((ellipse.x, ellipse.y), (700.0, 400.0), "position moved");
        assert_eq!((ellipse.w, ellipse.h), (120.0, 90.0), "size changed");

        let scribble = after
            .shapes
            .iter()
            .find(|shape| shape.form == "ink")
            .expect("the stroke");
        assert_eq!(scribble.ink, "amber");
        assert_eq!(
            scribble.points.first().copied(),
            Some((0.0, 0.0)),
            "points are relative to the origin on both sides"
        );

        assert!(
            after
                .shapes
                .iter()
                .any(|shape| shape.form == "text" && shape.label == "a caption"),
            "the text's words were lost"
        );
        // A caption is a bound text element out there and a `label` in here.
        assert!(
            after
                .shapes
                .iter()
                .any(|shape| shape.form == "rect" && shape.label == "the group"),
            "a box's caption came back as a loose text instead of its label"
        );
    }

    #[test]
    fn an_import_names_what_it_could_not_bring() {
        let document = r##"{
            "type": "excalidraw", "version": 2, "elements": [
                { "type": "rectangle", "id": "keep", "x": 10, "y": 20, "width": 100, "height": 50, "strokeColor": "#e03131" },
                { "type": "image", "id": "drop", "x": 0, "y": 0, "width": 10, "height": 10 },
                { "type": "embeddable", "id": "drop2", "x": 0, "y": 0, "width": 10, "height": 10 }
            ]
        }"##;
        let import = excalidraw::from_excalidraw(document).expect("import");
        assert_eq!(import.shapes.len(), 1);
        assert_eq!(import.shapes[0].form.as_deref(), Some("rect"));
        // A colour from outside our closed list snaps to the nearest one we
        // can actually render, rather than being dropped or invented.
        assert_eq!(import.shapes[0].ink.as_deref(), Some("red"));
        assert_eq!(import.skipped.len(), 2, "{:?}", import.skipped);
        assert!(
            import.skipped.iter().all(|line| line.contains("invisible")),
            "a skipped element has to say why: {:?}",
            import.skipped
        );
    }

    #[test]
    fn an_import_reconnects_bound_curved_arrows_to_imported_shapes() {
        let document = r##"{
            "type": "excalidraw", "version": 2, "elements": [
                { "type": "rectangle", "id": "source", "x": 20, "y": 40,
                  "width": 160, "height": 90, "strokeColor": "#1971c2" },
                { "type": "diamond", "id": "target", "x": 520, "y": 180,
                  "width": 160, "height": 110, "strokeColor": "#2f9e44" },
                { "type": "arrow", "id": "flow", "x": 180, "y": 85,
                  "width": 340, "height": 150,
                  "points": [[0, 0], [130, -60], [250, 170], [340, 150]],
                  "strokeColor": "#343a40", "endArrowhead": "arrow",
                  "startBinding": { "elementId": "source" },
                  "endBinding": { "elementId": "target" } }
            ]
        }"##;

        let import = excalidraw::from_excalidraw(document).expect("parse import");
        let mut scene = Scene::new();
        let landed = excalidraw::land_import(&mut scene, import, &Author::Human, 0.0, 0.0);
        assert_eq!(landed.bound_connectors, 1, "{:?}", landed.notes);
        assert!(landed.notes.is_empty(), "{:?}", landed.notes);

        let atlas = read(&scene).expect("read");
        let connector = atlas
            .shapes
            .iter()
            .find(|shape| shape.form == "arrow")
            .expect("arrow");
        assert!(!connector.from.is_empty());
        assert!(!connector.to.is_empty());
        assert_eq!(
            atlas.shape_path(connector).expect("painted path").len(),
            4,
            "the curved route was flattened"
        );

        let target = atlas.shape(&connector.to).expect("bound target");
        let target_id = target.id.clone();
        let before = atlas.shape_endpoints(connector).expect("endpoints").1;
        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(target_id),
                x: Some(target.x + 300.0),
                y: Some(target.y + 120.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("move target");
        let moved = read(&scene).expect("read moved");
        let connector = moved.shape(&connector.id).expect("same connector");
        let after = moved.shape_endpoints(connector).expect("endpoints").1;
        assert_ne!(before, after, "the imported arrow did not follow its shape");

        let exported: serde_json::Value =
            serde_json::from_str(&excalidraw::to_excalidraw(&moved)).expect("export");
        let arrow = exported["elements"]
            .as_array()
            .and_then(|elements| elements.iter().find(|element| element["type"] == "arrow"))
            .expect("exported arrow");
        assert_eq!(arrow["points"].as_array().map(Vec::len), Some(4));
        assert!(arrow["startBinding"]["elementId"].is_string());
        assert!(arrow["endBinding"]["elementId"].is_string());
    }

    #[test]
    fn a_file_that_is_not_a_drawing_fails_loudly() {
        assert!(excalidraw::from_excalidraw("not json").is_err());
        assert!(excalidraw::from_excalidraw(r#"{"hello": 1}"#).is_err());
        // Well-formed, but nothing in it can land.
        let error = excalidraw::from_excalidraw(
            r#"{"type":"excalidraw","elements":[{"type":"image","id":"x"}]}"#,
        )
        .expect_err("an import that lands nothing is a failure, not a success");
        assert!(error.contains("skipped"), "{error}");
    }

    /// An arrow with one end on a card and the other dropped in open space.
    ///
    /// It used to read as touching no node — which then had the drawing
    /// PROBLEMS report it as covering up the very card it is attached to.
    #[test]
    fn an_arrow_with_one_end_loose_still_names_the_card_it_is_on() {
        let mut scene = Scene::new();
        let node = node_at(&mut scene, "dispatch_tool", 200.0, 200.0);
        let arrow = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(300.0),
                y: Some(250.0),
                points: Some(format_points(&[(0.0, 0.0), (-100.0, 300.0)])),
                from: Some(node.clone()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place arrow");

        let atlas = read(&scene).expect("read");
        let reading = readings(&atlas)
            .into_iter()
            .find(|reading| reading.shape == arrow)
            .expect("a reading");
        assert_eq!(reading.relation, "starts-at");
        assert_eq!(reading.targets, vec![node.clone()]);

        let described = atlas.describe();
        assert!(
            described.contains("STARTS-AT \"dispatch_tool\""),
            "{described}"
        );
        // And it is not accused of covering up the card it is attached to.
        for problem in layout(&atlas).problems {
            assert!(
                !(problem.ids.contains(&arrow) && problem.ids.contains(&node)),
                "an arrow bound to a card must not be reported as meaningless over it: {problem:?}"
            );
        }
    }

    // ── what changed, said as relations ─────────────────────────────────

    /// The headline case: an arrow that now points somewhere else.
    ///
    /// `(93,834)` is true and useless. "It now connects A -> C, it was
    /// connecting A -> B" is the same edit described as the thing that
    /// actually changed.
    #[test]
    fn a_rebound_arrow_is_reported_as_the_relation_that_changed() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "dispatch_tool", 100.0, 100.0);
        let b = node_at(&mut scene, "the asymmetry", 600.0, 100.0);
        let c = node_at(&mut scene, "openai adapter", 600.0, 400.0);
        let arrow = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                from: Some(a.clone()),
                to: Some(b.clone()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place arrow");

        let before = read(&scene).expect("read").digest();

        // The human drags the head onto a different card.
        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(arrow.clone()),
                to: Some(c.clone()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("re-bind");

        let after = read(&scene).expect("read").digest();
        let lines = changes(&before, &after);
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(
            line.contains("now CONNECTS \"dispatch_tool\" -> \"openai adapter\""),
            "{line}"
        );
        assert!(
            line.contains("it was CONNECTS \"dispatch_tool\" -> \"the asymmetry\""),
            "the before half is the whole point: {line}"
        );
        assert!(
            line.contains("relating them"),
            "the intent, not just the geometry: {line}"
        );
        assert!(
            !line.contains('('),
            "a relation change must not be reported as coordinates: {line}"
        );
    }

    #[test]
    fn a_shape_that_relates_to_nothing_is_the_one_case_coordinates_survive() {
        let mut scene = Scene::new();
        let id = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("diamond".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                w: Some(80.0),
                h: Some(80.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place");
        let before = read(&scene).expect("read").digest();

        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(id),
                x: Some(900.0),
                y: Some(700.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("move");

        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("moved to (900,700)") && lines[0].contains("still touches no node"),
            "with no relation to describe, the position is the only thing to say: {}",
            lines[0]
        );
    }

    /// Moving a shape without changing what it says is not news.
    #[test]
    fn a_move_that_changes_no_meaning_is_not_reported() {
        let mut scene = Scene::new();
        node_at(&mut scene, "a", 200.0, 200.0);
        let id = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(150.0),
                y: Some(150.0),
                w: Some(320.0),
                h: Some(220.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("place");
        let before = read(&scene).expect("read").digest();
        assert_eq!(before.shapes[0].relation, "encloses");

        // Nudged, but still around the same card.
        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(id),
                x: Some(140.0),
                y: Some(145.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("nudge");

        assert!(
            changes(&before, &read(&scene).expect("read").digest()).is_empty(),
            "a shape that still says the same thing has not changed"
        );
    }

    #[test]
    fn a_long_list_of_changes_says_how_many_it_left_out() {
        let mut scene = Scene::new();
        let before = read(&scene).expect("read").digest();
        for index in 0..40 {
            place_node(
                &mut scene,
                &NodePatch {
                    label: Some(format!("node {index}")),
                    ..Default::default()
                },
                &Author::Human,
            )
            .expect("place");
        }
        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(lines.len() <= MAX_CHANGES + 1);
        assert!(
            lines
                .last()
                .expect("a last line")
                .contains("more change(s) not listed"),
            "a truncated list that reads as complete is worse than a long one: {:?}",
            lines.last()
        );
    }

    fn constraint(
        scene: &mut Scene,
        op: &str,
        members: &[&str],
        axis: Option<&str>,
        text: Option<&str>,
    ) -> String {
        let members = members
            .iter()
            .map(|member| (*member).to_string())
            .collect::<Vec<_>>();
        create_constraint(scene, op, &members, axis, text, None, &Author::Agent)
            .expect("create constraint")
    }

    #[test]
    fn every_constraint_op_creates_reads_and_describes() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "plan", Author::Agent);
        let b = node(&mut scene, "build", Author::Agent);
        let ids = [
            constraint(&mut scene, "group", &[&a, &b], None, None),
            constraint(&mut scene, "sequence", &[&a, &b], Some("y"), None),
            constraint(&mut scene, "attaches", &[&a, &b], None, None),
            constraint(&mut scene, "voids", &[&a], None, None),
            constraint(&mut scene, "labels", &[&b], None, Some("implementation")),
        ];

        let atlas = read(&scene).expect("read");
        assert_eq!(atlas.constraints.len(), 5);
        for op in ["group", "sequence", "attaches", "voids", "labels"] {
            assert!(
                atlas
                    .constraints
                    .iter()
                    .any(|constraint| constraint.op == op),
                "missing {op} projection"
            );
        }
        let sequence = atlas
            .constraints
            .iter()
            .find(|constraint| constraint.op == "sequence")
            .expect("sequence");
        assert_eq!(sequence.axis.as_deref(), Some("y"));
        assert_eq!(sequence.status, "sat");
        let labels = atlas
            .constraints
            .iter()
            .find(|constraint| constraint.op == "labels")
            .expect("labels");
        assert_eq!(labels.text.as_deref(), Some("implementation"));
        assert_eq!(labels.axis, None);
        assert_eq!(atlas.constraint_state.status, "sat");
        assert!(atlas.constraint_state.core.is_empty());
        assert!(atlas
            .constraints
            .windows(2)
            .all(|pair| pair[0].id <= pair[1].id));

        let described = atlas.describe();
        assert!(described.contains("\nCONSTRAINTS\n"), "{described}");
        assert!(
            described.contains(&format!("sequence[y]: {a} -> {b} (SAT)")),
            "{described}"
        );
        for id in ids {
            assert!(described.contains(&id), "missing {id} in {described}");
        }
    }

    #[test]
    fn constraint_authoring_refusals_store_nothing() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let c = node(&mut scene, "c", Author::Agent);

        let refuse = |scene: &mut Scene,
                      op: &str,
                      members: Vec<String>,
                      axis: Option<&str>,
                      text: Option<&str>| {
            let before = read(scene).expect("before").constraints.len();
            assert!(
                create_constraint(scene, op, &members, axis, text, None, &Author::Agent,).is_err(),
                "{op} with {members:?} should be refused"
            );
            assert_eq!(
                read(scene).expect("after").constraints.len(),
                before,
                "a refused write stored a constraint"
            );
        };

        refuse(&mut scene, "align", vec![a.clone(), b.clone()], None, None);
        for (op, members) in [
            ("group", vec![a.clone()]),
            ("sequence", vec![a.clone()]),
            ("attaches", vec![a.clone()]),
            ("attaches", vec![a.clone(), b.clone(), c.clone()]),
            ("voids", vec![a.clone(), b.clone()]),
            ("labels", vec![a.clone(), b.clone()]),
        ] {
            refuse(&mut scene, op, members, None, None);
        }
        refuse(&mut scene, "group", vec![a.clone(), a.clone()], None, None);
        refuse(
            &mut scene,
            "group",
            vec![a.clone(), "no-such-object".to_string()],
            None,
            None,
        );

        let existing = constraint(&mut scene, "group", &[&a, &b], None, None);
        refuse(&mut scene, "group", vec![a, existing], None, None);
    }

    #[test]
    fn two_constraint_cycle_is_the_minimal_unsat_core() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let forward = constraint(&mut scene, "sequence", &[&a, &b], Some("x"), None);
        let reverse = constraint(&mut scene, "sequence", &[&b, &a], Some("x"), None);

        let atlas = read(&scene).expect("read");
        let mut expected = vec![forward, reverse];
        expected.sort();
        assert_eq!(atlas.constraint_state.status, "unsat");
        assert_eq!(atlas.constraint_state.core, expected);
        assert!(atlas
            .constraints
            .iter()
            .all(|constraint| constraint.status == "sat"));
    }

    #[test]
    fn three_constraint_cycle_is_reported_without_mixing_axes() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let c = node(&mut scene, "c", Author::Agent);
        let first = constraint(&mut scene, "sequence", &[&a, &b], Some("x"), None);
        let second = constraint(&mut scene, "sequence", &[&b, &c], Some("x"), None);
        let third = constraint(&mut scene, "sequence", &[&c, &a], Some("x"), None);
        constraint(&mut scene, "sequence", &[&b, &a], Some("y"), None);

        let atlas = read(&scene).expect("read");
        let mut expected = vec![first, second, third];
        expected.sort();
        assert_eq!(atlas.constraint_state.status, "unsat");
        assert_eq!(atlas.constraint_state.core, expected);
    }

    #[test]
    fn deleting_a_member_leaves_a_broken_constraint() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let id = constraint(&mut scene, "sequence", &[&a, &b], None, None);

        remove(&scene, &b).expect("remove member");
        let atlas = read(&scene).expect("read");
        let broken = atlas
            .constraints
            .iter()
            .find(|constraint| constraint.id == id)
            .expect("constraint remains");
        assert_eq!(broken.status, "broken");
        assert_eq!(atlas.constraint_state.status, "sat");
        assert!(atlas.describe().contains("(BROKEN)"));
    }

    #[test]
    fn digest_changes_name_constraint_lifecycle_and_status_transitions() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let before = read(&scene).expect("read").digest();
        let id = constraint(&mut scene, "sequence", &[&a, &b], None, None);
        let created = read(&scene).expect("read").digest();
        assert!(changes(&before, &created)
            .iter()
            .any(|line| line.contains("NEW CONSTRAINT") && line.contains(&id)));

        remove(&scene, &b).expect("remove member");
        let broken = read(&scene).expect("read").digest();
        assert!(changes(&created, &broken)
            .iter()
            .any(|line| line.contains("BROKEN") && line.contains(&id)));

        remove(&scene, &id).expect("remove constraint");
        let removed = read(&scene).expect("read").digest();
        assert!(changes(&broken, &removed)
            .iter()
            .any(|line| line.contains("GONE CONSTRAINT") && line.contains(&id)));
    }

    #[test]
    fn snapshot_and_painting_pin_constraint_json_field_names() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let id = constraint(&mut scene, "sequence", &[&a, &b], None, None);
        let atlas = read(&scene).expect("read");

        let constraint_json = format!(
            r#""constraints":[{{"id":"{id}","op":"sequence","members":["{a}","{b}"],"axis":"x","text":null,"salience":"back","status":"sat","created_by":"agent"}}]"#
        );
        let snapshot = serde_json::to_string(&atlas).expect("snapshot JSON");
        assert!(snapshot.contains(&constraint_json), "{snapshot}");
        assert!(snapshot.contains(r#""constraint_state":{"status":"sat","core":[]}"#));

        let painting = serde_json::to_string(&atlas.painting()).expect("painting JSON");
        assert!(painting.contains(&constraint_json), "{painting}");
        assert!(painting.contains(r#""constraint_state":{"status":"sat","core":[]}"#));

        for document in [&snapshot, &painting] {
            let value: serde_json::Value = serde_json::from_str(document).expect("valid JSON");
            let constraint = &value["constraints"][0];
            assert!(
                constraint
                    .as_object()
                    .expect("constraint object")
                    .values()
                    .all(|value| !value.is_number()),
                "constraint fields must not contain numerics: {constraint}"
            );
        }
    }

    #[test]
    fn excalidraw_export_names_constraints_as_skipped() {
        let mut scene = Scene::new();
        let a = node(&mut scene, "a", Author::Agent);
        let b = node(&mut scene, "b", Author::Agent);
        let id = constraint(&mut scene, "group", &[&a, &b], None, None);
        let exported: serde_json::Value =
            serde_json::from_str(&excalidraw::to_excalidraw(&read(&scene).expect("read")))
                .expect("export JSON");
        assert!(exported["skipped"]
            .as_array()
            .expect("skipped report")
            .iter()
            .any(|note| note.as_str().is_some_and(|note| note.contains(&id))));
        assert!(exported["notes"].is_array());
    }

    #[test]
    fn capacity_counts_constraints_as_objects() {
        let constraint = Constraint {
            id: "c".to_string(),
            op: "group".to_string(),
            members: vec!["a".to_string(), "b".to_string()],
            axis: None,
            text: None,
            salience: "back".to_string(),
            status: "sat".to_string(),
            created_by: "agent".to_string(),
        };
        let atlas = Atlas {
            constraints: vec![constraint; MAX_OBJECTS],
            ..Atlas::default()
        };
        assert!(capacity(&atlas).is_err());
    }

    // ── the node register reads back as arrangement, not pixels ────────────

    /// Helper: move a node and return the change lines it produced.
    fn move_node(scene: &mut Scene, id: &str, x: f64, y: f64, author: &Author) {
        place_node(
            scene,
            &NodePatch {
                id: Some(id.to_string()),
                x: Some(x),
                y: Some(y),
                ..Default::default()
            },
            author,
        )
        .expect("move node");
    }

    /// The defect this whole change exists for: a human rearranging cards was
    /// told to the agent as a pixel count, which is the one fact on the line
    /// nobody can act on.
    #[test]
    fn a_rearranged_card_reads_back_as_who_it_now_sits_with() {
        let mut scene = Scene::new();
        let anchor = node_at(&mut scene, "anchor", 0.0, 0.0);
        let other = node_at(&mut scene, "far side", 900.0, 0.0);
        let travelling = node_at(&mut scene, "travelling", 0.0, 900.0);
        let _ = (&anchor, &other);
        let before = read(&scene).expect("read").digest();

        // Under "anchor": same column, a normal row gap away.
        move_node(&mut scene, &travelling, 0.0, 200.0, &Author::Human);

        let lines = changes(&before, &read(&scene).expect("read").digest());
        let line = lines
            .iter()
            .find(|line| line.contains("travelling"))
            .unwrap_or_else(|| panic!("no line about the moved card: {lines:?}"));
        assert!(
            line.contains("under \"anchor\""),
            "a move must be said in terms of the cards it now sits with: {line}"
        );
        assert!(
            !line.contains("px"),
            "a pixel count is the one thing on the line nobody can act on: {line}"
        );
    }

    #[test]
    fn a_human_made_overlap_is_the_new_relation_in_the_change_headline() {
        let mut scene = Scene::new();
        let moved = node_at(&mut scene, "human claim", 0.0, 0.0);
        node_at(&mut scene, "existing claim", 600.0, 0.0);
        let before = read(&scene).expect("read").digest();

        move_node(&mut scene, &moved, 600.0, 0.0, &Author::Human);

        let after = read(&scene).expect("read").digest();
        let section = describe_changes(&before, &after);
        let headline = section
            .lines()
            .find(|line| line.contains("human claim"))
            .unwrap_or_else(|| panic!("no change headline for the moved card: {section}"));
        assert!(
            headline.contains("now OVERLAPS \"existing claim\""),
            "the human's overlap must be the new relation: {headline}"
        );
        assert!(
            !headline.contains("PROBLEMS"),
            "the overlap is a human spatial claim, not a layout-defect headline: {headline}"
        );
    }

    /// The same judgement the drawn register already makes about shapes: a
    /// move that leaves the arrangement alone is not news.
    #[test]
    fn a_card_move_that_changes_no_arrangement_is_not_reported() {
        let mut scene = Scene::new();
        node_at(&mut scene, "anchor", 0.0, 0.0);
        let travelling = node_at(&mut scene, "travelling", 0.0, 200.0);
        let before = read(&scene).expect("read").digest();

        // Still directly under "anchor", just nudged well past MOVED_ENOUGH.
        move_node(&mut scene, &travelling, 0.0, 260.0, &Author::Human);

        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(
            !lines.iter().any(|line| line.contains("travelling")),
            "same neighbours means the arrangement did not change: {lines:?}"
        );
    }

    /// A card with nothing near it is the one case coordinates are the best
    /// answer available — the rule the shape register already follows.
    #[test]
    fn a_card_moved_off_on_its_own_falls_back_to_coordinates() {
        let mut scene = Scene::new();
        node_at(&mut scene, "anchor", 0.0, 0.0);
        let travelling = node_at(&mut scene, "travelling", 4000.0, 4000.0);
        let before = read(&scene).expect("read").digest();

        move_node(&mut scene, &travelling, 9000.0, 9000.0, &Author::Human);

        let lines = changes(&before, &read(&scene).expect("read").digest());
        let line = lines
            .iter()
            .find(|line| line.contains("travelling"))
            .unwrap_or_else(|| panic!("no line: {lines:?}"));
        assert!(
            line.contains("(9000,9000)") && line.contains("apart from everything"),
            "nothing to say it in terms of, so say where it is: {line}"
        );
    }

    /// The relationship register was absent from the digest entirely, so a
    /// human linking two claims was invisible to the agent.
    #[test]
    fn a_link_the_human_draws_is_reported() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "cause", 0.0, 0.0);
        let b = node_at(&mut scene, "effect", 600.0, 0.0);
        let before = read(&scene).expect("read").digest();

        link(&mut scene, &a, &b, "leads to", &Author::Human).expect("link");

        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(
            lines
                .iter()
                .any(|line| line.contains("NEW LINK") && line.contains("leads to")),
            "a relationship between two claims is a claim: {lines:?}"
        );
    }

    /// Removing a link is the human withdrawing a claim, and said so.
    #[test]
    fn a_withdrawn_link_is_reported() {
        let mut scene = Scene::new();
        let a = node_at(&mut scene, "cause", 0.0, 0.0);
        let b = node_at(&mut scene, "effect", 600.0, 0.0);
        let edge = link(&mut scene, &a, &b, "leads to", &Author::Human).expect("link");
        let before = read(&scene).expect("read").digest();

        remove(&scene, &edge).expect("remove link");

        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(
            lines.iter().any(|line| line.contains("GONE LINK")),
            "withdrawing a relationship is news: {lines:?}"
        );
    }

    /// Re-toning a card is the human changing what kind of thing it is.
    #[test]
    fn a_retoned_card_is_reported() {
        let mut scene = Scene::new();
        let id = node_at(&mut scene, "claim", 0.0, 0.0);
        let before = read(&scene).expect("read").digest();

        place_node(
            &mut scene,
            &NodePatch {
                id: Some(id),
                tone: Some("risk".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("retone");

        let lines = changes(&before, &read(&scene).expect("read").digest());
        assert!(
            lines.iter().any(|line| line.contains("is now a risk")),
            "tone was in the digest and never compared: {lines:?}"
        );
    }

    /// An agent that writes and then reads was handed its own work back as
    /// the human's. Its own writes are discounted; the human's are not.
    #[test]
    fn a_reader_is_not_told_about_its_own_writes() {
        let mut scene = Scene::new();
        let before = read(&scene).expect("read").digest();

        place_node(
            &mut scene,
            &NodePatch {
                label: Some("mine".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(200.0),
                ..Default::default()
            },
            &Author::Named("claude".to_string()),
        )
        .expect("agent writes");
        place_node(
            &mut scene,
            &NodePatch {
                label: Some("theirs".to_string()),
                x: Some(600.0),
                y: Some(0.0),
                w: Some(200.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("human writes");

        let after = read(&scene).expect("read").digest();
        let lines = changes_for(&before, &after, "claude");
        assert!(
            !lines.iter().any(|line| line.contains("mine")),
            "the reader's own write is not news to the reader: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("theirs")),
            "discounting my own writes must not swallow the human's: {lines:?}"
        );
    }

    /// A card the agent made and the human then moved is still the human's
    /// news, because the last write to it was not the agent's.
    #[test]
    fn the_human_moving_the_readers_card_still_reports() {
        let mut scene = Scene::new();
        node_at(&mut scene, "anchor", 0.0, 0.0);
        let mine = place_node(
            &mut scene,
            &NodePatch {
                label: Some("mine".to_string()),
                x: Some(0.0),
                y: Some(900.0),
                w: Some(200.0),
                ..Default::default()
            },
            &Author::Named("claude".to_string()),
        )
        .expect("agent writes");
        measure_node(&scene, &mine, 100.0).expect("measure");
        let before = read(&scene).expect("read").digest();

        move_node(&mut scene, &mine, 0.0, 200.0, &Author::Human);

        let after = read(&scene).expect("read").digest();
        let lines = changes_for(&before, &after, "claude");
        assert!(
            lines.iter().any(|line| line.contains("mine")),
            "the last write was the human's, so it is theirs to report: {lines:?}"
        );
    }

    /// The header asserted "what the human did" over lists containing none of
    /// the human's work.
    #[test]
    fn the_header_does_not_credit_the_human_for_agent_writes() {
        let mut scene = Scene::new();
        let before = read(&scene).expect("read").digest();
        place_node(
            &mut scene,
            &NodePatch {
                label: Some("agent card".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(200.0),
                ..Default::default()
            },
            &Author::Named("claude".to_string()),
        )
        .expect("agent writes");

        let text = describe_changes(&before, &read(&scene).expect("read").digest());
        assert!(
            !text.contains("what the human did"),
            "no human touched any of this: {text}"
        );
        assert!(
            text.contains("claude"),
            "say whose writes these are: {text}"
        );
    }
}

/// The Excalidraw attributes and the frame element, added 2026-08-25 so a
/// drawing can say more than "box, arrow, words": how heavy, how sure, how
/// faded, which panel.
#[cfg(test)]
mod style_and_frame_tests {
    use super::*;
    use ag_ui_canvas::scene::{Author, Scene};

    fn rect(scene: &mut Scene, patch: ShapePatch) -> String {
        place_shape(
            scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                w: Some(200.0),
                h: Some(120.0),
                ..patch
            },
            &Author::Agent,
        )
        .expect("rect")
    }

    fn frame(scene: &mut Scene, name: &str) -> String {
        place_shape(
            scene,
            &ShapePatch {
                form: Some("frame".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(800.0),
                h: Some(600.0),
                label: Some(name.to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("frame")
    }

    #[test]
    fn a_shape_authored_before_these_fields_reads_as_the_defaults() {
        let mut scene = Scene::new();
        let id = rect(&mut scene, ShapePatch::default());
        let atlas = read(&scene).expect("read");
        let shape = atlas.shape(&id).expect("shape");
        assert_eq!(shape.stroke_width, DEFAULT_STROKE_WIDTH);
        assert_eq!(shape.stroke_style, "solid");
        assert_eq!(shape.opacity, 100.0);
        assert_eq!(
            shape.roundness, "round",
            "a rect drew rounded before this field existed"
        );
        assert_eq!(shape.font_size, 0.0);
        assert_eq!(shape.angle, 0.0);
        assert!(shape.groups.is_empty());
        assert!(shape.frame.is_empty());
        // ...and says nothing about any of it.
        assert_eq!(shape.style_note(&atlas), "");
    }

    #[test]
    fn every_attribute_is_stored_clamped_and_read_back() {
        let mut scene = Scene::new();
        let holder = frame(&mut scene, "Before");
        let id = rect(
            &mut scene,
            ShapePatch {
                stroke_width: Some(40.0), // clamped to 12
                stroke_style: Some("dashed".to_string()),
                opacity: Some(-5.0), // clamped to 0
                roundness: Some("sharp".to_string()),
                font_size: Some(28.0),
                angle: Some(std::f64::consts::FRAC_PI_4),
                groups: Some("g1 g2".to_string()),
                frame: Some(holder.clone()),
                label: Some("draft".to_string()),
                ..Default::default()
            },
        );
        let atlas = read(&scene).expect("read");
        let shape = atlas.shape(&id).expect("shape");
        assert_eq!(shape.stroke_width, STROKE_WIDTH_RANGE.1);
        assert_eq!(shape.stroke_style, "dashed");
        assert_eq!(shape.opacity, 0.0);
        assert_eq!(shape.roundness, "sharp");
        assert_eq!(shape.font_size, 28.0);
        assert!((shape.angle - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
        assert_eq!(shape.groups, vec!["g1".to_string(), "g2".to_string()]);
        assert_eq!(shape.frame, holder);

        let note = shape.style_note(&atlas);
        for expected in [
            "dashed",
            "12px stroke",
            "0% opacity",
            "rotated 45°",
            "28px type",
            "group g2",
            "in frame \"Before\"",
        ] {
            assert!(
                note.contains(expected),
                "{expected:?} missing from {note:?}"
            );
        }
        let described = atlas.describe();
        assert!(
            described.contains("holding 1 drawn shape(s)"),
            "{described}"
        );
    }

    #[test]
    fn the_closed_lists_refuse_what_is_not_on_them() {
        let mut scene = Scene::new();
        for (patch, expect) in [
            (
                ShapePatch {
                    stroke_style: Some("wavy".to_string()),
                    ..Default::default()
                },
                "stroke_style",
            ),
            (
                ShapePatch {
                    roundness: Some("bevelled".to_string()),
                    ..Default::default()
                },
                "roundness",
            ),
            (
                ShapePatch {
                    frame: Some("nope".to_string()),
                    ..Default::default()
                },
                "no frame",
            ),
            (
                ShapePatch {
                    opacity: Some(f64::NAN),
                    ..Default::default()
                },
                "opacity",
            ),
        ] {
            let error = place_shape(
                &mut scene,
                &ShapePatch {
                    form: Some("rect".to_string()),
                    w: Some(50.0),
                    h: Some(50.0),
                    ..patch
                },
                &Author::Agent,
            )
            .expect_err(expect);
            assert!(error.contains(expect), "{error}");
        }
    }

    #[test]
    fn frames_do_not_nest_and_only_a_frame_holds() {
        let mut scene = Scene::new();
        let outer = frame(&mut scene, "outer");
        let plain = rect(&mut scene, ShapePatch::default());
        let nested = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("frame".to_string()),
                w: Some(100.0),
                h: Some(100.0),
                frame: Some(outer.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("nested frame");
        assert!(
            nested.contains("cannot sit inside another frame"),
            "{nested}"
        );
        let in_rect = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("ellipse".to_string()),
                w: Some(40.0),
                h: Some(40.0),
                frame: Some(plain.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("rect as frame");
        assert!(in_rect.contains("not a frame"), "{in_rect}");
    }

    #[test]
    fn a_stroke_cannot_take_an_angle() {
        let mut scene = Scene::new();
        let error = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("line".to_string()),
                points: Some("0,0 50,50".to_string()),
                angle: Some(1.0),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect_err("angle on a line");
        assert!(error.contains("rotating its points"), "{error}");
    }

    #[test]
    fn a_rotated_box_covers_what_its_rotated_region_covers() {
        // A tall thin rect at (100,0) 40x300, rotated a quarter turn, lies
        // flat across y≈130..170 from x≈-30..270. A card at (200,140) sits
        // under the rotated box and NOT under the unrotated one.
        let mut scene = Scene::new();
        let card = (200.0, 140.0, 240.0, 160.0);
        let id = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("rect".to_string()),
                x: Some(100.0),
                y: Some(0.0),
                w: Some(40.0),
                h: Some(300.0),
                angle: Some(std::f64::consts::FRAC_PI_2),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("rect");
        let atlas = read(&scene).expect("read");
        let shape = atlas.shape(&id).expect("shape");
        assert_eq!(
            shape.covers_bounds(card),
            Coverage::Inside,
            "{:?}",
            shape.bounds()
        );
        let (l, t, r, b) = shape.bounds();
        assert!(
            l < -20.0 && r > 260.0 && t > 120.0 && b < 180.0,
            "bounds {:?}",
            (l, t, r, b)
        );
    }

    #[test]
    fn styles_groups_and_frames_survive_an_excalidraw_round_trip() {
        let mut scene = Scene::new();
        let holder = frame(&mut scene, "Before");
        let styled = rect(
            &mut scene,
            ShapePatch {
                stroke_width: Some(4.0),
                stroke_style: Some("dotted".to_string()),
                opacity: Some(40.0),
                roundness: Some("sharp".to_string()),
                font_size: Some(20.0),
                angle: Some(0.5),
                groups: Some("g1".to_string()),
                frame: Some(holder.clone()),
                label: Some("draft".to_string()),
                ..Default::default()
            },
        );
        let words = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("text".to_string()),
                x: Some(400.0),
                y: Some(400.0),
                label: Some("big words".to_string()),
                font_size: Some(36.0),
                groups: Some("g1".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("text");
        let arrow = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(10.0),
                y: Some(10.0),
                points: Some("0,0 80,40".to_string()),
                stroke_style: Some("dashed".to_string()),
                roundness: Some("round".to_string()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("arrow");

        let before = read(&scene).expect("read");
        let document = excalidraw::to_excalidraw(&before);
        let parsed: serde_json::Value = serde_json::from_str(&document).expect("json");
        let find = |id: &str| {
            parsed["elements"]
                .as_array()
                .unwrap()
                .iter()
                .find(|element| element["id"] == id)
                .cloned()
                .unwrap_or_else(|| panic!("no element {id}"))
        };
        let exported = find(&styled);
        assert_eq!(exported["strokeWidth"], 4.0);
        assert_eq!(exported["strokeStyle"], "dotted");
        assert_eq!(exported["opacity"], 40.0);
        assert_eq!(exported["roundness"], serde_json::Value::Null, "sharp rect");
        assert_eq!(exported["angle"], 0.5);
        assert_eq!(exported["groupIds"], serde_json::json!(["g1"]));
        assert_eq!(exported["frameId"], serde_json::json!(holder));
        let caption = find(&format!("{styled}-text"));
        assert_eq!(caption["fontSize"], 20.0);
        assert_eq!(caption["frameId"], serde_json::json!(holder));
        assert_eq!(caption["opacity"], 40.0);
        let exported_frame = find(&holder);
        assert_eq!(exported_frame["type"], "frame");
        assert_eq!(exported_frame["name"], "Before");
        assert_eq!(find(&words)["fontSize"], 36.0);
        assert_eq!(find(&arrow)["roundness"], serde_json::json!({ "type": 2 }));
        assert_eq!(find(&arrow)["strokeStyle"], "dashed");

        let mut landed = Scene::new();
        let import = excalidraw::from_excalidraw(&document).expect("import");
        let result = excalidraw::land_import(&mut landed, import, &Author::Human, 0.0, 0.0);
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert!(
            !result.notes.iter().any(|note| note.contains("frame")),
            "{:?}",
            result.notes
        );
        let after = read(&landed).expect("read back");
        let frame_back = after
            .shapes
            .iter()
            .find(|shape| shape.form == "frame")
            .expect("the frame came back");
        assert_eq!(frame_back.label, "Before");
        let rect_back = after
            .shapes
            .iter()
            .find(|shape| shape.form == "rect" && shape.label == "draft")
            .expect("the styled rect");
        assert_eq!(rect_back.stroke_width, 4.0);
        assert_eq!(rect_back.stroke_style, "dotted");
        assert_eq!(rect_back.opacity, 40.0);
        assert_eq!(rect_back.roundness, "sharp");
        assert_eq!(rect_back.font_size, 20.0);
        assert!((rect_back.angle - 0.5).abs() < 1e-9);
        assert_eq!(rect_back.groups, vec!["g1".to_string()]);
        assert_eq!(
            rect_back.frame, frame_back.id,
            "frame membership was remapped to the local id"
        );
        let text_back = after
            .shapes
            .iter()
            .find(|shape| shape.form == "text")
            .expect("text");
        assert_eq!(text_back.font_size, 36.0);
        let arrow_back = after
            .shapes
            .iter()
            .find(|shape| shape.form == "arrow")
            .expect("arrow");
        assert_eq!(arrow_back.stroke_style, "dashed");
        assert_eq!(arrow_back.roundness, "round");
    }

    #[test]
    fn restyling_a_shape_is_a_reported_change() {
        let mut scene = Scene::new();
        let id = rect(&mut scene, ShapePatch::default());
        let before = read(&scene).expect("read").digest();
        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(id.clone()),
                stroke_style: Some("dashed".to_string()),
                opacity: Some(40.0),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("restyle");
        let after = read(&scene).expect("read").digest();
        let delta = describe_changes_for(&before, &after, "agent");
        assert!(
            delta.contains("was restyled: now (dashed, 40% opacity) — it was plain"),
            "{delta}"
        );
    }

    #[test]
    fn a_member_is_not_read_against_its_own_frame_but_an_outsider_is() {
        let mut scene = Scene::new();
        let holder = frame(&mut scene, "panel");
        let member = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                x: Some(100.0),
                y: Some(100.0),
                points: Some("0,0 60,0".to_string()),
                frame: Some(holder.clone()),
                ..Default::default()
            },
            &Author::Agent,
        )
        .expect("member arrow");
        let outsider = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("line".to_string()),
                x: Some(-50.0),
                y: Some(300.0),
                points: Some("0,0 900,0".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("outsider line");
        let atlas = read(&scene).expect("read");
        let all = readings(&atlas);
        let of = |id: &str| {
            all.iter()
                .find(|reading| reading.shape == id)
                .expect("reading")
        };
        assert_eq!(of(&member).relation, "free", "{:?}", of(&member));
        assert_eq!(of(&outsider).relation, "crosses-out", "{:?}", of(&outsider));
        assert_eq!(of(&outsider).targets, vec![holder.clone()]);
    }

    #[test]
    fn a_member_whose_frame_did_not_import_lands_free_and_says_so() {
        let document = serde_json::json!({
            "type": "excalidraw",
            "version": 2,
            "elements": [
                { "type": "rectangle", "id": "r1", "x": 0, "y": 0, "width": 50, "height": 50,
                  "strokeColor": "#6ea8ff", "frameId": "missing-frame" }
            ]
        })
        .to_string();
        let mut scene = Scene::new();
        let import = excalidraw::from_excalidraw(&document).expect("import");
        let result = excalidraw::land_import(&mut scene, import, &Author::Human, 0.0, 0.0);
        assert_eq!(result.ids.len(), 1);
        assert!(
            result
                .notes
                .iter()
                .any(|note| note.contains("outside their frame")),
            "{:?}",
            result.notes
        );
        let atlas = read(&scene).expect("read");
        assert!(atlas.shapes[0].frame.is_empty());
    }
}

#[cfg(test)]
mod segment_tests {
    use super::*;

    fn proposal() -> SegmentProposal {
        SegmentProposal {
            label: "Mechanical owl".to_string(),
            tags: vec!["visual-instinct".to_string(), "identity-anchor".to_string()],
            ocr: vec!["VISUAL INSTINCT".to_string()],
            prompt_box: [500.0, 180.0, 1040.0, 900.0],
            x: 120.0,
            y: 160.0,
            w: 420.0,
            animation: "none".to_string(),
            parent_id: String::new(),
        }
    }

    fn materialization(id: &str, generation: u32) -> SegmentMaterialization {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        let occlusion_box = if generation > 1 {
            serde_json::json!([700.0, 480.0, 850.0, 650.0])
        } else {
            JsonValue::Null
        };
        SegmentMaterialization {
            generation,
            model_id: SEGMENT_MODEL_ID.to_string(),
            encoder_sha256: SEGMENT_ENCODER_SHA256.to_string(),
            decoder_sha256: SEGMENT_DECODER_SHA256.to_string(),
            predicted_iou: 0.91,
            mask_encoding: SEGMENT_MASK_ENCODING.to_string(),
            mask_width: 2,
            mask_height: 2,
            mask_runs: base64::engine::general_purpose::STANDARD.encode(bytes),
            mask_source_box: [500.0, 180.0, 1040.0, 900.0],
            parts: Vec::new(),
            receipt: serde_json::json!({
                "modelId": SEGMENT_MODEL_ID,
                "backendRequested": "webgpu",
                "executionLocation": "browser-worker",
                "sourceSha256": SEGMENT_SOURCE_SHA256,
                "encoderSha256": SEGMENT_ENCODER_SHA256,
                "decoderSha256": SEGMENT_DECODER_SHA256,
                "stableObjectId": id,
                "generation": generation,
                "sourceSize": [SEGMENT_SOURCE_WIDTH, SEGMENT_SOURCE_HEIGHT],
                "maskSourceBox": [500.0, 180.0, 1040.0, 900.0],
                "materializedMaskSize": [2, 2],
                "predictedIoU": 0.91,
                "predictedIoUContract": "raw-regression-output-not-probability",
                "predictedIoURawInUnitInterval": true,
                "occlusionApplied": generation > 1,
                "occlusionBox": occlusion_box,
                "alternateProviderConfigured": false,
                "adapterBoundToRuntime": true,
                "fallbackUsed": false,
                "mockUsed": false
            }),
        }
    }

    fn wing_part(
        id: &str,
        generation: u32,
        role: &str,
        prompt_box: [f64; 4],
    ) -> SegmentPartMaterialization {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        SegmentPartMaterialization {
            role: role.to_string(),
            prompt_box,
            predicted_iou: 1.003535509109497,
            mask_encoding: SEGMENT_MASK_ENCODING.to_string(),
            mask_width: 2,
            mask_height: 2,
            mask_runs: base64::engine::general_purpose::STANDARD.encode(bytes),
            mask_source_box: prompt_box,
            receipt: serde_json::json!({
                "role": role,
                "modelId": SEGMENT_MODEL_ID,
                "backendRequested": "webgpu",
                "executionLocation": "browser-worker",
                "sourceSha256": SEGMENT_SOURCE_SHA256,
                "encoderSha256": SEGMENT_ENCODER_SHA256,
                "decoderSha256": SEGMENT_DECODER_SHA256,
                "stableObjectId": id,
                "generation": generation,
                "promptBox": prompt_box,
                "maskSourceBox": prompt_box,
                "materializedMaskSize": [2, 2],
                "predictedIoU": 1.003535509109497,
                "predictedIoUContract": "raw-regression-output-not-probability",
                "predictedIoURawInUnitInterval": false,
                "alternateProviderConfigured": false,
                "adapterBoundToRuntime": true,
                "fallbackUsed": false,
                "mockUsed": false
            }),
        }
    }

    fn wing_materialization(id: &str, generation: u32) -> SegmentMaterialization {
        let left = [540.0, 430.0, 700.0, 770.0];
        let right = [840.0, 430.0, 1000.0, 770.0];
        let mut result = materialization(id, generation);
        result.parts = vec![
            wing_part(id, generation, "left-wing", left),
            wing_part(id, generation, "right-wing", right),
        ];
        result.receipt["partCount"] = serde_json::json!(2);
        result.receipt["partRoles"] = serde_json::json!(["left-wing", "right-wing"]);
        result.receipt["partPromptBoxes"] = serde_json::json!([left, right]);
        result.receipt["occlusionApplied"] = serde_json::json!(false);
        result.receipt["occlusionBox"] = JsonValue::Null;
        result
    }

    fn semantic_receipt(proposal: &SegmentProposal) -> SegmentSemanticReceipt {
        SegmentSemanticReceipt {
            schema: SEGMENT_SEMANTIC_RECEIPT_SCHEMA.to_string(),
            provider: "test-provider".to_string(),
            model: "test-vision-model".to_string(),
            request_id: "test-request-1".to_string(),
            execution_location: "external-multimodal-agent".to_string(),
            trust: "participant-claimed".to_string(),
            source_sha256: SEGMENT_SOURCE_SHA256.to_string(),
            source_size: [SEGMENT_SOURCE_WIDTH, SEGMENT_SOURCE_HEIGHT],
            input_mime_type: "image/png".to_string(),
            output_sha256: segment_semantic_output_sha256(proposal).expect("semantic digest"),
            fallback_used: false,
            mock_used: false,
        }
    }

    #[test]
    fn atlas_mints_then_browser_materializes_one_stable_segment() {
        let mut scene = Scene::new();
        let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");
        let proposed = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(proposed.form, "segment");
        assert_eq!(proposed.created_by, "agent");
        assert_eq!(proposed.touched_by, "agent");
        assert_eq!(proposed.segment_status, "proposed");
        assert_eq!(proposed.segment_generation, 1);
        assert_eq!(proposed.segment_semantics_source, "oracle");
        assert!(proposed.segment_semantics_receipt.is_null());

        materialize_segment(&mut scene, &id, &materialization(&id, 1)).expect("materialize");
        let materialized = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(materialized.id, id);
        assert_eq!(materialized.segment_status, "materialized");
        assert_eq!(materialized.created_by, "agent");
        assert_eq!(materialized.touched_by, "agent");
        assert_eq!(materialized.segment_model_id, SEGMENT_MODEL_ID);
        assert_eq!(materialized.segment_generation, 1);
        assert!((materialized.h - 560.0).abs() < 1e-9);
    }

    #[test]
    fn human_mask_acceptance_mints_and_materializes_one_atlas_object() {
        let mut scene = Scene::new();
        let mut result = materialization("not-yet-bound", 1);
        let id = accept_human_segment(&mut scene, &proposal(), &mut result, &Author::Human)
            .expect("accept human-selected mask");

        let atlas = read(&scene).expect("read accepted object");
        let segment = atlas.shape(&id).expect("accepted segment");
        assert_eq!(atlas.shapes.len(), 1);
        assert_eq!(segment.id, id);
        assert_eq!(segment.created_by, "human");
        assert_eq!(segment.touched_by, "human");
        assert_eq!(segment.segment_status, "materialized");
        assert_eq!(segment.segment_semantics_source, "human-prompt");
        assert_eq!(segment.segment_generation, 1);
        assert_eq!(
            segment
                .segment_receipt
                .get("stableObjectId")
                .and_then(JsonValue::as_str),
            Some(id.as_str())
        );
        assert!(atlas.describe().contains("LAST-EDITED-BY=human"));
    }

    #[test]
    fn human_mask_acceptance_uses_a_second_generated_infographic_source() {
        let source = segment_source_by_id("evidence-loop-stages-v1").expect("evidence source");
        let mut scene = Scene::new();
        let mut evidence_proposal = proposal();
        evidence_proposal.label = "Evidence loop".to_string();
        evidence_proposal.prompt_box = [560.0, 300.0, 980.0, 690.0];
        let mut result = materialization("not-yet-bound", 1);
        result.mask_source_box = evidence_proposal.prompt_box;
        result.receipt["sourceSha256"] = serde_json::json!(source.sha256);
        result.receipt["sourceSize"] = serde_json::json!([source.width, source.height]);
        result.receipt["maskSourceBox"] = serde_json::json!(evidence_proposal.prompt_box);

        let id = accept_human_segment(&mut scene, &evidence_proposal, &mut result, &Author::Human)
            .expect("accept evidence-loop mask");
        let atlas = read(&scene).expect("read evidence-loop mask");
        let segment = atlas.shape(&id).expect("evidence-loop segment");
        assert_eq!(segment.segment_source_url, source.url);
        assert_eq!(segment.segment_source_sha256, source.sha256);
        assert_eq!(segment.segment_source_width, source.width);
        assert_eq!(segment.segment_source_height, source.height);
    }

    #[test]
    fn unknown_or_cross_source_human_masks_leave_no_partial_object() {
        let mut scene = Scene::new();
        let mut unknown = materialization("not-yet-bound", 1);
        unknown.receipt["sourceSha256"] = serde_json::json!("0".repeat(64));
        let error = accept_human_segment(&mut scene, &proposal(), &mut unknown, &Author::Human)
            .expect_err("unknown source must fail");
        assert!(error.contains("generated source"), "{error}");
        assert!(read(&scene)
            .expect("read after rejection")
            .shapes
            .is_empty());

        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("default-source root");
        let source = segment_source_by_id("evidence-loop-stages-v1").expect("evidence source");
        let mut part_proposal = proposal();
        part_proposal.parent_id = root;
        part_proposal.w = 24.0;
        let mut part_result = materialization("not-yet-bound", 1);
        part_result.receipt["sourceSha256"] = serde_json::json!(source.sha256);
        part_result.receipt["sourceSize"] = serde_json::json!([source.width, source.height]);
        let error =
            accept_human_segment(&mut scene, &part_proposal, &mut part_result, &Author::Human)
                .expect_err("cross-source part must fail");
        assert!(error.contains("same generated source"), "{error}");
        assert_eq!(read(&scene).expect("read root only").shapes.len(), 1);
    }

    #[test]
    fn human_masks_form_one_bounded_root_part_tree_with_a_persisted_pivot() {
        let mut scene = Scene::new();
        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("root mask");

        let mut part_proposal = proposal();
        part_proposal.label = "Left eye".to_string();
        part_proposal.parent_id = root.clone();
        part_proposal.w = 24.0;
        let mut part_result = materialization("not-yet-bound", 1);
        let part =
            accept_human_segment(&mut scene, &part_proposal, &mut part_result, &Author::Human)
                .expect("part mask");

        set_segment_pivot(&mut scene, &root, 0.5, 1.0, &Author::Human)
            .expect("bottom-center pivot");
        set_segment_animation(&mut scene, &root, "rock", &Author::Human).expect("rock animation");
        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(part.clone()),
                label: Some("Near eye".to_string()),
                ..ShapePatch::default()
            },
            &Author::Human,
        )
        .expect("rename part");

        let atlas = read(&scene).expect("read hierarchy");
        let root_shape = atlas.shape(&root).expect("root segment");
        let part_shape = atlas.shape(&part).expect("part segment");
        assert_eq!(root_shape.segment_animation, "rock");
        assert_eq!(
            (root_shape.segment_pivot_x, root_shape.segment_pivot_y),
            (0.5, 1.0)
        );
        assert_eq!(part_shape.segment_parent_id, root);
        assert_eq!(part_shape.label, "Near eye");
        assert_eq!(part_shape.w, 24.0);
        assert!(atlas.describe().contains("parent=root"));
        assert!(atlas.describe().contains("parent="));
    }

    #[test]
    fn tap_parts_is_one_root_behavior_and_refuses_a_part_or_empty_root() {
        let mut scene = Scene::new();
        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("root mask");

        let empty_error = set_segment_animation(&mut scene, &root, "tap-parts", &Author::Human)
            .expect_err("empty root must not accept a part behavior");
        assert!(
            empty_error.contains("requires at least one"),
            "{empty_error}"
        );

        let mut part_proposal = proposal();
        part_proposal.label = "Claw 1".to_string();
        part_proposal.parent_id = root.clone();
        part_proposal.w = 24.0;
        let mut part_result = materialization("not-yet-bound", 1);
        let part =
            accept_human_segment(&mut scene, &part_proposal, &mut part_result, &Author::Human)
                .expect("part mask");

        set_segment_animation(&mut scene, &root, "tap-parts", &Author::Human)
            .expect("root part behavior");
        let part_error = set_segment_animation(&mut scene, &part, "tap-parts", &Author::Human)
            .expect_err("part must not own the group behavior");
        assert!(part_error.contains("root segment"), "{part_error}");

        let atlas = read(&scene).expect("read part behavior");
        assert_eq!(
            atlas.shape(&root).expect("root").segment_animation,
            "tap-parts"
        );
        assert_eq!(atlas.shape(&part).expect("part").segment_animation, "none");
    }

    #[test]
    fn expressive_motion_targets_exact_parts_without_changing_saved_positions() {
        let mut scene = Scene::new();
        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("root mask");
        let mut part_ids = Vec::new();
        for (label, x) in [("Left claw", 210.0), ("Right claw", 330.0)] {
            let mut part_proposal = proposal();
            part_proposal.label = label.to_string();
            part_proposal.parent_id = root.clone();
            part_proposal.x = x;
            part_proposal.w = 24.0;
            let mut part_result = materialization("not-yet-bound", 1);
            part_ids.push(
                accept_human_segment(&mut scene, &part_proposal, &mut part_result, &Author::Human)
                    .expect("part mask"),
            );
        }
        let before = read(&scene)
            .expect("read positions")
            .shapes
            .iter()
            .map(|shape| (shape.id.clone(), shape.x, shape.y))
            .collect::<Vec<_>>();
        set_segment_animation(&mut scene, &root, "tap-parts", &Author::Human)
            .expect("legacy animation");

        let motion = SegmentMotionProgram {
            schema: SEGMENT_MOTION_SCHEMA.to_string(),
            label: "Alternating claw loading".to_string(),
            child_compositing: "cutout".to_string(),
            tracks: vec![SegmentMotionTrack {
                label: "Claw taps".to_string(),
                target_ids: part_ids.clone(),
                duration_ms: 900,
                delay_ms: 40,
                stagger_ms: 110,
                r#loop: true,
                alternate: false,
                curve: Some([0.25, 0.1, 0.25, 1.0]),
                origin: Some([0.5, 1.0]),
                keyframes: vec![
                    SegmentMotionKeyframe {
                        at: 0.0,
                        x: 0.0,
                        y: 0.0,
                        rotate: 0.0,
                        scale_x: 1.0,
                        scale_y: 1.0,
                        opacity: 1.0,
                    },
                    SegmentMotionKeyframe {
                        at: 0.35,
                        x: 0.0,
                        y: 11.0,
                        rotate: 4.0,
                        scale_x: 0.96,
                        scale_y: 1.04,
                        opacity: 0.8,
                    },
                    SegmentMotionKeyframe {
                        at: 1.0,
                        x: 0.0,
                        y: 0.0,
                        rotate: 0.0,
                        scale_x: 1.0,
                        scale_y: 1.0,
                        opacity: 1.0,
                    },
                ],
            }],
        };

        assert_eq!(
            set_segment_motion(&mut scene, &root, Some(&motion), &Author::Agent)
                .expect("targeted motion"),
            2
        );
        let atlas = read(&scene).expect("read motion");
        let root_shape = atlas.shape(&root).expect("root");
        assert_eq!(root_shape.segment_motion, serde_json::json!(motion));
        assert_eq!(root_shape.segment_animation, "none");
        assert_eq!(root_shape.touched_by, "agent");
        assert_eq!(
            atlas
                .shapes
                .iter()
                .map(|shape| (shape.id.clone(), shape.x, shape.y))
                .collect::<Vec<_>>(),
            before
        );
        assert!(atlas
            .describe()
            .contains("motion=\"Alternating claw loading\" compositing=cutout tracks=1 targets=2"));

        let mut unrelated_result = materialization("not-yet-bound", 1);
        let unrelated = accept_human_segment(
            &mut scene,
            &proposal(),
            &mut unrelated_result,
            &Author::Human,
        )
        .expect("unrelated root");
        let mut invalid = motion.clone();
        invalid.tracks[0].target_ids.push(unrelated);
        let error = set_segment_motion(&mut scene, &root, Some(&invalid), &Author::Agent)
            .expect_err("unrelated target");
        assert!(error.contains("cannot target"), "{error}");
        assert_eq!(
            read(&scene)
                .expect("read after refusal")
                .shape(&root)
                .expect("root after refusal")
                .segment_motion,
            serde_json::json!(motion)
        );

        set_segment_motion(&mut scene, &root, None, &Author::Human).expect("clear motion");
        assert!(read(&scene)
            .expect("read cleared")
            .shape(&root)
            .expect("root cleared")
            .segment_motion
            .is_null());
    }

    #[test]
    fn segment_hierarchy_accepts_nested_parts_and_rejects_depth_overflow() {
        let mut scene = Scene::new();
        let mut dangling = proposal();
        dangling.parent_id = "missing-root".to_string();
        let mut result = materialization("not-yet-bound", 1);
        let error = accept_human_segment(&mut scene, &dangling, &mut result, &Author::Human)
            .expect_err("dangling parent");
        assert!(error.contains("no parent segment"), "{error}");
        assert!(read(&scene).expect("read refusal").shapes.is_empty());

        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("root mask");
        let mut part_proposal = proposal();
        part_proposal.parent_id = root;
        let mut part_result = materialization("not-yet-bound", 1);
        let part =
            accept_human_segment(&mut scene, &part_proposal, &mut part_result, &Author::Human)
                .expect("part mask");
        let mut nested = proposal();
        nested.parent_id = part.clone();
        nested.w = 16.0;
        let mut nested_result = materialization("not-yet-bound", 1);
        let nested = accept_human_segment(&mut scene, &nested, &mut nested_result, &Author::Human)
            .expect("nested part");

        let mut deepest = proposal();
        deepest.parent_id = nested.clone();
        deepest.w = 12.0;
        let mut deepest_result = materialization("not-yet-bound", 1);
        let deepest =
            accept_human_segment(&mut scene, &deepest, &mut deepest_result, &Author::Human)
                .expect("third nested tier");
        let before = read(&scene).expect("before depth refusal").shapes.len();

        let mut too_deep = proposal();
        too_deep.parent_id = deepest;
        too_deep.w = 8.0;
        let mut too_deep_result = materialization("not-yet-bound", 1);
        let error =
            accept_human_segment(&mut scene, &too_deep, &mut too_deep_result, &Author::Human)
                .expect_err("depth overflow");
        assert!(error.contains("at most"), "{error}");
        assert_eq!(
            read(&scene).expect("read depth refusal").shapes.len(),
            before
        );
    }

    #[test]
    fn segment_reparent_preserves_geometry_and_motion_reaches_nested_descendants() {
        let mut scene = Scene::new();
        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("root mask");
        let mut foot_proposal = proposal();
        foot_proposal.label = "Left foot".to_string();
        foot_proposal.parent_id = root.clone();
        foot_proposal.x = 240.0;
        foot_proposal.w = 48.0;
        let mut foot_result = materialization("not-yet-bound", 1);
        let foot =
            accept_human_segment(&mut scene, &foot_proposal, &mut foot_result, &Author::Human)
                .expect("foot mask");
        let mut claw_proposal = proposal();
        claw_proposal.label = "Claw 1".to_string();
        claw_proposal.parent_id = root.clone();
        claw_proposal.x = 251.0;
        claw_proposal.w = 12.0;
        let mut claw_result = materialization("not-yet-bound", 1);
        let claw =
            accept_human_segment(&mut scene, &claw_proposal, &mut claw_result, &Author::Human)
                .expect("claw mask");
        let before = read(&scene).expect("before reparent");
        let before_claw = before.shape(&claw).expect("claw before").clone();

        set_segment_parent(&mut scene, &claw, &foot, &Author::Agent).expect("nest claw");
        let nested = read(&scene).expect("nested hierarchy");
        let nested_claw = nested.shape(&claw).expect("nested claw");
        assert_eq!(nested_claw.segment_parent_id, foot);
        assert_eq!(
            (nested_claw.x, nested_claw.y),
            (before_claw.x, before_claw.y)
        );
        assert_eq!(
            nested_claw.segment_generation,
            before_claw.segment_generation
        );

        let motion = SegmentMotionProgram {
            schema: SEGMENT_MOTION_SCHEMA.to_string(),
            label: "Nested claw tap".to_string(),
            child_compositing: "cutout".to_string(),
            tracks: vec![SegmentMotionTrack {
                label: "Claw".to_string(),
                target_ids: vec![claw.clone()],
                duration_ms: 600,
                delay_ms: 0,
                stagger_ms: 0,
                r#loop: true,
                alternate: false,
                curve: None,
                origin: Some([0.5, 0.0]),
                keyframes: vec![
                    SegmentMotionKeyframe {
                        at: 0.0,
                        x: 0.0,
                        y: 0.0,
                        rotate: 0.0,
                        scale_x: 1.0,
                        scale_y: 1.0,
                        opacity: 1.0,
                    },
                    SegmentMotionKeyframe {
                        at: 1.0,
                        x: 0.0,
                        y: 2.0,
                        rotate: 2.0,
                        scale_x: 1.0,
                        scale_y: 0.95,
                        opacity: 1.0,
                    },
                ],
            }],
        };
        assert_eq!(
            set_segment_motion(&mut scene, &root, Some(&motion), &Author::Agent)
                .expect("root targets nested claw"),
            1
        );
        let cycle = set_segment_parent(&mut scene, &foot, &claw, &Author::Agent)
            .expect_err("cycle refused");
        assert!(cycle.contains("cycle"), "{cycle}");
        remove(&scene, &foot).expect("remove foot");
        assert_eq!(
            read(&scene)
                .expect("promoted claw")
                .shape(&claw)
                .expect("claw remains")
                .segment_parent_id,
            root
        );
    }

    #[test]
    fn removing_a_segment_root_promotes_its_parts_instead_of_deleting_them() {
        let mut scene = Scene::new();
        let mut root_result = materialization("not-yet-bound", 1);
        let root = accept_human_segment(&mut scene, &proposal(), &mut root_result, &Author::Human)
            .expect("root mask");
        let mut part_proposal = proposal();
        part_proposal.parent_id = root.clone();
        let mut part_result = materialization("not-yet-bound", 1);
        let part =
            accept_human_segment(&mut scene, &part_proposal, &mut part_result, &Author::Human)
                .expect("part mask");

        remove(&scene, &root).expect("remove root");

        let atlas = read(&scene).expect("read promoted part");
        assert!(atlas.shape(&root).is_none());
        assert_eq!(
            atlas.shape(&part).expect("part remains").segment_parent_id,
            ""
        );
    }

    #[test]
    fn rejected_human_mask_acceptance_leaves_no_partial_object() {
        let mut scene = Scene::new();
        let mut result = materialization("not-yet-bound", 1);
        result.encoder_sha256 = "0".repeat(64);

        let error = accept_human_segment(&mut scene, &proposal(), &mut result, &Author::Human)
            .expect_err("wrong graph must fail closed");

        assert!(error.contains("pinned MobileSAM"), "{error}");
        assert!(read(&scene)
            .expect("read after rejected acceptance")
            .shapes
            .is_empty());
    }

    #[test]
    fn multimodal_agent_semantics_are_bound_before_atlas_mints_the_id() {
        let mut scene = Scene::new();
        let proposal = proposal();
        let receipt = semantic_receipt(&proposal);
        let id = propose_agent_segment(&mut scene, &proposal, &receipt, &Author::Agent)
            .expect("receipt-bound proposal");
        let segment = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(segment.segment_semantics_source, "multimodal-agent");
        assert_eq!(
            segment
                .segment_semantics_receipt
                .get("model")
                .and_then(JsonValue::as_str),
            Some("test-vision-model")
        );
        assert_eq!(
            segment
                .segment_semantics_receipt
                .get("output_sha256")
                .and_then(JsonValue::as_str),
            Some(receipt.output_sha256.as_str())
        );
        assert!(read(&scene)
            .expect("read")
            .describe()
            .contains("semantics=multimodal-agent:test-vision-model"));
    }

    #[test]
    fn multimodal_agent_receipt_rejects_output_drift_mock_and_wrong_source_without_minting() {
        let proposal = proposal();
        for (name, mutate) in [("output drift", 0u8), ("mock", 1u8), ("wrong source", 2u8)] {
            let mut scene = Scene::new();
            let mut receipt = semantic_receipt(&proposal);
            match mutate {
                0 => receipt.output_sha256 = "0".repeat(64),
                1 => receipt.mock_used = true,
                2 => receipt.source_sha256 = "0".repeat(64),
                _ => unreachable!(),
            }
            let error = propose_agent_segment(&mut scene, &proposal, &receipt, &Author::Agent)
                .expect_err(name);
            assert!(!error.is_empty(), "{name} must explain its refusal");
            assert!(read(&scene).expect("read after refusal").shapes.is_empty());
        }
    }

    #[test]
    fn materialization_fails_closed_on_stale_generation_bad_graph_or_bad_rle() {
        let mut scene = Scene::new();
        let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");

        let stale = materialization(&id, 2);
        assert!(materialize_segment(&mut scene, &id, &stale)
            .expect_err("stale generation")
            .contains("expects proposed generation 1"));

        let mut wrong_graph = materialization(&id, 1);
        wrong_graph.encoder_sha256 = "0".repeat(64);
        assert!(materialize_segment(&mut scene, &id, &wrong_graph)
            .expect_err("wrong graph")
            .contains("pinned MobileSAM"));

        let mut wrong_rle = materialization(&id, 1);
        wrong_rle.mask_runs = base64::engine::general_purpose::STANDARD.encode(3u32.to_le_bytes());
        assert!(materialize_segment(&mut scene, &id, &wrong_rle)
            .expect_err("wrong pixel count")
            .contains("covers 3 pixels, expected 4"));

        let mut wrong_receipt = materialization(&id, 1);
        wrong_receipt.receipt["predictedIoU"] = serde_json::json!(0.5);
        assert!(materialize_segment(&mut scene, &id, &wrong_receipt)
            .expect_err("wrong receipt")
            .contains("predictedIoU does not match"));
    }

    #[test]
    fn raw_predicted_iou_is_preserved_even_when_the_regressor_exceeds_one() {
        let mut scene = Scene::new();
        let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");
        let mut result = materialization(&id, 1);
        result.predicted_iou = 1.003535509109497;
        result.receipt["predictedIoU"] = serde_json::json!(result.predicted_iou);
        result.receipt["predictedIoURawInUnitInterval"] = serde_json::json!(false);
        materialize_segment(&mut scene, &id, &result).expect("raw regressor score");
        let segment = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(segment.segment_predicted_iou, 1.003535509109497);
    }

    #[test]
    fn wing_request_and_human_flap_keep_one_parent_identity() {
        let mut scene = Scene::new();
        let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");
        let generation = request_segment_wings(
            &mut scene,
            &id,
            [540.0, 430.0, 700.0, 770.0],
            [840.0, 430.0, 1000.0, 770.0],
            &Author::Agent,
        )
        .expect("wing request");
        assert_eq!(generation, 2);

        materialize_segment(&mut scene, &id, &wing_materialization(&id, generation))
            .expect("wing materialization");
        set_segment_flap(&mut scene, &id, 0.75, &Author::Human).expect("human flap");

        let atlas = read(&scene).expect("read");
        let segment = atlas.shape(&id).expect("segment");
        assert_eq!(atlas.shapes.len(), 1);
        assert_eq!(segment.id, id);
        assert_eq!(segment.segment_generation, 2);
        assert_eq!(segment.segment_parts.as_array().map(Vec::len), Some(2));
        assert_eq!(segment.segment_flap, 0.75);
        assert_eq!(segment.created_by, "agent");
        assert_eq!(segment.touched_by, "human");
    }

    #[test]
    fn wing_materialization_rejects_incomplete_duplicate_and_prompt_drifted_parts() {
        for case in ["incomplete", "duplicate", "prompt-drift"] {
            let mut scene = Scene::new();
            let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");
            let generation = request_segment_wings(
                &mut scene,
                &id,
                [540.0, 430.0, 700.0, 770.0],
                [840.0, 430.0, 1000.0, 770.0],
                &Author::Agent,
            )
            .expect("wing request");
            let mut result = wing_materialization(&id, generation);
            match case {
                "incomplete" => {
                    result.parts.pop();
                    result.receipt["partCount"] = serde_json::json!(1);
                    result.receipt["partRoles"] = serde_json::json!(["left-wing"]);
                    result.receipt["partPromptBoxes"] =
                        serde_json::json!([[540.0, 430.0, 700.0, 770.0]]);
                }
                "duplicate" => result.parts[1].role = "left-wing".to_string(),
                "prompt-drift" => result.parts[0].prompt_box[0] += 1.0,
                _ => unreachable!(),
            }
            let error = materialize_segment(&mut scene, &id, &result).expect_err(case);
            assert!(error.contains("wing"), "{case}: {error}");
            assert_eq!(
                read(&scene)
                    .expect("read after refusal")
                    .shape(&id)
                    .expect("segment")
                    .segment_status,
                "proposed"
            );
        }
    }

    #[test]
    fn human_move_agent_animation_and_occluded_regeneration_keep_identity() {
        let mut scene = Scene::new();
        let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");
        materialize_segment(&mut scene, &id, &materialization(&id, 1)).expect("materialize");

        place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(id.clone()),
                x: Some(240.0),
                y: Some(260.0),
                ..ShapePatch::default()
            },
            &Author::Human,
        )
        .expect("human move");
        let moved = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(moved.id, id);
        assert_eq!(moved.touched_by, "human");
        assert_eq!(moved.segment_generation, 1);

        set_segment_animation(&mut scene, &id, "pulse", &Author::Agent).expect("animate");
        let animated = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(animated.id, id);
        assert_eq!(animated.segment_animation, "pulse");
        assert_eq!(animated.touched_by, "agent");

        let generation = regenerate_segment(
            &mut scene,
            &id,
            [500.0, 180.0, 1040.0, 900.0],
            [700.0, 480.0, 850.0, 650.0],
            &Author::Agent,
        )
        .expect("regenerate");
        assert_eq!(generation, 2);
        materialize_segment(&mut scene, &id, &materialization(&id, 2)).expect("materialize again");
        let regenerated = read(&scene)
            .expect("read")
            .shape(&id)
            .cloned()
            .expect("segment");
        assert_eq!(regenerated.id, id);
        assert_eq!(regenerated.segment_generation, 2);
        assert_eq!(
            regenerated.segment_occlusion_box,
            vec![700.0, 480.0, 850.0, 650.0]
        );
        assert_eq!(regenerated.segment_status, "materialized");
    }

    #[test]
    fn ordinary_shape_creation_cannot_forge_a_segment() {
        let mut scene = Scene::new();
        let error = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("segment".to_string()),
                x: Some(0.0),
                y: Some(0.0),
                w: Some(100.0),
                h: Some(100.0),
                ..ShapePatch::default()
            },
            &Author::Agent,
        )
        .expect_err("ordinary segment creation");
        assert!(error.contains("form must be one of"), "{error}");

        let id = propose_segment(&mut scene, &proposal(), &Author::Agent).expect("proposal");
        let error = place_shape(
            &mut scene,
            &ShapePatch {
                id: Some(id.clone()),
                form: Some("rect".to_string()),
                ..ShapePatch::default()
            },
            &Author::Human,
        )
        .expect_err("segment form change");
        assert!(error.contains("keeps its form"), "{error}");
        assert_eq!(
            read(&scene)
                .expect("read")
                .shape(&id)
                .expect("segment")
                .form,
            "segment"
        );
    }

    fn explanation_fixture(a: &str, b: &str) -> ExplanationFlowDefinition {
        ExplanationFlowDefinition {
            about: vec![],
            scene_ids: vec![],
            schema: EXPLANATION_FLOW_SCHEMA.to_string(),
            id: "distance-check".to_string(),
            title: "Distance check".to_string(),
            goal: "Decide from visible evidence whether a candidate is inside.".to_string(),
            start: "observe".to_string(),
            beats: vec![
                ExplanationBeat {
                    id: "observe".to_string(),
                    title: "Observe the comparison".to_string(),
                    intent: "Connect the displayed inequality to the classification.".to_string(),
                    cue: "The boundary is included because the comparison uses less than or equal."
                        .to_string(),
                    evidence: vec![ExplanationEvidence {
                        target_id: a.to_string(),
                        detail: "The displayed distance is no greater than epsilon.".to_string(),
                    }],
                    actions: vec![ExplanationVisualAction {
                        kind: "point".to_string(),
                        target_id: a.to_string(),
                        cue_span: Some([4, 12]),
                    }],
                    advance: ExplanationAdvance {
                        mode: "agent".to_string(),
                        prompt: "Is it inside or beyond?".to_string(),
                        transitions: vec![
                            ExplanationTransition {
                                id: "inside".to_string(),
                                label: "Inside".to_string(),
                                control: Some("button".to_string()),
                                next: Some("contrast".to_string()),
                                target_ids: vec![a.to_string()],
                                phrases: vec!["inside".to_string(), "yes".to_string()],
                            },
                            ExplanationTransition {
                                id: "beyond".to_string(),
                                label: "Beyond".to_string(),
                                control: Some("button".to_string()),
                                next: Some("contrast".to_string()),
                                target_ids: vec![b.to_string()],
                                phrases: vec!["beyond".to_string(), "no".to_string()],
                            },
                        ],
                    },
                },
                ExplanationBeat {
                    id: "contrast".to_string(),
                    title: "Contrast the other case".to_string(),
                    intent: "Transfer the same rule to a second visible case.".to_string(),
                    cue: "Now compare the second candidate with the same threshold.".to_string(),
                    evidence: vec![ExplanationEvidence {
                        target_id: b.to_string(),
                        detail: "The second candidate supplies new visible evidence.".to_string(),
                    }],
                    actions: vec![ExplanationVisualAction {
                        kind: "reveal".to_string(),
                        target_id: b.to_string(),
                        cue_span: None,
                    }],
                    advance: ExplanationAdvance {
                        mode: "terminal".to_string(),
                        prompt: String::new(),
                        transitions: Vec::new(),
                    },
                },
            ],
        }
    }

    fn explanation_targets(scene: &mut Scene) -> (String, String) {
        let first = place_node(
            scene,
            &NodePatch {
                label: Some("Inside evidence".to_string()),
                ..NodePatch::default()
            },
            &Author::Human,
        )
        .expect("first explanation target");
        let second = place_node(
            scene,
            &NodePatch {
                label: Some("Beyond evidence".to_string()),
                ..NodePatch::default()
            },
            &Author::Human,
        )
        .expect("second explanation target");
        (first, second)
    }

    #[test]
    fn concurrent_exploration_identity_and_ownership_fail_closed() {
        for same_name in [true, false] {
            let mut left = Scene::new();
            let (inside, beyond) = explanation_targets(&mut left);
            let mut right =
                Scene::from_state(&left.encode_full().expect("baseline")).expect("replica");
            let mut definition = explanation_fixture(&beyond, &beyond);
            definition.about = vec![inside];
            definition.scene_ids = vec![beyond];
            define_explanation_flow(&mut left, &definition, &Author::Agent).expect("left room");
            if !same_name {
                definition.id = "parallel-question".to_string();
            }
            let conflicting = define_explanation_flow(&mut right, &definition, &Author::Human)
                .expect("right room");
            left.apply_update(&right.encode_full().expect("update"))
                .expect("merge");
            assert!(read(&left)
                .expect_err("ambiguous merged scope")
                .contains(if same_name { "identity" } else { "ownership" }));
            remove(&left, &conflicting.object_id)
                .expect("remove conflicting definition without reading ambiguous projection");
            assert_eq!(read(&left).expect("recovered").explanations.len(), 1);
        }
    }

    #[test]
    fn drawing_inside_an_exploration_retains_scope_and_cursor() {
        let mut scene = Scene::new();
        let (inside, beyond) = explanation_targets(&mut scene);
        let mut definition = explanation_fixture(&beyond, &beyond);
        definition.about = vec![inside];
        definition.scene_ids = vec![beyond];
        define_explanation_flow(&mut scene, &definition, &Author::Agent).expect("room");
        let id = place_exploration_shape(
            &mut scene,
            &definition.id,
            &ShapePatch {
                form: Some("rect".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("draw");
        let atlas = read(&scene).expect("read");
        let flow = &atlas.explanations[0];
        assert!(flow.definition.scene_ids.contains(&id));
        assert_eq!(flow.state.current_beat, "observe");
        assert_eq!(flow.state.entered_revision, 1);
        assert_eq!(flow.state.revision, 1);
        let mut peer = Scene::from_state(&scene.encode_full().expect("clone")).expect("peer");
        let a = place_exploration_shape(
            &mut scene,
            &definition.id,
            &ShapePatch {
                form: Some("rect".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("left ink");
        control_explanation_flow(&scene, &definition.id, 1, "pause", &Author::Human)
            .expect("pause during drawing");
        let b = place_exploration_node(
            &mut peer,
            &definition.id,
            &NodePatch {
                label: Some("local thought".to_string()),
                ..Default::default()
            },
            &Author::Human,
        )
        .expect("right node");
        scene
            .apply_update(&peer.encode_full().expect("peer state"))
            .expect("merge drawings");
        let merged = read(&scene).expect("merged");
        assert!(merged.explanations[0].definition.scene_ids.contains(&a));
        assert!(merged.explanations[0].definition.scene_ids.contains(&b));
        assert_eq!(merged.explanations[0].state.status, "paused");
    }

    #[test]
    fn scoped_explanations_coexist_and_reject_cross_room_ownership() {
        let mut scene = Scene::new();
        let (inside, beyond) = explanation_targets(&mut scene);
        let mut first = explanation_fixture(&beyond, &beyond);
        first.about = vec![inside.clone()];
        first.scene_ids = vec![beyond.clone()];
        define_explanation_flow(&mut scene, &first, &Author::Agent).expect("first room");
        let mut second = explanation_fixture(&inside, &beyond);
        second.id = "second-question".to_string();
        second.about = vec![inside.clone()];
        define_explanation_flow(&mut scene, &second, &Author::Agent).expect("second room");
        control_explanation_flow(&scene, &first.id, 1, "pause", &Author::Human)
            .expect("independent pause");
        let atlas = read(&scene).expect("read both rooms");
        assert_eq!(atlas.explanations.len(), 2);
        assert_eq!(
            atlas
                .explanations
                .iter()
                .find(|flow| flow.definition.id == second.id)
                .expect("second")
                .state
                .status,
            "active"
        );
        assert!(atlas.describe().contains("second-question"));
        let before = scene.encode_full().expect("before invalid scope");
        second.scene_ids = vec![beyond.clone(), beyond.clone()];
        assert!(define_explanation_flow(&mut scene, &second, &Author::Agent)
            .expect_err("duplicate scope")
            .contains("duplicate"));
        assert_eq!(
            before,
            scene.encode_full().expect("duplicate refusal is atomic")
        );
        second.scene_ids = vec![beyond.clone()];
        assert!(define_explanation_flow(&mut scene, &second, &Author::Agent)
            .expect_err("ownership conflict")
            .contains("another exploration"));
        assert_eq!(before, scene.encode_full().expect("unchanged"));
        second.scene_ids = vec![inside];
        assert!(define_explanation_flow(&mut scene, &second, &Author::Agent).is_err());
        second.scene_ids = vec!["missing".to_string()];
        assert!(define_explanation_flow(&mut scene, &second, &Author::Agent).is_err());
        first.beats[0].cue = "A revised explanation.".to_string();
        define_explanation_flow(&mut scene, &first, &Author::Agent).expect("replace first only");
        let atlas = read(&scene).expect("read revised rooms");
        assert_eq!(atlas.explanations.len(), 2);
        assert_eq!(
            atlas
                .explanations
                .iter()
                .find(|flow| flow.definition.id == second.id)
                .expect("second preserved")
                .state
                .revision,
            1
        );
    }

    #[test]
    fn explanation_path_trace_requires_a_connector_and_preserves_marks_on_revision() {
        let mut scene = Scene::new();
        let (inside, beyond) = explanation_targets(&mut scene);
        let connector = place_shape(
            &mut scene,
            &ShapePatch {
                form: Some("arrow".to_string()),
                from: Some(inside.clone()),
                to: Some(beyond.clone()),
                ..ShapePatch::default()
            },
            &Author::Agent,
        )
        .expect("connector");
        let mut definition = explanation_fixture(&inside, &beyond);
        definition.beats[0].actions[0].kind = "trace-path".to_string();
        let before = scene.encode_full().expect("before refusal");
        assert!(
            define_explanation_flow(&mut scene, &definition, &Author::Agent)
                .expect_err("nodes cannot be traced")
                .contains("arrow or line")
        );
        assert_eq!(before, scene.encode_full().expect("atomic refusal"));
        definition.beats[0].actions[0].target_id = connector.clone();
        define_explanation_flow(&mut scene, &definition, &Author::Agent).expect("trace flow");
        let question = mark(
            &mut scene,
            &connector,
            "!",
            "Explain the return path",
            &Author::Human,
        )
        .expect("visual challenge");
        definition.beats[0].cue = "The return path also merges a shared update.".to_string();
        define_explanation_flow(&mut scene, &definition, &Author::Agent).expect("revision");
        let before_stale = scene.encode_full().expect("before stale control");
        assert!(
            control_explanation_flow(&scene, &definition.id, 1, "pause", &Author::Human).is_err()
        );
        assert_eq!(
            scene.encode_full().expect("after stale control"),
            before_stale
        );
        let projected = read(&scene).expect("read revision");
        assert_eq!(
            projected.explanation.as_ref().expect("flow").state.revision,
            2
        );
        assert!(projected
            .marks
            .iter()
            .any(|mark| mark.id == question && mark.target == connector));
        assert!(projected.describe().contains("trace-path"));
    }

    #[test]
    fn one_generic_explanation_flow_advances_with_utterance_and_selection_evidence() {
        let mut scene = Scene::new();
        let (inside, beyond) = explanation_targets(&mut scene);
        let definition = explanation_fixture(&inside, &beyond);
        let flow = define_explanation_flow(&mut scene, &definition, &Author::Agent)
            .expect("define explanation");
        assert_eq!(flow.state.current_beat, "observe");
        assert_eq!(flow.state.status, "active");

        let state = advance_explanation_flow(
            &scene,
            "distance-check",
            1,
            "inside",
            "yes",
            std::slice::from_ref(&inside),
            &Author::Human,
        )
        .expect("pointed affirmative transition");
        assert_eq!(state.current_beat, "contrast");
        assert_eq!(state.status, "completed");
        assert_eq!(state.last_response, "yes");
        assert_eq!(state.last_selected_target_ids, vec![inside]);

        let projected = read(&scene).expect("read advanced flow");
        assert_eq!(
            projected.explanation,
            Some(ExplanationFlow { state, ..flow })
        );
        let description = projected.describe();
        assert!(description.contains("EXPLANATION FLOW"), "{description}");
        assert!(description.contains("response=\"yes\""), "{description}");
    }

    #[test]
    fn invalid_explanation_graphs_and_stale_transitions_are_atomic() {
        let mut scene = Scene::new();
        let (inside, beyond) = explanation_targets(&mut scene);
        let mut invalid_control = explanation_fixture(&inside, &beyond);
        invalid_control.beats[0].advance.transitions[0].control = Some("link".to_string());
        let error = define_explanation_flow(&mut scene, &invalid_control, &Author::Agent)
            .expect_err("unknown learner control refusal");
        assert!(
            error.contains("explanation transition control") && error.contains("button"),
            "{error}"
        );
        assert!(read(&scene)
            .expect("read after control refusal")
            .explanation
            .is_none());

        let mut missing = explanation_fixture(&inside, &beyond);
        missing.beats[0].evidence[0].target_id = "not-on-page".to_string();
        let before = scene.encode_full().expect("state before missing target");
        let error = define_explanation_flow(&mut scene, &missing, &Author::Agent)
            .expect_err("missing target refusal");
        assert!(error.contains("is not on the atlas"), "{error}");
        assert_eq!(scene.encode_full().expect("state after refusal"), before);

        let mut cyclic = explanation_fixture(&inside, &beyond);
        cyclic.beats[1].advance = ExplanationAdvance {
            mode: "continue".to_string(),
            prompt: "Again".to_string(),
            transitions: vec![ExplanationTransition {
                id: "again".to_string(),
                label: "Again".to_string(),
                control: None,
                next: Some("observe".to_string()),
                target_ids: Vec::new(),
                phrases: Vec::new(),
            }],
        };
        let error = define_explanation_flow(&mut scene, &cyclic, &Author::Agent)
            .expect_err("cycle refusal");
        assert!(error.contains("must be finite"), "{error}");
        assert!(read(&scene)
            .expect("read after graph refusals")
            .explanation
            .is_none());

        let definition = explanation_fixture(&inside, &beyond);
        define_explanation_flow(&mut scene, &definition, &Author::Agent).expect("valid definition");
        let before = scene.encode_full().expect("state before stale revision");
        let error = advance_explanation_flow(
            &scene,
            "distance-check",
            9,
            "inside",
            "yes",
            std::slice::from_ref(&inside),
            &Author::Human,
        )
        .expect_err("stale revision refusal");
        assert!(error.contains("expected 9, current is 1"), "{error}");
        assert_eq!(
            scene.encode_full().expect("state after stale refusal"),
            before
        );
    }

    #[test]
    fn explanation_control_and_replica_restore_preserve_one_coherent_cursor() {
        let mut scene = Scene::new();
        let (inside, beyond) = explanation_targets(&mut scene);
        let definition = explanation_fixture(&inside, &beyond);
        define_explanation_flow(&mut scene, &definition, &Author::Agent)
            .expect("define explanation");
        let paused = control_explanation_flow(&scene, "distance-check", 1, "pause", &Author::Human)
            .expect("pause");
        assert_eq!(paused.status, "paused");
        assert_eq!(paused.entered_revision, 1);
        let resumed =
            control_explanation_flow(&scene, "distance-check", 2, "resume", &Author::Human)
                .expect("resume");
        assert_eq!(resumed.status, "active");
        assert_eq!(resumed.entered_revision, 1);
        let restarted =
            control_explanation_flow(&scene, "distance-check", 3, "restart", &Author::Human)
                .expect("restart");
        assert_eq!(restarted.current_beat, "observe");
        assert_eq!(restarted.entered_revision, 4);

        let replica =
            Scene::from_state(&scene.encode_full().expect("encode flow")).expect("restore replica");
        let restored = read(&replica)
            .expect("read replica")
            .explanation
            .expect("restored explanation");
        assert_eq!(restored.definition, definition);
        assert_eq!(restored.state, restarted);
    }
}
