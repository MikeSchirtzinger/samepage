//! The human's replica.
//!
//! This is a real CRDT peer, not a thin view: the page owns its own yrs `Doc`,
//! merges concurrent host updates locally, and speaks the same y-sync protocol
//! over the runtime's `/ws` that the host speaks. The human's drags are
//! therefore *edits to the shared document*, not requests for the server to
//! edit it, which is what makes "both of us can modify it" true rather than
//! aspirational.
//!
//! Everything about what an atlas object MEANS lives in `same-page-atlas-core`
//! and is compiled into both this wasm module and the native host, so neither
//! side can drift into its own private shape of the page.

#[cfg(any(test, target_arch = "wasm32"))]
mod trace;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use ag_ui_canvas::codec::{decode_frame, encode_sync, Frame};
use ag_ui_canvas::scene::{Author, Scene};
use ag_ui_canvas::sync::{greeting, handle_payload, update_message};
use same_page_atlas_core as atlas;
use same_page_atlas_core::anchor;
use same_page_atlas_core::camera;
use same_page_atlas_core::labels;
use same_page_atlas_core::panels;
use wasm_bindgen::prelude::*;

/// Install the one composed browser subscriber before JavaScript receives any
/// exports. The product-owned layer retains typed records. Diagnostic builds
/// may independently mirror the same spans into Chrome User Timing.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    trace::install().map_err(err)
}

/// Read the browser-local trace ring without contacting the host.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn trace_snapshot() -> Result<String, JsValue> {
    trace::snapshot_json().map_err(err)
}

/// Measure one synchronous renderer frame, retaining nested WASM operations.
/// The callback never escapes this scope, so unrelated async work cannot inherit it.
#[wasm_bindgen]
pub fn trace_browser_frame(callback: &js_sys::Function) -> Result<JsValue, JsValue> {
    let span = tracing::info_span!("atlas.render", source = "browser_renderer");
    let _entered = span.enter();
    callback.call0(&JsValue::NULL)
}

/// Clear the browser-local trace ring and its overflow counter.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn trace_clear() {
    trace::clear();
}

/// Measure the bounded trace machinery in the actual browser WASM module.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn trace_benchmark(iterations: u32, samples: u32) -> Result<String, JsValue> {
    trace::benchmark_json(iterations, samples).map_err(err)
}

/// Document schema compiled into this browser replica.
#[wasm_bindgen]
pub fn doc_schema_version() -> u32 {
    atlas::DOC_SCHEMA_VERSION
}

#[wasm_bindgen]
pub fn segment_max_depth() -> u32 {
    atlas::SEGMENT_MAX_DEPTH as u32
}

/// Build the ephemeral viewport report sent to `/atlas/attention`.
///
/// Camera state is not CRDT state. Keeping this encoder in the browser replica
/// still gives the page one validated vocabulary for world bounds, zoom, and
/// whether its active chip came from selection or the composer.
#[wasm_bindgen]
#[allow(
    clippy::too_many_arguments,
    reason = "the flat WASM boundary mirrors the browser attention payload"
)]
pub fn attention_payload(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    zoom: f64,
    dragging: Option<String>,
    camera: &str,
    source: &str,
) -> Result<String, JsValue> {
    encode_attention_payload(x, y, w, h, zoom, dragging, camera, source).map_err(err)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the encoder preserves the flat WASM boundary without a second payload shape"
)]
fn encode_attention_payload(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    zoom: f64,
    dragging: Option<String>,
    camera: &str,
    source: &str,
) -> Result<String, String> {
    let viewport = atlas::ViewportRect::new(x, y, w, h)?;
    if !zoom.is_finite() || zoom <= 0.0 {
        return Err("attention zoom must be a positive number".to_string());
    }
    if !matches!(camera, "follow" | "ask" | "hold") {
        return Err("attention camera must be follow, ask, or hold".to_string());
    }
    if !matches!(source, "selection" | "composer") {
        return Err("attention source must be selection or composer".to_string());
    }
    if dragging
        .as_ref()
        .is_some_and(|id| id.trim().is_empty() || id.chars().count() > 160)
    {
        return Err("attention dragging must be a non-empty object id".to_string());
    }
    serde_json::to_string(&serde_json::json!({
        "viewport": {
            "x": viewport.x,
            "y": viewport.y,
            "w": viewport.w,
            "h": viewport.h,
        },
        "zoom": zoom,
        "dragging": dragging,
        "camera": camera,
        "source": source,
    }))
    .map_err(|error| error.to_string())
}

fn agent_ink_step_json(scene: &Scene) -> Result<String, String> {
    let page = atlas::read(scene)?;
    let index_variable = atlas::agent_ink::step_index_variable(&page)?;
    let index = index_variable.and_then(|variable| {
        let value = variable.value;
        (value.is_finite() && value >= 0.0 && value.fract() == 0.0).then_some(value as u32)
    });
    // Stored steps are recorded in order starting at 0 (`advance_step`
    // only ever moves one past the last recorded step), so the first gap
    // is the end of the timeline. There is no public API to list stored
    // indices directly; probing is bounded by `MAX_STORED_STEPS`.
    let mut count = 0u32;
    while count < atlas::agent_ink::MAX_STORED_STEPS
        && atlas::agent_ink::stored_step(scene, count).is_ok()
    {
        count += 1;
    }
    serde_json::to_string(&serde_json::json!({
        "index_variable": index_variable.map(|variable| variable.name.clone()),
        "index": index,
        "count": count,
    }))
    .map_err(|error| error.to_string())
}

fn set_agent_ink_variable_and_solve(
    scene: &mut Scene,
    name: &str,
    value: f64,
) -> Result<String, String> {
    atlas::agent_ink::set_variable_value(scene, name, value, &Author::Human)?;
    let solve = atlas::agent_ink::solve(scene, &Author::Human)?;
    serde_json::to_string(&solve).map_err(|error| error.to_string())
}

/// Wire frames produced by local edits, drained by the JS transport.
type Outbox = Arc<Mutex<Vec<Vec<u8>>>>;

/// Paint-only identifiers resolved from the shared core's canonical
/// `CURRENT STATE` projection. The browser renderer receives ids, not the
/// variable or ordering inputs from which it could invent a second state
/// machine.
#[derive(serde::Serialize)]
struct StateMachinePaint {
    machine_id: String,
    state_id: Option<String>,
    transition_id: Option<String>,
    unresolved: bool,
    read_back: String,
}

