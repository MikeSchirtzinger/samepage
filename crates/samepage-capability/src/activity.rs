//! Counted, storage-free instrumentation shared by independently compiled artifacts.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActivityRecord {
    pub kind: String,
    pub capability: String,
    pub span_id: String,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub level: String,
    pub at_ms: f64,
    pub started_monotonic_ms: Option<f64>,
    pub duration_ms: Option<f64>,
    pub fields: std::collections::BTreeMap<String, Value>,
}
static NEXT: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
thread_local! { static PARENTS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) }; }

pub trait ActivitySink: Send + Sync {
    fn emit(&self, record: ActivityRecord);
}
#[cfg(not(target_arch = "wasm32"))]
fn sink() -> &'static std::sync::RwLock<Option<std::sync::Arc<dyn ActivitySink>>> {
    static SINK: std::sync::OnceLock<std::sync::RwLock<Option<std::sync::Arc<dyn ActivitySink>>>> =
        std::sync::OnceLock::new();
    SINK.get_or_init(Default::default)
}
#[cfg(not(target_arch = "wasm32"))]
pub fn install_sink(value: Option<std::sync::Arc<dyn ActivitySink>>) {
    *sink().write().unwrap_or_else(|e| e.into_inner()) = value;
}
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}
fn emit(record: ActivityRecord) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::{JsCast, JsValue};
        let global = js_sys::global();
        let hook = js_sys::Reflect::get(&global, &JsValue::from_str("__samepageTrace"))
            .ok()
            .and_then(|v| v.dyn_into::<js_sys::Function>().ok());
        if let Some(hook) = hook {
            if let Ok(raw) = serde_json::to_string(&record) {
                let _ = hook.call1(&global, &JsValue::from_str(&raw));
            }
        } else {
            let key = JsValue::from_str("__samepageTraceDropped");
            let count = js_sys::Reflect::get(&global, &key)
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let _ = js_sys::Reflect::set(&global, &key, &JsValue::from_f64(count + 1.0));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let target = sink().read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(target) = target {
            target.emit(record);
        } else {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}
pub fn now() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::{JsCast, JsValue};
        let global = js_sys::global();
        let perf = js_sys::Reflect::get(&global, &JsValue::from_str("performance")).unwrap();
        let fun: js_sys::Function = js_sys::Reflect::get(&perf, &JsValue::from_str("now"))
            .unwrap()
            .unchecked_into();
        fun.call0(&perf).unwrap().as_f64().unwrap()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.0
    }
}
pub fn wall_time() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    }
}

/// A span never records inputs, credentials, source text or action arguments.
/// Identity includes the artifact namespace, since WASM binaries have separate counters.
pub struct Span {
    record: ActivityRecord,
    started: f64,
}
impl Span {
    pub fn new(capability: &str, name: &str) -> Self {
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed) + 1;
        let origin = if cfg!(target_arch = "wasm32") {
            "wasm"
        } else {
            "native"
        };
        let id = format!("{origin}:{capability}:{sequence}");
        let (parent, trace) = PARENTS
            .with(|p| p.borrow().last().cloned())
            .map_or((None, id.clone()), |(p, t)| (Some(p), t));
        PARENTS.with(|p| p.borrow_mut().push((id.clone(), trace.clone())));
        let started = now();
        let record = ActivityRecord {
            kind: "span_open".into(),
            capability: capability.into(),
            span_id: id,
            trace_id: trace,
            parent_span_id: parent,
            name: name.into(),
            target: format!("samepage::{capability}"),
            level: "INFO".into(),
            at_ms: wall_time(),
            started_monotonic_ms: Some(started),
            duration_ms: None,
            fields: [("origin".into(), json!(origin))].into(),
        };
        emit(record.clone());
        Self { record, started }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        PARENTS.with(|p| {
            p.borrow_mut().pop();
        });
        self.record.kind = "span_close".into();
        self.record.at_ms = wall_time();
        self.record.duration_ms = Some((now() - self.started).max(0.0));
        emit(self.record.clone());
    }
}
