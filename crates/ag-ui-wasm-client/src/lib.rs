//! Browser AG-UI client. Two things live here:
//!
//! - [`Protocol`]: the typed protocol client the runtime's shared browser
//!   core (`/_agui/client.js`) drives. It owns one
//!   `ag_ui_core::assembly::Assembler`; every SSE `data:` payload goes through
//!   [`Protocol::push`], which parses it as `ag_ui_core::Event`, runs the
//!   lifecycle state machine, and hands JavaScript one frame: the validated
//!   event, the assembled transcript updates, and any anomalies it repaired.
//!   JavaScript never interprets an event type string again.
//! - [`subscribe_events`] / [`post_json`]: an `EventSource` + fetch transport
//!   for wasm apps that own their own page (the canvas web crate).
//!
//! ## JS surface
//!
//! ```js
//! import init, { Protocol } from '/_agui/protocol/ag_ui_wasm_client.js';
//! await init();
//! const protocol = new Protocol();
//! source.onmessage = (message) => {
//!   const { event, updates, anomalies } = protocol.push(message.data);
//!   for (const update of updates) render(update); // { kind: 'text-delta', ... }
//! };
//! ```

use ag_ui_core::assembly::{Anomaly, Assembler, PushError, Update};
use ag_ui_core::event::Event as AgUiEvent;
use ag_ui_core::JsonValue;
use js_sys::Function;
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{EventSource, MessageEvent, Request, RequestInit, Response};

// Install a friendlier panic message in the browser console on first load.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// What one pushed payload produced, in the shape JavaScript receives.
#[derive(Debug, Serialize)]
struct Frame<'a> {
    event: &'a AgUiEvent,
    updates: &'a [Update],
    #[serde(skip_serializing_if = "<[Anomaly]>::is_empty")]
    anomalies: &'a [Anomaly],
}

/// A refused payload, in the shape JavaScript receives as the thrown value.
/// `kind` is `"parse"` for a payload that is not a valid AG-UI event, or the
/// assembly error's own kebab-case kind (`"text-content-without-start"`).
#[derive(Debug, Serialize)]
struct Refusal {
    kind: String,
    message: String,
    #[serde(flatten)]
    detail: JsonValue,
}

impl From<PushError> for Refusal {
    fn from(error: PushError) -> Self {
        let message = error.to_string();
        match error {
            PushError::Parse(_) => Refusal {
                kind: "parse".to_string(),
                message,
                detail: JsonValue::Null,
            },
            PushError::Assembly(assembly) => {
                let mut detail = serde_json::to_value(&assembly).unwrap_or(JsonValue::Null);
                let kind = detail
                    .as_object_mut()
                    .and_then(|fields| fields.remove("kind"))
                    .and_then(|kind| kind.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "assembly".to_string());
                Refusal {
                    kind,
                    message,
                    detail,
                }
            }
        }
    }
}

/// The typed AG-UI protocol client for one event stream.
///
/// One `Protocol` per connection. Its assembler state survives an
/// `EventSource` reconnect on purpose: the runtime replays an in-flight
/// message as `TEXT_MESSAGE_START` + the whole text so far, and the
/// assembler resets that message when the replayed start arrives.
#[wasm_bindgen]
#[derive(Default)]
pub struct Protocol {
    assembler: Assembler,
}

#[wasm_bindgen]
impl Protocol {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Protocol {
        Protocol::default()
    }

    /// Parse one SSE `data:` payload and run the lifecycle state machine.
    ///
    /// Returns `{ event, updates, anomalies? }`. Throws `{ kind, message, ... }`
    /// when the payload is not a valid AG-UI event or when accepting it would
    /// lose content; the assembler is unchanged in that case and the caller
    /// should resync (reconnect and take the replay).
    pub fn push(&mut self, raw: &str) -> Result<JsValue, JsValue> {
        let (event, outcome) = self
            .assembler
            .push_json(raw)
            .map_err(|error| to_js(&Refusal::from(error)))?;
        let frame = Frame {
            event: &event,
            updates: &outcome.updates,
            anomalies: &outcome.anomalies,
        };
        to_js_result(&frame)
    }

    /// `{ events, updates, anomalies, errors }` since construction.
    pub fn counters(&self) -> Result<JsValue, JsValue> {
        to_js_result(&self.assembler.counters())
    }

    /// The run lifecycle as the assembler last saw it, e.g.
    /// `{ phase: "running", threadId, runId }`.
    pub fn run(&self) -> Result<JsValue, JsValue> {
        to_js_result(&self.assembler.run())
    }