fn uniquely_matching_state<'a>(
    states: &[&'a atlas::Node],
    text: &str,
    accepted_tail: impl Fn(&str) -> bool,
) -> Option<(&'a atlas::Node, String)> {
    let mut matches = states.iter().filter_map(|state| {
        let quoted = format!("{:?}", state.label);
        text.strip_prefix(&quoted)
            .filter(|tail| accepted_tail(tail))
            .map(|tail| (*state, tail.to_string()))
    });
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

fn state_machine_paint(page: &atlas::Atlas) -> Vec<StateMachinePaint> {
    let digest = page.digest();
    digest
        .nodes
        .into_iter()
        .filter(|machine| {
            machine.diagram_kind == "state_machine" && !machine.state_read_back.is_empty()
        })
        .map(|machine| {
            let states = page.children_of(&machine.id);
            let projected = machine
                .state_read_back
                .strip_prefix("CURRENT STATE ")
                .filter(|rest| !rest.starts_with("unresolved:"));
            let current = projected.and_then(|rest| {
                uniquely_matching_state(&states, rest, |tail| {
                    tail.is_empty() || tail.starts_with(" (was ")
                })
                .map(|(state, _)| state)
            });
            let transition = current.and_then(|current| {
                let current_token = format!("CURRENT STATE {:?}", current.label);
                let history = machine
                    .state_read_back
                    .strip_prefix(&current_token)?
                    .strip_prefix(" (was ")?;
                let (previous, tail) = uniquely_matching_state(&states, history, |tail| {
                    tail == ")" || tail.starts_with(", ")
                })?;
                let connecting = page
                    .edges
                    .iter()
                    .filter(|edge| {
                        !edge.event.is_empty() && edge.from == previous.id && edge.to == current.id
                    })
                    .collect::<Vec<_>>();
                let [edge] = connecting.as_slice() else {
                    return None;
                };
                (tail == format!(", via {})", edge.event)).then_some(*edge)
            });
            StateMachinePaint {
                machine_id: machine.id,
                state_id: current.map(|state| state.id.clone()),
                transition_id: transition.map(|edge| edge.id.clone()),
                unresolved: machine
                    .state_read_back
                    .starts_with("CURRENT STATE unresolved:")
                    || current.is_none(),
                read_back: machine.state_read_back,
            }
        })
        .collect()
}

#[wasm_bindgen]
pub struct AtlasDoc {
    scene: Scene,
    outbox: Outbox,
    /// Set while a remote frame is being integrated. Without it the update
    /// observer would push the host's own update straight back at the host;
    /// yrs would dedupe it, but the socket would carry a pointless echo of
    /// every remote edit.
    integrating: Arc<AtomicBool>,
    /// Any committed transaction (local or remote) marks the projection stale
    /// so the renderer redraws exactly once per frame instead of per edit.
    dirty: Arc<AtomicBool>,
    _updates: yrs::Subscription,
}

#[wasm_bindgen]
impl AtlasDoc {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<AtlasDoc, JsValue> {
        console_error_panic_hook::set_once();
        let scene = Scene::new();
        let outbox: Outbox = Arc::new(Mutex::new(Vec::new()));
        let integrating = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(true));

        let updates = {
            let outbox = outbox.clone();
            let integrating = integrating.clone();
            let dirty = dirty.clone();
            scene
                .on_update(move |update| {
                    dirty.store(true, Ordering::Release);
                    if integrating.load(Ordering::Acquire) {
                        return;
                    }
                    outbox
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(encode_sync(&update_message(update)));
                })
                .map_err(err)?
        };

        Ok(AtlasDoc {
            scene,
            outbox,
            integrating,
            dirty,
            _updates: updates,
        })
    }

    /// First frame to put on a freshly opened socket: our state vector, so the
    /// host replies with exactly what this replica is missing.
    pub fn hello(&self) -> Result<Vec<u8>, JsValue> {
        Ok(encode_sync(&greeting(&self.scene).map_err(err)?))
    }

    /// Integrate one inbound binary frame; returns reply frames to send.
    pub fn receive(&self, frame: &[u8]) -> Result<js_sys::Array, JsValue> {
        let span = tracing::info_span!("atlas.receive", source = "sync_transport");
        let _entered = span.enter();
        let replies = match decode_frame(frame).map_err(err)? {
            Frame::Sync(payload) => {
                self.integrating.store(true, Ordering::Release);
                let result = handle_payload(&self.scene, payload);
                self.integrating.store(false, Ordering::Release);
                result.map_err(err)?
            }
            // The atlas has no bulk-geometry channel; ignore rather than fail,
            // so an unrelated frame on a shared transport cannot kill the page.
            _ => Vec::new(),
        };
        let out = js_sys::Array::new();
        for reply in replies {
            out.push(&js_sys::Uint8Array::from(encode_sync(&reply).as_slice()).into());
        }
        Ok(out)
    }

    /// Frames this replica owes the host after local edits.
    pub fn take_outbound(&self) -> js_sys::Array {
        let span = tracing::info_span!("atlas.take_outbound", source = "sync_transport");
        let _entered = span.enter();
        let out = js_sys::Array::new();
        let mut outbox = self
            .outbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for frame in outbox.drain(..) {
            out.push(&js_sys::Uint8Array::from(frame.as_slice()).into());
        }
        out
    }

    /// True at most once per batch of edits; clears the flag.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// The whole shared page as JSON, for the renderer.
    pub fn snapshot(&self) -> Result<String, JsValue> {
        let snapshot_span = tracing::info_span!("atlas.snapshot", operation = "snapshot");
        let _snapshot_entered = snapshot_span.enter();
        let read_span = tracing::info_span!("atlas.read", source = "browser_crdt");
        let atlas = {
            let _read_entered = read_span.enter();
            let atlas = atlas::read(&self.scene).map_err(err)?;
            tracing::info!(
                event = "atlas.snapshot.ready",
                node_count = atlas.nodes.len() as u64,
                valid = true,
                surface = "same-page-atlas"
            );
            atlas
        };
        serde_json::to_string(&atlas).map_err(|error| err(error.to_string()))
    }

    /// Revision of this actual browser replica, captured alongside its pixels.
    pub fn document_revision(&self) -> Result<Vec<u8>, JsValue> {
        self.scene
            .state_vector_v1()
            .map_err(|error| err(error.to_string()))
    }

    /// The same read-back string the agent sees. Exposed so the human can
    /// check, in one click, what the agent is actually looking at.
    pub fn describe(&self) -> Result<String, JsValue> {
        let span = tracing::info_span!("atlas.describe", source = "browser_crdt");
        let _entered = span.enter();
        Ok(atlas::read(&self.scene).map_err(err)?.describe())
    }

    /// Current state ids and an optional uniquely named transition id for the
    /// renderer. Every item starts from the same canonical `CURRENT STATE`
    /// sentence carried by `Atlas::digest`; this adapter only resolves that
    /// sentence back to paintable ids and never writes to the CRDT.
    pub fn state_machine_paint(&self) -> Result<String, JsValue> {
        let page = atlas::read(&self.scene).map_err(err)?;
        serde_json::to_string(&state_machine_paint(&page)).map_err(|error| err(error.to_string()))
    }

    /// The style register one node actually chose, as the read-back's own
    /// words, as a JSON array of strings.
    ///
    /// The same filter `atlas_read` applies (`Node::style_words`), so a card
    /// that chose nothing shows nothing and the page and the agent describe a
    /// card identically. The inspector had a fixed six-row schema that
    /// predates the register: the agent could publish a taxonomy, the page
    /// could render it, and the page could not tell the human what any of it
    /// meant, so the claim was auditable only through the agent's sentence.
    /// Reimplementing the default-filter in JavaScript would be a second
    /// answer to the same question, which is how the two drift.
    pub fn node_style_words(&self, id: &str) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        let words = atlas
            .node(id)
            .map(atlas::Node::style_words)
            .unwrap_or_default();
        serde_json::to_string(&words).map_err(|error| err(error.to_string()))
    }

    /// The page reduced to what a later comparison needs, as JSON.
    ///
    /// The agent has been carrying one of these between turns since the change
    /// section existed. The human had no equivalent, which is why the canvas
    /// could tell the agent "the human moved this" and could not tell the
    /// human anything at all about what the agent had done. Same structure,
    /// same comparison, both directions.
    pub fn digest(&self) -> Result<String, JsValue> {
        let digest = atlas::read(&self.scene).map_err(err)?.digest();
        serde_json::to_string(&digest).map_err(|error| err(error.to_string()))
    }

    pub fn architecture_review(&self) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        Ok(atlas::architecture::inspect(&atlas)
            .map_err(err)?
            .to_string())
    }

    pub fn propose_architecture(&mut self, input: &str) -> Result<String, JsValue> {
        let input =
            serde_json::from_str(input).map_err(|e| err(format!("invalid proposal: {e}")))?;
        atlas::architecture::propose(&mut self.scene, &input, &Author::Human).map_err(err)
    }

    /// What changed against a digest taken earlier, as a JSON array of the
    /// same sentences the agent is given.
    ///
    /// `reader` is whose own writes to discount, so the human is not shown
    /// their own edits as news — the identical courtesy the agent gets.
    pub fn changes_since(&self, before: &str, reader: &str) -> Result<String, JsValue> {
        let before: atlas::Digest =
            serde_json::from_str(before).map_err(|error| err(error.to_string()))?;
        let after = atlas::read(&self.scene).map_err(err)?.digest();
        let lines = atlas::changes_for(&before, &after, reader);
        serde_json::to_string(&lines).map_err(|error| err(error.to_string()))
    }

    /// Advance one visible explanation transition as a human CRDT edit.
    /// The optimistic revision and coherent JSON state are validated by the
    /// same core function the host's agent action calls.
    pub fn advance_explanation(
        &mut self,
        flow_id: &str,
        expected_revision: u32,
        transition_id: &str,
        response: &str,
        selected_target_ids: &str,
    ) -> Result<String, JsValue> {
        let span = tracing::info_span!("atlas.advance_explanation", source = "human_edit");
        let _entered = span.enter();
        let selected_target_ids: Vec<String> = serde_json::from_str(selected_target_ids)
            .map_err(|error| err(format!("invalid selected target ids: {error}")))?;
        let state = atlas::advance_explanation_flow(
            &self.scene,
            flow_id,
            expected_revision,
            transition_id,
            response,
            &selected_target_ids,
            &Author::Human,
        )
        .map_err(err)?;
        serde_json::to_string(&state).map_err(|error| err(error.to_string()))
    }

    /// Pause, resume, restart, or stop the explanation as a human CRDT edit.
    pub fn control_explanation(
        &mut self,
        flow_id: &str,
        expected_revision: u32,
        command: &str,
    ) -> Result<String, JsValue> {
        let span = tracing::info_span!("atlas.control_explanation", source = "human_edit");
        let _entered = span.enter();
        let state = atlas::control_explanation_flow(
            &self.scene,
            flow_id,
            expected_revision,
            command,
            &Author::Human,
        )
        .map_err(err)?;
        serde_json::to_string(&state).map_err(|error| err(error.to_string()))
    }

    /// Create or update a node. `patch` is a JSON `NodePatch`.
    pub fn place_node(&mut self, patch: &str) -> Result<String, JsValue> {
        let patch: atlas::NodePatch =
            serde_json::from_str(patch).map_err(|error| err(error.to_string()))?;
        atlas::place_node(&mut self.scene, &patch, &Author::Human).map_err(err)
    }

    pub fn place_exploration_node(
        &mut self,
        flow_id: &str,
        patch: &str,
    ) -> Result<String, JsValue> {
        let patch: atlas::NodePatch =
            serde_json::from_str(patch).map_err(|error| err(error.to_string()))?;
        atlas::place_exploration_node(&mut self.scene, flow_id, &patch, &Author::Human).map_err(err)
    }

    /// Mark an existing container as a hierarchy or state machine through the
    /// same core mutation used by the host replica.
    pub fn set_diagram_kind(&self, id: &str, kind: &str) -> Result<(), JsValue> {
        atlas::set_node_diagram_kind(&self.scene, id, kind, &Author::Human).map_err(err)
    }

    /// Set the two independent state roles on a state-machine node.
    pub fn set_state_roles(&self, id: &str, initial: bool, terminal: bool) -> Result<(), JsValue> {
        atlas::set_state_roles(
            &self.scene,
            id,
            Some(initial),
            Some(terminal),
            &Author::Human,
        )
        .map_err(err)
    }

    /// Create one state-machine transition through the shared core.
    pub fn transition(
        &mut self,
        from: &str,
        to: &str,
        event: &str,
        guard: Option<String>,
    ) -> Result<String, JsValue> {
        atlas::transition(
            &mut self.scene,
            from,
            to,
            event,
            guard.as_deref(),
            &Author::Human,
        )
        .map_err(err)
    }

    pub fn link(&mut self, from: &str, to: &str, label: &str) -> Result<String, JsValue> {
        atlas::link(&mut self.scene, from, to, label, &Author::Human).map_err(err)
    }

    pub fn save_decision_draft(&mut self, definition: &str) -> Result<String, JsValue> {
        let input: atlas::decision::DraftInput =
            serde_json::from_str(definition).map_err(|error| err(error.to_string()))?;
        atlas::decision::save(&mut self.scene, &input, &Author::Human).map_err(err)
    }

    pub fn set_context(&mut self, definition: &str) -> Result<String, JsValue> {
        let input: atlas::context::ContextInput =
            serde_json::from_str(definition).map_err(|error| err(error.to_string()))?;
        atlas::context::set(&mut self.scene, &input, &Author::Human).map_err(err)
    }

    pub fn mark(&mut self, target: &str, glyph: &str, text: &str) -> Result<String, JsValue> {
        let span = tracing::info_span!("atlas.mark", source = "human_edit");
        let _entered = span.enter();
        atlas::mark(&mut self.scene, target, glyph, text, &Author::Human).map_err(err)
    }

    /// Open a challenge on a node: the `?` mark with the fixed text from
    /// `docs/design-challenge-v0.md`, so the agent answers with claims.
    pub fn challenge(&mut self, node_id: &str) -> Result<String, JsValue> {
        atlas::mark(
            &mut self.scene,
            node_id,
            "?",
            CHALLENGE_TEXT,
            &Author::Human,
        )
        .map_err(err)
    }

    /// The human's verdict on one claim: `accepted` or `rejected`.
    pub fn claim_verdict(&self, id: &str, verdict: &str) -> Result<(), JsValue> {
        atlas::claims::claim_verdict(&self.scene, id, verdict, &Author::Human).map_err(err)
    }

    /// Create or update a shape. `patch` is a JSON `ShapePatch`.
    pub fn place_exploration_shape(
        &mut self,
        flow_id: &str,
        patch: &str,
    ) -> Result<String, JsValue> {
        let patch: atlas::ShapePatch =
            serde_json::from_str(patch).map_err(|error| err(error.to_string()))?;
        atlas::place_exploration_shape(&mut self.scene, flow_id, &patch, &Author::Human)
            .map_err(err)
    }

    /// Create or update a shape. `patch` is a JSON `ShapePatch`.
    pub fn place_shape(&mut self, patch: &str) -> Result<String, JsValue> {
        let patch: atlas::ShapePatch =
            serde_json::from_str(patch).map_err(|error| err(error.to_string()))?;
        atlas::place_shape(&mut self.scene, &patch, &Author::Human).map_err(err)
    }

    /// Mint one human-selected Atlas object and attach the real Worker mask in
    /// one browser gesture. The shared core removes the proposal if mask
    /// validation fails, so the CRDT never keeps a half-created object.
    pub fn accept_segment(&mut self, proposal: &str, result: &str) -> Result<String, JsValue> {
        let proposal: atlas::SegmentProposal =
            serde_json::from_str(proposal).map_err(|error| err(error.to_string()))?;
        let mut result: atlas::SegmentMaterialization =
            serde_json::from_str(result).map_err(|error| err(error.to_string()))?;
        atlas::accept_human_segment(&mut self.scene, &proposal, &mut result, &Author::Human)
            .map_err(err)
    }

    /// Attach real browser-worker MobileSAM output to a host-minted segment.
    /// The shared core validates the exact generation, model pins, encoding,
    /// and RLE pixel count. This derived write deliberately carries no human
    /// or agent author because neither seat authored the model's mask.
    pub fn materialize_segment(&mut self, id: &str, result: &str) -> Result<(), JsValue> {
        let result: atlas::SegmentMaterialization =
            serde_json::from_str(result).map_err(|error| err(error.to_string()))?;
        atlas::materialize_segment(&mut self.scene, id, &result).map_err(err)
    }

    /// Write the human slider's normalized wing position into the shared
    /// document. Rendering derives mirrored angles from this one scalar.
    pub fn set_segment_flap(&mut self, id: &str, value: f64) -> Result<(), JsValue> {
        atlas::set_segment_flap(&mut self.scene, id, value, &Author::Human).map_err(err)
    }

    /// Set or clear the selected object's animation as a human CRDT write.
    pub fn set_segment_animation(&mut self, id: &str, animation: &str) -> Result<(), JsValue> {
        atlas::set_segment_animation(&mut self.scene, id, animation, &Author::Human).map_err(err)
    }

    /// Set or clear an expressive keyframe program as one human CRDT write.
    /// The JSON string is `null` to stop motion or an
    /// `atlas-segment-motion-v1` program to start it.
    pub fn set_segment_motion(&mut self, id: &str, motion: &str) -> Result<(), JsValue> {
        let motion = if motion.trim() == "null" {
            None
        } else {
            Some(
                serde_json::from_str::<atlas::SegmentMotionProgram>(motion)
                    .map_err(|error| err(format!("invalid segment motion: {error}")))?,
            )
        };
        atlas::set_segment_motion(&mut self.scene, id, motion.as_ref(), &Author::Human)
            .map(|_| ())
            .map_err(err)
    }

    /// Nest an existing part under another part of the same root object while
    /// preserving its world position and mask identity.
    pub fn set_segment_parent(&mut self, id: &str, parent_id: &str) -> Result<(), JsValue> {
        atlas::set_segment_parent(&mut self.scene, id, parent_id, &Author::Human).map_err(err)
    }

    /// Set the selected segment's normalized 2D animation pivot.
    pub fn set_segment_pivot(&mut self, id: &str, x: f64, y: f64) -> Result<(), JsValue> {
        atlas::set_segment_pivot(&mut self.scene, id, x, y, &Author::Human).map_err(err)
    }

    /// The drawn layer resolved to what should actually be painted: outline
    /// polygons for ink, binding-resolved endpoints for arrows.
    ///
    /// The renderer computes none of this itself. An arrow's endpoints move
    /// with the node it is bound to and an ink stroke's edge is an outline
    /// rather than its centre line, deriving either one here, in JavaScript,
    /// would put a second opinion about the drawing's geometry on the page.
    pub fn painting(&self) -> Result<String, JsValue> {
        let span = tracing::info_span!("atlas.painting", source = "browser_geometry");
        let _entered = span.enter();
        let atlas = atlas::read(&self.scene).map_err(err)?;
        serde_json::to_string(&atlas.painting()).map_err(|error| err(error.to_string()))
    }

    /// Run the same deterministic page checks as the model's host.
    pub fn validation(&self) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        let report = atlas::validation::validate(&atlas).map_err(err)?;
        serde_json::to_string(&report).map_err(|error| err(error.to_string()))
    }

    /// Every agent-ink variable, as a JSON array of `{name, value, state,
    /// represents, unit}`.
    ///
    /// Serializes `Atlas::variables` directly. The panel does not recompute
    /// or reformat anything from these fields; it only displays them, so the
    /// human's numbers are always the same numbers the agent's read-back
    /// describes.
    pub fn agent_ink_variables(&self) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        serde_json::to_string(&atlas.variables).map_err(|error| err(error.to_string()))
    }

    /// Every agent-ink relation, as a JSON array of `{name, op, members, m,
    /// b}`.
    pub fn agent_ink_relations(&self) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        serde_json::to_string(&atlas.relations).map_err(|error| err(error.to_string()))
    }

    /// The document's agent-ink mode and, once promoted, its promotion
    /// record, as JSON `{mode, promotion}`. `promotion` is `null` for a
    /// document that has always been in its current mode.
    ///
    /// This is what lets the browser panel decide whether promotion is even
    /// on offer: a learning-mode document with steps shows the control, an
    /// already-alignment document does not, because calling it there would
    /// only ever come back "there is nothing to promote".
    pub fn agent_ink_mode(&self) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        serde_json::to_string(&serde_json::json!({
            "mode": atlas.agent_ink_mode.unwrap_or_default().as_str(),
            "promotion": atlas.agent_ink_promotion,
        }))
        .map_err(|error| err(error.to_string()))
    }

    /// The document's step timeline, as JSON `{index_variable, index,
    /// count}`.
    ///
    /// The step-index variable is whichever variable is in the `scrubbing`
    /// state: that state exists specifically to mark the one gesture-owned
    /// numeric value a human drags through a timeline rather than pins or
    /// leaves to the solver. A document with none has nothing to scrub, so
    /// `index_variable` and `index` come back `null` and `count` is `0`.
    pub fn agent_ink_step(&self) -> Result<String, JsValue> {
        agent_ink_step_json(&self.scene).map_err(err)
    }

    /// Restore a stored step by index, as a human edit.
    ///
    /// Goes through `agent_ink::scrub_step`, which replays persisted values
    /// byte for byte, never `agent_ink::solve`. Dragging the scrubber must
    /// reproduce exactly what was recorded at that index, not resolve a new
    /// answer for wherever the drag lands.
    pub fn agent_ink_scrub(&mut self, index: u32) -> Result<String, JsValue> {
        let atlas = atlas::read(&self.scene).map_err(err)?;
        let index_variable = atlas::agent_ink::step_index_variable(&atlas)
            .map_err(err)?
            .map(|variable| variable.name.clone())
            .ok_or_else(|| err("no step-index variable is in the scrubbing state"))?;
        let step =
            atlas::agent_ink::scrub_step(&mut self.scene, &index_variable, index, &Author::Human)
                .map_err(err)?;
        serde_json::to_string(&step).map_err(|error| err(error.to_string()))
    }

    /// Write a value onto a variable the human owns, then resolve everything
    /// that depends on it before the page redraws.
    ///
    /// `agent_ink::set_variable_value` rejects a free or derived variable
    /// before anything is written, the same ownership rule the agent's write
    /// path is held to. Returning the real solve state keeps a pinned-input
    /// edit from leaving solver-owned values and bound geometry stale.
    pub fn agent_ink_set_variable(&mut self, name: &str, value: f64) -> Result<String, JsValue> {
        set_agent_ink_variable_and_solve(&mut self.scene, name, value).map_err(err)
    }

    /// The human drag path: read a shape or node property the drag just
    /// wrote back into the agent-ink variable that represents it, then
    /// solve so anything that depends on that variable moves too.
    ///
    /// Returns JSON `{"variable": string|null, "solve": SolveState|null}`.
    /// `variable` is `null` when nothing represents `object.property`; a
    /// drag on an unbound shape has nothing to capture and both fields come
    /// back `null` with no solve run.
    ///
    /// An `Err` means the property is bound to a free or derived variable,
    /// which is solver-owned and must not be overwritten by a gesture. The
    /// drag's raw geometry write already landed through the ordinary
    /// `place_shape`/`place_node` call this follows, so the shape would
    /// otherwise be left sitting wherever the drag let go of it, disagreeing
    /// with the variable that actually owns its position. Running `solve`
    /// here even on this path re-derives every live-bound property from its
    /// (unchanged) variable value, which is what puts the shape back rather
    /// than leaving a human drag that silently did nothing.
    pub fn agent_ink_capture(
        &mut self,
        object: &str,
        property: &str,
        value: f64,
    ) -> Result<String, JsValue> {
        match atlas::agent_ink::capture_represented_property(
            &self.scene,
            object,
            property,
            value,
            &Author::Human,
        ) {
            Ok(variable) => {
                let solve = match &variable {
                    Some(_) => Some(
                        atlas::agent_ink::solve(&mut self.scene, &Author::Human).map_err(err)?,
                    ),
                    None => None,
                };
                serde_json::to_string(&serde_json::json!({
                    "variable": variable,
                    "solve": solve,
                }))
                .map_err(|error| err(error.to_string()))
            }
            Err(error) => {
                let _ = atlas::agent_ink::solve(&mut self.scene, &Author::Human);
                Err(err(error))
            }
        }
    }

    pub fn remove(&mut self, id: &str) -> Result<usize, JsValue> {
        atlas::remove(&self.scene, id).map_err(err)
    }

    /// Promote a drawn shape into a node, keeping its words, place, marks,
    /// and bindings. Returns the new node's id.
    ///
    /// The human's half of repairing the register split: ink that turned out
    /// to mean something gets standing in the register the agent reasons
    /// about, as their own edit through their own replica.
    pub fn lift_shape(&mut self, id: &str) -> Result<String, JsValue> {
        atlas::lift_shape(&mut self.scene, id, &Author::Human).map_err(err)
    }

    /// Report a card's rendered height back into the document.
    ///
    /// The one fact about the shared page that only the side doing layout can
    /// know. Without it the agent reasons about overlap from a guess, and the
    /// host, which has no DOM at all, has nothing better to offer it.
    pub fn measure(&mut self, id: &str, height: f64) -> Result<(), JsValue> {
        atlas::measure_node(&self.scene, id, height).map_err(err)
    }

    /// The same write for a `text` shape, whose height is however many lines
    /// its words wrap to, the one thing about it nobody authors.
    pub fn measure_shape(&mut self, id: &str, height: f64) -> Result<(), JsValue> {
        atlas::measure_shape(&self.scene, id, height).map_err(err)
    }

    /// The whole page as an `.excalidraw` document.
    pub fn export_excalidraw(&self) -> Result<String, JsValue> {
        Ok(atlas::excalidraw::to_excalidraw(
            &atlas::read(&self.scene).map_err(err)?,
        ))
    }

    /// Land an `.excalidraw` document on the atlas as HUMAN edits.
    ///
    /// Through the replica rather than through the host on purpose. An import
    /// is something a person did, and routing it via a server endpoint would
    /// make the one gesture in this surface that arrives as somebody else's
    /// write, the property the whole example exists to demonstrate, quietly
    /// broken by a convenience.
    ///
    /// Returns `{landed, skipped, notes}`; `skipped` names every element that
    /// could not come across and why, because a half-arrived document reported
    /// as a success is the failure mode this page is built to prevent. `notes`
    /// names what landed changed, such as an endpoint whose target did not
    /// import, which is a different sentence from "not imported".
    pub fn import_excalidraw(&mut self, document: &str) -> Result<String, JsValue> {
        let import = atlas::excalidraw::from_excalidraw(document).map_err(err)?;
        let landed =
            atlas::excalidraw::land_import(&mut self.scene, import, &Author::Human, 0.0, 0.0);
        serde_json::to_string(&serde_json::json!({
            "landed": landed.ids.len(),
            "skipped": landed.skipped,
            "notes": landed.notes,
            "bound_connectors": landed.bound_connectors,
        }))
        .map_err(|error| err(error.to_string()))
    }
}

