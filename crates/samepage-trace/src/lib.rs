//! Optional trace storage. No document, filesystem or subscriber in the WASM closure.
use samepage_capability::activity::ActivityRecord;
use samepage_capability::{
    ActionDecl, Artifact, Audience, Call, CapError, Capability, Delta, Descriptor, Effect, Plan,
    Trigger, View,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
pub const ID: &str = "trace";
pub const VERSION: &str = "0.1.0";
pub const CAPACITY: usize = 256;
pub const REDACTED: &str = "[REDACTED]";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TraceRecord {
    pub sequence: u64,
    #[serde(flatten)]
    pub event: ActivityRecord,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub schema: String,
    pub capacity: usize,
    pub emitted: u64,
    pub dropped: u64,
    pub records: Vec<TraceRecord>,
}
#[derive(Debug)]
pub struct Ring {
    capacity: usize,
    emitted: u64,
    dropped: u64,
    records: VecDeque<TraceRecord>,
}
impl Default for Ring {
    fn default() -> Self {
        Self::new(CAPACITY).unwrap()
    }
}
fn invalid(message: impl Into<String>) -> CapError {
    CapError::new("invalid", message)
}
pub fn sensitive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|p| name.contains(p))
}
fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if sensitive(key) {
                    *value = json!(REDACTED);
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact(item);
            }
        }
        _ => {}
    }
}
impl Ring {
    pub fn new(capacity: usize) -> Result<Self, CapError> {
        if !(1..=CAPACITY).contains(&capacity) {
            return Err(invalid("trace capacity must be 1..256"));
        }
        Ok(Self {
            capacity,
            emitted: 0,
            dropped: 0,
            records: VecDeque::with_capacity(capacity),
        })
    }
    pub fn record(&mut self, mut event: ActivityRecord) -> Result<(), CapError> {
        if !matches!(
            event.kind.as_str(),
            "span_open" | "span_close" | "span_exit" | "event"
        ) || event.capability.is_empty()
            || event.capability.len() > 64
            || event.name.len() > 256
            || event.target.len() > 256
            || event.level.len() > 16
            || event.span_id.len() > 128
            || event.trace_id.len() > 128
            || event
                .parent_span_id
                .as_ref()
                .is_some_and(|id| id.len() > 128)
            || !event.at_ms.is_finite()
            || event.started_monotonic_ms.is_some_and(|v| !v.is_finite())
            || event.duration_ms.is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err(invalid("invalid typed trace record"));
        }
        if serde_json::to_vec(&event)
            .map_err(|e| invalid(e.to_string()))?
            .len()
            > 16384
        {
            return Err(invalid("trace record exceeds 16 KiB"));
        }
        for (key, value) in &mut event.fields {
            if sensitive(key) {
                *value = json!(REDACTED);
            } else {
                redact(value);
            }
        }
        self.emitted += 1;
        if self.records.len() == self.capacity {
            self.records.pop_front();
            self.dropped += 1;
        }
        self.records.push_back(TraceRecord {
            sequence: self.emitted,
            event,
        });
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            schema: "same-page-trace-v1".into(),
            capacity: self.capacity,
            emitted: self.emitted,
            dropped: self.dropped,
            records: self.records.iter().cloned().collect(),
        }
    }
    pub fn clear(&mut self) {
        self.emitted = 0;
        self.dropped = 0;
        self.records.clear();
    }
    /// The clock is injected. Runtime uses the monotonic clock; equivalence uses
    /// an explicit deterministic clock, so the real portable work is compared.
    pub fn benchmark(
        &mut self,
        iterations: u32,
        samples: u32,
        clock: impl FnMut() -> f64,
    ) -> Result<Value, CapError> {
        self.benchmark_at(iterations, samples, clock, 0.0)
    }
    /// A monotonic clock plus its epoch offset. Runtime records retain real
    /// per-probe timings; fixtures supply a deterministic clock and offset.
    pub fn benchmark_at(
        &mut self,
        iterations: u32,
        samples: u32,
        mut clock: impl FnMut() -> f64,
        epoch_ms: f64,
    ) -> Result<Value, CapError> {
        bounds(iterations, samples)?;
        let mut disabled = Vec::new();
        let mut enabled = Vec::new();
        let mut checksum = 0u64;
        for sample in 0..samples {
            let start = clock();
            for i in 0..iterations {
                checksum = checksum.wrapping_add(u64::from(i) * 31);
                std::hint::black_box(checksum);
            }
            disabled.push((clock() - start).max(0.0));
            self.clear();
            let start = clock();
            for i in 0..iterations {
                checksum = checksum.wrapping_add(u64::from(i) * 31);
                let id = format!("benchmark:trace:{sample}:{i}");
                let probe_start = clock();
                let event = ActivityRecord {
                    kind: "span_open".into(),
                    capability: ID.into(),
                    span_id: id.clone(),
                    trace_id: id.clone(),
                    parent_span_id: None,
                    name: "trace.probe".into(),
                    target: "samepage::trace".into(),
                    level: "INFO".into(),
                    at_ms: epoch_ms + probe_start,
                    started_monotonic_ms: Some(probe_start),
                    duration_ms: None,
                    fields: [
                        ("iteration".into(), json!(i)),
                        ("valid".into(), json!(true)),
                        ("api_token".into(), json!("trace-secret-negative-control")),
                    ]
                    .into(),
                };
                self.record(event.clone())?;
                let mut tick = event.clone();
                tick.kind = "event".into();
                tick.at_ms = epoch_ms + clock();
                tick.parent_span_id = Some(id);
                tick.fields.insert("checksum".into(), json!(checksum));
                self.record(tick)?;
                let mut exit = event.clone();
                exit.kind = "span_exit".into();
                let exited = clock();
                exit.at_ms = epoch_ms + exited;
                exit.duration_ms = Some((exited - probe_start).max(0.0));
                self.record(exit)?;
                let mut close = event;
                close.kind = "span_close".into();
                let closed = clock();
                close.at_ms = epoch_ms + closed;
                close.duration_ms = Some((closed - probe_start).max(0.0));
                self.record(close)?;
            }
            enabled.push((clock() - start).max(0.0));
        }
        let d = median(&disabled);
        let e = median(&enabled);
        Ok(
            json!({"samples":samples,"iterations":iterations,"disabled_ms":disabled,"enabled_ms":enabled,"disabled_median_ms":d,"enabled_median_ms":e,"overhead_median_ms":e-d,"overhead_us_per_iteration":(e-d)*1000.0/f64::from(iterations),"checksum":checksum}),
        )
    }
}
fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let m = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[m - 1] + sorted[m]) / 2.0
    } else {
        sorted[m]
    }
}
pub fn bounds(iterations: u32, samples: u32) -> Result<(), CapError> {
    if !(1..=10000).contains(&iterations) {
        return Err(invalid(
            "trace benchmark iterations must be between 1 and 10000",
        ));
    }
    if !(1..=30).contains(&samples) {
        return Err(invalid("trace benchmark samples must be between 1 and 30"));
    }
    Ok(())
}
pub fn descriptor() -> Descriptor {
    let mut d = Descriptor::new(ID, VERSION, Artifact::Wasm).trigger(Trigger::route("activity"));
    for (name, effect, description) in [
        (
            "trace_read",
            Effect::Query,
            "Read the diagnostic spans this host holds in memory, such as each capability's plan time, oldest first, at most 256 with older ones dropped. Spans are timing, not history: the activity journal records who changed what.",
        ),
        (
            "trace_clear",
            Effect::Mutate,
            "Empty the in-memory span ring. The document and the activity journal are untouched.",
        ),
        (
            "trace_benchmark",
            Effect::Mutate,
            "Measure what recording a span costs: record iterations spans in each of samples runs and report the timings. The benchmark spans enter the ring.",
        ),
    ] {
        let mut a = ActionDecl::new(name, Audience::Both, effect, description);
        a.input_schema = if name == "trace_benchmark" {
            json!({"type":"object","required":["iterations","samples"],"properties":{"iterations":{"type":"integer","minimum":1,"maximum":10000},"samples":{"type":"integer","minimum":1,"maximum":30}},"additionalProperties":false})
        } else {
            json!({"type":"object","properties":{},"additionalProperties":false})
        };
        d = d.action(a);
    }
    d
}
pub struct Trace;
impl Capability for Trace {
    fn descriptor(&self) -> Descriptor {
        descriptor()
    }
    fn project(&self, _: &View) -> Result<Value, CapError> {
        Ok(
            json!({"schema":"same-page-trace-v1","capacity":CAPACITY,"storage":"process-local","document_records":false}),
        )
    }
    fn digest(&self, v: &View) -> Result<Value, CapError> {
        self.project(v)
    }
    fn validate(&self, _: &Delta, _: &View) -> Result<(), CapError> {
        Ok(())
    }
    fn plan(&self, call: &Call, _: &View) -> Result<Plan, CapError> {
        let a = descriptor()
            .actions
            .into_iter()
            .find(|a| a.name == call.action)
            .ok_or_else(|| CapError::new("unknown_action", &call.action))?;
        let args = call
            .args
            .as_object()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        for key in args.keys() {
            if a.input_schema["properties"].get(key).is_none() {
                return Err(invalid(format!("unknown argument {key}")));
            }
        }
        if call.action == "trace_benchmark" {
            let i = call.args["iterations"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| invalid("iterations must be u32"))?;
            let s = call.args["samples"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| invalid("samples must be u32"))?;
            bounds(i, s)?;
        }
        Ok(Plan {
            ops: vec![],
            readback: call.action.clone(),
        })
    }
}
/// One identical portable stateful protocol, used by native adapters and WASM.
/// Benchmark's test clock is optional; the drawer never supplies it.
pub fn operate(ring: &mut Ring, raw: &str) -> String {
    let result = (|| -> Result<Value, CapError> {
        let input: Value =
            serde_json::from_str(raw).map_err(|e| CapError::new("bad_input", e.to_string()))?;
        match input["operation"].as_str().unwrap_or("") {
            "record" => {
                let r = serde_json::from_value(input["record"].clone())
                    .map_err(|e| invalid(e.to_string()))?;
                ring.record(r)?;
                Ok(json!(ring.snapshot()))
            }
            "snapshot" => Ok(json!(ring.snapshot())),
            "clear" => {
                ring.clear();
                Ok(json!(ring.snapshot()))
            }
            "benchmark" => {
                let i = input["iterations"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| invalid("iterations must be u32"))?;
                let s = input["samples"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| invalid("samples must be u32"))?;
                if let Some(step) = input["clock_step_ms"].as_f64() {
                    if !step.is_finite() || step < 0.0 {
                        return Err(invalid("invalid clock step"));
                    }
                    let mut tick = 0.0;
                    ring.benchmark(i, s, || {
                        tick += step;
                        tick
                    })
                } else {
                    ring.benchmark_at(
                        i,
                        s,
                        samepage_capability::activity::now,
                        samepage_capability::activity::wall_time()
                            - samepage_capability::activity::now(),
                    )
                }
            }
            _ => Err(CapError::new("unknown_action", "unknown trace operation")),
        }
    })();
    match result {
        Ok(v) => json!({"abi":samepage_capability::ABI,"ok":v}),
        Err(e) => json!({"abi":samepage_capability::ABI,"error":e}),
    }
    .to_string()
}
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
samepage_capability::export_capability!(Trace, Trace);
#[cfg(all(target_arch = "wasm32", feature = "wasm-export"))]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn trace_operation(input: &str) -> String {
    static RING: std::sync::OnceLock<std::sync::Mutex<Ring>> = std::sync::OnceLock::new();
    operate(
        &mut RING.get_or_init(Default::default).lock().unwrap(),
        input,
    )
}