    /// The accumulated text of a message that is still streaming, or
    /// `undefined` once it has finished or if it never started.
    pub fn text(&self, message_id: &str) -> Option<String> {
        let id = message_id.parse().ok()?;
        self.assembler.text(&id).map(str::to_owned)
    }

    /// Ids of the text messages currently streaming, oldest first.
    pub fn open_text_ids(&self) -> Vec<String> {
        self.assembler
            .open_text_ids()
            .map(ToString::to_string)
            .collect()
    }

    /// Forget every open item. Counters are kept.
    pub fn reset(&mut self) {
        self.assembler.reset();
    }
}

/// Serialize once in Rust and let the engine build the object: one serde
/// pass plus one `JSON.parse`, no per-field reflection calls.
fn to_js_result<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(value)
        .map_err(|error| JsValue::from_str(&format!("serialize frame: {error}")))?;
    js_sys::JSON::parse(&json)
}

/// Best effort conversion for a value that is itself an error: if it cannot
/// be turned into an object, the thrown value is its message string.
fn to_js(refusal: &Refusal) -> JsValue {
    to_js_result(refusal).unwrap_or_else(|_| JsValue::from_str(&refusal.message))
}

/// Handle returned from `subscribe_events`. Callers must retain it for as long
/// as the stream is needed; closing or dropping it clears the handler and
/// releases the Rust callback allocation.
#[wasm_bindgen]
pub struct Subscription {
    es: EventSource,
    on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
}

#[wasm_bindgen]
impl Subscription {
    /// Close the underlying EventSource. No further events will fire.
    pub fn close(&mut self) {
        self.es.set_onmessage(None);
        self.es.close();
        self.on_message.take();
    }

    /// Current readyState: 0 connecting, 1 open, 2 closed.
    #[wasm_bindgen(getter)]
    pub fn ready_state(&self) -> u16 {
        self.es.ready_state()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.es.set_onmessage(None);
        self.es.close();
    }
}

/// Subscribe to an SSE endpoint emitting AG-UI events as type-tagged JSON
/// (one `data: {...}` per message; no named SSE events). The callback is
/// invoked only for a valid, known `ag_ui_core::Event` converted to a JS
/// object. A malformed payload or callback exception closes the subscription;
/// continuing after a missed state transition would leave the client divergent.
///
/// `EventSource` reconnects automatically on transient network failures,
/// using the spec's `Last-Event-ID` header if the server sets event IDs.
#[wasm_bindgen]
pub fn subscribe_events(url: &str, on_event: Function) -> Result<Subscription, JsValue> {
    let es = EventSource::new(url)?;
    let es_on_message = es.clone();

    let on_message = Closure::wrap(Box::new(move |e: MessageEvent| {
        let Some(raw) = e.data().as_string() else {
            terminate_event_source(
                &es_on_message,
                "[ag-ui-wasm-client] SSE data is not a string; subscription closed",
            );
            return;
        };
        if let Err(parse_err) = parse_ag_ui_event(&raw) {
            terminate_event_source(
                &es_on_message,
                &format!(
                    "[ag-ui-wasm-client] invalid AG-UI event ({parse_err}); subscription closed"
                ),
            );
            return;
        }
        // The typed parse above guarantees valid JSON. Parse again into a real
        // JS Object because downstream code expects `.type`, not an ES6 Map.
        let payload = match js_sys::JSON::parse(&raw) {
            Ok(payload) => payload,
            Err(error) => {
                terminate_event_source(
                    &es_on_message,
                    &format!(
                        "[ag-ui-wasm-client] JSON conversion failed ({error:?}); subscription closed"
                    ),
                );
                return;
            }
        };
        if let Err(err) = on_event.call1(&JsValue::NULL, &payload) {
            terminate_event_source(
                &es_on_message,
                &format!("[ag-ui-wasm-client] callback threw ({err:?}); subscription closed"),
            );
        }
    }) as Box<dyn FnMut(MessageEvent)>);

    es.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

    Ok(Subscription {
        es,
        on_message: Some(on_message),
    })
}

fn parse_ag_ui_event(raw: &str) -> Result<(), serde_json::Error> {
    serde_json::from_str::<AgUiEvent<JsonValue>>(raw).map(|_| ())
}

fn terminate_event_source(es: &EventSource, message: &str) {
    web_sys::console::error_1(&JsValue::from_str(message));
    es.set_onmessage(None);
    es.close();
}