/// The text a challenge mark carries. Kept verbatim with the contract so
/// the agent can recognise a challenge by its words.
pub const CHALLENGE_TEXT: &str = "challenge: lay out what you understand about this, one claim at a time, with the basis for each";

/// Exposed so the browser can recognise a challenge mark without a copy.
#[wasm_bindgen]
pub fn challenge_text() -> String {
    CHALLENGE_TEXT.to_string()
}

/// The closed lists and ranges a shape's look is authored from, so the
/// palette's width and dash controls are generated from the model rather
/// than kept as a second copy in JavaScript.
#[wasm_bindgen]
pub fn shape_vocabulary() -> String {
    serde_json::json!({
        "forms": atlas::FORMS,
        "stroke_styles": atlas::STROKE_STYLES,
        "roundness": atlas::ROUNDNESS,
        "stroke_width_range": atlas::STROKE_WIDTH_RANGE,
        "default_stroke_width": atlas::DEFAULT_STROKE_WIDTH,
        "opacity_range": atlas::OPACITY_RANGE,
        "font_size_range": atlas::FONT_SIZE_RANGE,
    })
    .to_string()
}

/// What each ink name actually looks like.
///
/// The renderer asks rather than keeping its own table: the palette also
/// decides what colour an exported `.excalidraw` comes out, and two copies of
/// it would eventually mean a drawing that changes colour on the way out.
#[wasm_bindgen]
pub fn ink_palette() -> String {
    serde_json::to_string(
        &atlas::INK_HEX
            .iter()
            .copied()
            .collect::<std::collections::BTreeMap<&str, &str>>(),
    )
    .unwrap_or_else(|_| "{}".to_string())
}

