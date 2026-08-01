//! Browser AG-UI client. Subscribes to an SSE stream of AG-UI events via the
//! built-in `EventSource` (automatic reconnect, free); POSTs writes via fetch.
//! Deserializes incoming `data: {...}` payloads as `ag_ui_core::Event` so the
//! browser and the agent CLI share the same Rust event types.
//!
//! ## JS surface
//!
//! ```js
//! import init, { subscribe_events, post_focus } from './ag_ui_wasm_client.js';
//! await init();
//! const sub = subscribe_events('/events', (evt) => {
//!   // evt is the deserialized AG-UI Event as a JS object
//!   if (evt.type === 'STATE_SNAPSHOT') applyFocus(evt.snapshot.node);
//! });
//! await post_focus('/focus', 'code/data-sources', 'human', 'tighten add-source UX');
//! sub.close();
//! ```

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
mod tests {
    use super::parse_ag_ui_event;

    #[test]
    fn typed_event_parser_rejects_malformed_and_unknown_events() {
        assert!(parse_ag_ui_event("not-json").is_err());
        assert!(parse_ag_ui_event(r#"{"type":"UNKNOWN_EVENT"}"#).is_err());
        assert!(parse_ag_ui_event(r#"{"missing":"type"}"#).is_err());
    }
}