/// POST a focus update to the colab server. Mirrors the AG-UI write pattern
/// (POST + JSON body). The server is expected to broadcast a corresponding
/// `STATE_SNAPSHOT` event on the SSE stream.
#[wasm_bindgen]
pub async fn post_focus(
    url: &str,
    node: Option<String>,
    by: Option<String>,
    reason: Option<String>,
) -> Result<JsValue, JsValue> {
    #[derive(Serialize)]
    struct FocusBody {
        node: Option<String>,
        by: String,
        reason: Option<String>,
    }
    let body = FocusBody {
        node,
        by: by.unwrap_or_else(|| "human".to_string()),
        reason,
    };
    json_post(url, &body).await
}

/// Generic POST helper. Body is any JS value that JSON-stringifies.
#[wasm_bindgen]
pub async fn post_json(url: &str, body: JsValue) -> Result<JsValue, JsValue> {
    let body_str = js_sys::JSON::stringify(&body)?
        .as_string()
        .ok_or_else(|| JsValue::from_str("body could not be stringified"))?;
    raw_post(url, &body_str).await
}

// ── internals ──────────────────────────────────────────────────────────

async fn json_post<T: Serialize>(url: &str, body: &T) -> Result<JsValue, JsValue> {
    let body_str = serde_json::to_string(body)
        .map_err(|e| JsValue::from_str(&format!("serialize body: {e}")))?;
    raw_post(url, &body_str).await
}

async fn raw_post(url: &str, body_str: &str) -> Result<JsValue, JsValue> {
    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&JsValue::from_str(body_str));

    let req = Request::new_with_str_and_init(url, &opts)?;
    req.headers().set("Content-Type", "application/json")?;

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let resp_value = JsFuture::from(window.fetch_with_request(&req)).await?;
    let resp: Response = resp_value.dyn_into()?;
    if !resp.ok() {
        let status = resp.status();
        let status_text = resp.status_text();
        let detail = JsFuture::from(resp.text()?)
            .await?
            .as_string()
            .unwrap_or_default()
            .chars()
            .take(400)
            .collect::<String>();
        return Err(JsValue::from_str(
            format!("HTTP {status} {status_text}: {detail}").trim(),
        ));
    }
    let json_promise = resp.json()?;
    JsFuture::from(json_promise).await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::{parse_ag_ui_event, Frame, Refusal};
    use ag_ui_core::assembly::{Assembler, PushError};
    use serde_json::json;

    const A: &str = "00000000-0000-0000-0000-00000000000a";

    #[test]
    fn typed_event_parser_rejects_malformed_and_unknown_events() {
        assert!(parse_ag_ui_event("not-json").is_err());
        assert!(parse_ag_ui_event(r#"{"type":"UNKNOWN_EVENT"}"#).is_err());
        assert!(parse_ag_ui_event(r#"{"missing":"type"}"#).is_err());
    }

    /// The frame is the JavaScript contract: the validated event under
    /// `event`, assembled updates under `updates`, anomalies only when there
    /// are any. Proven here natively; the wasm boundary only `JSON.parse`s it.
    #[test]
    fn a_frame_carries_the_event_the_updates_and_only_real_anomalies() {
        let mut assembler = Assembler::new();
        let raw =
            json!({"type": "TEXT_MESSAGE_START", "messageId": A, "role": "assistant"}).to_string();
        let (event, outcome) = assembler.push_json(&raw).unwrap();
        let frame = serde_json::to_value(Frame {
            event: &event,
            updates: &outcome.updates,
            anomalies: &outcome.anomalies,
        })
        .unwrap();
        assert_eq!(frame["event"]["type"], "TEXT_MESSAGE_START");
        assert_eq!(frame["updates"][0]["kind"], "text-started");
        assert_eq!(frame["updates"][0]["messageId"], A);
        assert!(frame.get("anomalies").is_none());

        let (event, outcome) = assembler.push_json(&raw).unwrap();
        let frame = serde_json::to_value(Frame {
            event: &event,
            updates: &outcome.updates,
            anomalies: &outcome.anomalies,
        })
        .unwrap();
        assert_eq!(frame["anomalies"][0]["kind"], "text-restarted");
    }

    #[test]
    fn a_refusal_names_its_kind_and_keeps_the_typed_detail() {
        let mut assembler = Assembler::new();
        let raw = json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": A, "delta": "x"}).to_string();
        let error = assembler.push_json(&raw).unwrap_err();
        let refusal = serde_json::to_value(Refusal::from(error)).unwrap();
        assert_eq!(refusal["kind"], "text-content-without-start");
        assert_eq!(refusal["messageId"], A);
        assert!(refusal["message"]
            .as_str()
            .unwrap()
            .contains("never started"));

        let error = assembler.push_json("{nope").unwrap_err();
        assert!(matches!(error, PushError::Parse(_)));
        let refusal = serde_json::to_value(Refusal::from(error)).unwrap();
        assert_eq!(refusal["kind"], "parse");
    }
}