/// The outline of a stroke that has not been committed yet.
///
/// The in-progress stroke under the human's pointer has no object in the CRDT
///, it is not part of the shared document until they lift the pen, so it
/// cannot come back through [`AtlasDoc::painting`]. It still has to be drawn
/// by the same code, or the ink would visibly change shape at the moment it
/// lands, which reads as the surface disagreeing with itself.
#[wasm_bindgen]
pub fn ink_outline(points: &str) -> String {
    let parsed: Vec<(f64, f64)> = points
        .split_whitespace()
        .filter_map(|pair| {
            let (x, y) = pair.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        })
        .collect();
    let polygon = atlas::outline(&parsed, atlas::INK_SIZE);
    serde_json::to_string(&polygon).unwrap_or_else(|_| "[]".to_string())
}

/// The camera, owned by the core, driven from the page.
///
/// The page forwards pointer, wheel, and key events into this object and
/// writes back the strings it returns. It does not compute a transform, a
/// scale, a fit, or an anchored zoom: the host and the browser have to agree
/// about where things are, and two implementations of that are two opinions.
///
/// JSON in and JSON out, matching the rest of this file's convention.
#[wasm_bindgen]
pub struct AtlasCamera {
    camera: camera::Camera2d,
    approach: camera::Approach,
}

