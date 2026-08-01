//! Semantic canvas events — the shared human↔agent vocabulary (notes §3).
//!
//! These ride the EXISTING JSON AG-UI channel as `CustomEvent`s; scene object
//! ids are the join key between this channel and the binary state channel.
//! Low volume by design: a drag emits ONE `canvas.dragged` at drag-end, while
//! the per-frame positions ride the CRDT channel.

use ag_ui_core::event::{BaseEvent, CustomEvent};
use serde_json::json;

pub const EV_SELECTED: &str = "canvas.selected";
pub const EV_DRAGGED: &str = "canvas.dragged";
pub const EV_DWELLED: &str = "canvas.dwelled";
pub const EV_CREATED: &str = "canvas.created";
pub const EV_DELETED: &str = "canvas.deleted";
pub const EV_FOCUS: &str = "canvas.focus";

fn custom(name: &str, value: serde_json::Value) -> CustomEvent {
    CustomEvent {
        base: BaseEvent::default(),
        name: name.to_string(),
        value,
    }
}

pub fn selected(object_id: &str, by: &str) -> CustomEvent {
    custom(EV_SELECTED, json!({ "id": object_id, "by": by }))
}

pub fn dragged(object_id: &str, from: (f64, f64), to: (f64, f64), by: &str) -> CustomEvent {
    custom(
        EV_DRAGGED,
        json!({
            "id": object_id,
            "from": [from.0, from.1],
            "to": [to.0, to.1],
            "by": by,
        }),
    )
}

pub fn dwelled(region: &str, ms: u64, by: &str) -> CustomEvent {
    custom(EV_DWELLED, json!({ "region": region, "ms": ms, "by": by }))
}

/// The learner is *attending to* an object — the join point for deixis ("this",
/// "that", "here"). Source-agnostic by design: `phase` says HOW attention was
/// expressed ("click" = explicit, "dwell" = a rested hover), so a pointer today
/// and a gaze/voice-referent tomorrow speak the same event. `world` is the
/// object's position in the agent's -10..10 plane.
pub fn focus(object_id: &str, kind: &str, world: (f64, f64), phase: &str, by: &str) -> CustomEvent {
    custom(
        EV_FOCUS,
        json!({
            "id": object_id,
            "kind": kind,
            "world": [world.0, world.1],
            "phase": phase,
            "by": by,
        }),
    )
}

pub fn created(object_id: &str, kind: &str, by: &str) -> CustomEvent {
    custom(
        EV_CREATED,
        json!({ "id": object_id, "kind": kind, "by": by }),
    )
}

pub fn deleted(object_id: &str, by: &str) -> CustomEvent {
    custom(EV_DELETED, json!({ "id": object_id, "by": by }))
}