/// Options for an opening fit. Everything the caller measured about the pane
/// it is about to frame into, so the core is never guessing at chrome.
#[derive(serde::Deserialize)]
struct OpeningOptions {
    #[serde(default = "default_margin")]
    margin: f64,
    #[serde(default)]
    top_inset: f64,
    #[serde(default = "default_max_scale")]
    max_scale: f64,
    /// `None` means "frame the whole content, cluster rules do not apply":
    /// what a forced fit and the learner layout both want.
    #[serde(default)]
    readable_scale: Option<f64>,
    /// Set when the board holds one connected diagram, so a whole fit above
    /// this floor beats framing its densest fragment.
    #[serde(default)]
    cohesion_floor: Option<f64>,
    /// Boxes eligible to seed a cluster, when that is a subset of the content
    /// (drawn ink contributes to the extent but does not seed a cluster).
    #[serde(default)]
    cluster: Option<Vec<camera::WorldBox>>,
}

fn default_margin() -> f64 {
    40.0
}

fn default_max_scale() -> f64 {
    1.0
}

#[wasm_bindgen]
impl AtlasCamera {
    #[wasm_bindgen(constructor)]
    pub fn new() -> AtlasCamera {
        AtlasCamera {
            camera: camera::Camera2d::default(),
            approach: camera::Approach::default(),
        }
    }

    /// Pan by a screen-pixel delta, one to one at every zoom.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.camera.pan(dx, dy);
    }

    /// Zoom about a point in the pane, keeping the world under it fixed.
    pub fn zoom_at(&mut self, x: f64, y: f64, factor: f64) {
        self.camera.zoom_at(camera::Point { x, y }, factor);
    }

    /// Zoom about the middle of the pane. What the +/- buttons do.
    pub fn zoom_from_center(&mut self, width: f64, height: f64, factor: f64) {
        self.camera
            .zoom_from_center(camera::Viewport { width, height }, factor);
    }

    /// Where an untouched board should open. Returns `{camera, extent}` as
    /// JSON, or `null` when there is nothing to frame. It does NOT move the
    /// camera: the caller compares the extent against the one it already
    /// fitted, so a re-fit never fights a reveal.
    pub fn plan_opening(
        &self,
        boxes_json: &str,
        viewport_json: &str,
        options_json: &str,
    ) -> Result<Option<String>, JsValue> {
        let boxes: Vec<camera::WorldBox> = serde_json::from_str(boxes_json).map_err(err)?;
        let viewport: camera::Viewport = serde_json::from_str(viewport_json).map_err(err)?;
        let options: OpeningOptions = serde_json::from_str(options_json).map_err(err)?;
        let planned = match options.readable_scale {
            None => camera::fit(
                &boxes,
                viewport,
                options.margin,
                options.top_inset,
                options.max_scale,
            ),
            Some(readable) => camera::opening(
                &boxes,
                viewport,
                readable,
                options.margin,
                options.cluster.as_deref().unwrap_or(&boxes),
                options.cohesion_floor,
                options.max_scale,
            ),
        };
        planned
            .map(|fit| serde_json::to_string(&fit).map_err(err))
            .transpose()
    }

    /// Where the camera goes to make one target legible in the middle of the
    /// pane. `null` when the target has no geometry.
    pub fn plan_focus(
        &self,
        box_json: &str,
        viewport_json: &str,
    ) -> Result<Option<String>, JsValue> {
        let target: camera::WorldBox = serde_json::from_str(box_json).map_err(err)?;
        let viewport: camera::Viewport = serde_json::from_str(viewport_json).map_err(err)?;
        camera::focus(target, viewport)
            .map(|planned| serde_json::to_string(&planned).map_err(err))
            .transpose()
    }

    /// The scale at which a target is fully legible, before clamping. The
    /// reveal policy compares it against the current scale to decide whether
    /// moving the camera is warranted at all.
    pub fn readable_scale_for(&self, box_json: &str, viewport_json: &str) -> Result<f64, JsValue> {
        let target: camera::WorldBox = serde_json::from_str(box_json).map_err(err)?;
        let viewport: camera::Viewport = serde_json::from_str(viewport_json).map_err(err)?;
        Ok(camera::readable_scale_for(target, viewport))
    }

    /// Adopt a pose the core produced earlier (a plan, or a saved return
    /// point). The page never assembles one of these itself.
    pub fn adopt(&mut self, camera_json: &str) -> Result<(), JsValue> {
        let next: camera::Camera2d = serde_json::from_str(camera_json).map_err(err)?;
        self.camera = camera::Camera2d::new(next.x, next.y, next.scale);
        self.approach.cancel_and_resync(self.camera);
        Ok(())
    }

    /// Start an animated move toward a pose.
    pub fn approach(&mut self, camera_json: &str) -> Result<(), JsValue> {
        let target: camera::Camera2d = serde_json::from_str(camera_json).map_err(err)?;
        self.approach =
            camera::Approach::to(camera::Camera2d::new(target.x, target.y, target.scale));
        Ok(())
    }

    /// Advance an animated move by `dt` seconds and return the new transform.
    pub fn step(&mut self, dt: f64) -> String {
        self.camera = self.approach.step(self.camera, dt);
        self.camera.transform()
    }

    pub fn approaching(&self) -> bool {
        self.approach.active
    }

    /// Stop animating and adopt the pose on screen. Called on pointerdown,
    /// before any drag delta: without it the drag starts from the pose the
    /// animation was aiming at and the camera fights the hand.
    pub fn cancel_and_resync(&mut self) {
        self.approach.cancel_and_resync(self.camera);
    }

    /// The CSS transform the page writes onto the world layer.
    pub fn transform(&self) -> String {
        self.camera.transform()
    }

    /// `{x, y, scale}`. The page mirrors this for rendering that has to know
    /// the zoom (a grab handle's world size is the inverse of it), never to
    /// compute a new camera.
    pub fn state(&self) -> String {
        serde_json::to_string(&self.camera).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn scale(&self) -> f64 {
        self.camera.scale
    }

    /// A pane point in world coordinates.
    pub fn to_world(&self, x: f64, y: f64) -> String {
        serde_json::to_string(&self.camera.to_world(camera::Point { x, y }))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// A world point in pane coordinates.
    pub fn to_screen(&self, x: f64, y: f64) -> String {
        serde_json::to_string(&self.camera.to_screen(camera::Point { x, y }))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Where a set of registered world boxes lands on screen right now.
    ///
    /// `origin` is the pane's own top-left in client coordinates; `clips` are
    /// the clipping ancestors in the same coordinates. Every overlay anchored
    /// to the board goes through this, so pan, zoom, and resize all reproject
    /// by one code path and none of them can be the one that was forgotten.
    pub fn project_anchors(
        &self,
        origin_json: &str,
        anchors_json: &str,
        clips_json: &str,
    ) -> Result<String, JsValue> {
        let origin: camera::Point = serde_json::from_str(origin_json).map_err(err)?;
        let anchors: Vec<(String, camera::WorldBox)> =
            serde_json::from_str(anchors_json).map_err(err)?;
        let clips: Vec<anchor::ClipRect> = serde_json::from_str(clips_json).map_err(err)?;
        let placed = anchor::project_all(&self.camera, origin, &anchors, &clips);
        serde_json::to_string(&placed).map_err(err)
    }

    /// The whole board drawn small, with the pane's own rectangle on it.
    ///
    /// `null` when there is nothing to draw or no room to draw it in; the
    /// page then renders an empty frame rather than removing the control,
    /// because a minimap that disappears on an empty board is a control
    /// nobody can find when they have panned off into nothing.
    #[allow(
        clippy::too_many_arguments,
        reason = "the flat WASM boundary takes measurements the page made"
    )]
    pub fn minimap(
        &self,
        content_json: &str,
        pane_width: f64,
        pane_height: f64,
        width: f64,
        height: f64,
        margin: f64,
    ) -> Result<Option<String>, JsValue> {
        let content: camera::WorldBox = serde_json::from_str(content_json).map_err(err)?;
        let pane = camera::Viewport {
            width: pane_width,
            height: pane_height,
        };
        let Some(seen) = self.camera.world_viewport(pane) else {
            return Ok(None);
        };
        camera::minimap_view(content, seen, width, height, margin)
            .map(|view| serde_json::to_string(&view).map_err(err))
            .transpose()
    }

    /// Move the camera to the world point under a minimap pixel, keeping the
    /// zoom. What a click or a drag on the minimap asks for.
    pub fn center_on_minimap_point(
        &mut self,
        content_json: &str,
        view_json: &str,
        x: f64,
        y: f64,
        pane_width: f64,
        pane_height: f64,
    ) -> Result<bool, JsValue> {
        let content: camera::WorldBox = serde_json::from_str(content_json).map_err(err)?;
        let view: camera::MinimapView = serde_json::from_str(view_json).map_err(err)?;
        let point = camera::minimap_to_world(&view, content, x, y);
        let pane = camera::Viewport {
            width: pane_width,
            height: pane_height,
        };
        match camera::centered_on(point, pane, self.camera.scale) {
            Some(next) => {
                self.camera = next;
                self.approach.cancel_and_resync(self.camera);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The world rectangle the pane shows, or `null` for a pane with no size.
    pub fn world_viewport(&self, width: f64, height: f64) -> Option<String> {
        self.camera
            .world_viewport(camera::Viewport { width, height })
            .and_then(|seen| serde_json::to_string(&seen).ok())
    }
}

impl Default for AtlasCamera {
    fn default() -> Self {
        Self::new()
    }
}

/// Where every panel sits this frame.
///
/// `open` is the set of flyout ids the human has toggled on, browser-local
/// state the core never owns. `facts` is what the page measured about the
/// document and the pane. `focus` is the panel the human last reached for,
/// which outranks z order for its zone. The registry answers with a zone, a
/// slot, and a state per panel, and refuses at construction to give two
/// panels one zone.
#[wasm_bindgen]
pub fn panel_layout(
    open_json: &str,
    facts_json: &str,
    focus: Option<String>,
) -> Result<String, JsValue> {
    let open: Vec<String> = serde_json::from_str(open_json).map_err(err)?;
    let facts: panels::DocumentFacts = serde_json::from_str(facts_json).map_err(err)?;
    let layout = panels::PanelRegistry::atlas().layout_focused(&open, &facts, focus.as_deref());
    serde_json::to_string(&layout).map_err(err)
}

/// What a card shows at this zoom: `"title"`, `"body"`, or `"source"`.
///
/// The thresholds are legibility, not taste: below 0.45 a body line is under
/// six pixels tall on this renderer, which is texture rather than text, and a
/// board fitted at that scale rendered every card's title, body, and source
/// on top of the picture they were supposed to be annotating.
#[wasm_bindgen]
pub fn label_detail(scale: f64, selected: bool) -> String {
    match labels::detail_for(scale, selected) {
        labels::DetailLevel::Title => "title",
        labels::DetailLevel::Body => "body",
        labels::DetailLevel::Source => "source",
    }
    .to_string()
}

/// The full label pass: viewport cull, importance budget, and screen-space
/// de-clutter, plus the detail level per card.
#[wasm_bindgen]
pub fn label_plan(
    cards_json: &str,
    scale: f64,
    width: f64,
    height: f64,
    policy_json: Option<String>,
) -> Result<String, JsValue> {
    let cards: Vec<labels::LabelCandidate> = serde_json::from_str(cards_json).map_err(err)?;
    let policy: labels::LabelPolicy = match policy_json {
        Some(json) => serde_json::from_str(&json).map_err(err)?,
        None => labels::LabelPolicy::default(),
    };
    let planned = labels::plan(&cards, scale, width, height, &policy);
    serde_json::to_string(&planned).map_err(err)
}

fn err(error: impl ToString) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[wasm_bindgen]
impl AtlasDoc {
    /// One node's challenge standing, badge, and canonical read-back summary.
    pub fn node_challenge(&self, node_id: &str) -> Result<String, JsValue> {
        let challenge = atlas::read(&self.scene).map_err(err)?.challenge(node_id);
        serde_json::to_string(&challenge).map_err(|error| err(error.to_string()))
    }
}

#[wasm_bindgen]
impl AtlasDoc {
    /// One container's aggregate challenge standing over its whole subtree.
    pub fn challenge_within(&self, node_id: &str) -> Result<String, JsValue> {
        let challenge = atlas::read(&self.scene)
            .map_err(err)?
            .challenge_within(node_id);
        serde_json::to_string(&challenge).map_err(|error| err(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_variable_edit_resolves_dependent_values_before_returning() {
        let mut scene = Scene::new();
        atlas::agent_ink::create_variable(
            &mut scene,
            "input",
            3.0,
            "pinned",
            None,
            None,
            &Author::Human,
        )
        .expect("input variable");
        atlas::agent_ink::create_variable(
            &mut scene,
            "output",
            6.0,
            "free",
            None,
            None,
            &Author::Human,
        )
        .expect("output variable");
        atlas::agent_ink::create_relation(
            &mut scene,
            "output follows input",
            "linear",
            &["output", "input"],
            Some(2.0),
            Some(0.0),
            None,
            &Author::Human,
        )
        .expect("linear relation");

        let solve_json = set_agent_ink_variable_and_solve(&mut scene, "input", 7.0)
            .expect("human edit and solve");
        let solve: serde_json::Value = serde_json::from_str(&solve_json).expect("solve state JSON");
        assert_eq!(solve["status"], "converged");

        let atlas = atlas::read(&scene).expect("resolved document");
        assert_eq!(
            atlas
                .variables
                .iter()
                .find(|variable| variable.name == "input")
                .expect("input")
                .value,
            7.0
        );
        assert_eq!(
            atlas
                .variables
                .iter()
                .find(|variable| variable.name == "output")
                .expect("output")
                .value,
            14.0
        );
        assert!(
            atlas
                .variables
                .iter()
                .all(|variable| variable.touched_by == "human"),
            "the browser input and every dependent value must retain the human byline: {:#?}",
            atlas.variables
        );
    }

    #[test]
    fn the_browser_refuses_an_ambiguous_timeline_index() {
        let mut scene = Scene::new();
        for name in ["first step", "second step"] {
            atlas::agent_ink::create_variable(
                &mut scene,
                name,
                0.0,
                "scrubbing",
                None,
                None,
                &Author::Human,
            )
            .expect("scrubbing variable");
        }

        assert!(
            agent_ink_step_json(&scene).is_err(),
            "the browser must not silently choose one of two timeline indices"
        );
    }

    #[test]
    fn viewport_payload_carries_zoom_and_attention_source_without_document_state() {
        let payload =
            encode_attention_payload(50.0, 60.0, 400.0, 300.0, 1.5, None, "follow", "composer")
                .expect("attention payload");
        let payload: serde_json::Value = serde_json::from_str(&payload).expect("payload JSON");

        assert_eq!(payload["viewport"]["x"], 50.0);
        assert_eq!(payload["viewport"]["w"], 400.0);
        assert_eq!(payload["zoom"], 1.5);
        assert_eq!(payload["source"], "composer");
        assert!(payload["dragging"].is_null());
    }

    #[test]
    fn viewport_payload_rejects_unknown_attention_vocabulary() {
        let error = encode_attention_payload(0.0, 0.0, 100.0, 100.0, 1.0, None, "follow", "hover")
            .expect_err("unknown source");
        assert!(error.contains("selection or composer"), "{error}");
    }
}
