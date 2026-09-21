use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

const TRACE_SCHEMA: &str = "same-page-trace-v1";
const TRACE_CAPACITY: usize = 256;
const REDACTED: &str = "[REDACTED]";
const PROBE_SECRET: &str = "atlas-secret-negative-control";

static CAPTURE_ENABLED: AtomicBool = AtomicBool::new(true);

thread_local! {
    static TRACE_STATE: RefCell<TraceState> = RefCell::new(TraceState::new(TRACE_CAPACITY));
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
enum TraceValue {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
}

#[derive(Clone, Debug, Serialize)]
struct TraceRecord {
    sequence: u64,
    kind: &'static str,
    trace_id: u64,
    span_id: u64,
    parent_span_id: Option<u64>,
    name: String,
    target: String,
    level: String,
    at_ms: f64,
    started_monotonic_ms: Option<f64>,
    duration_ms: Option<f64>,
    fields: BTreeMap<String, TraceValue>,
}

#[derive(Debug, Serialize)]
struct TraceSnapshot {
    schema: &'static str,
    capacity: usize,
    emitted: u64,
    dropped: u64,
    records: Vec<TraceRecord>,
}

struct TraceState {
    capacity: usize,
    emitted: u64,
    dropped: u64,
    next_span_id: u64,
    records: VecDeque<TraceRecord>,
}

impl TraceState {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            emitted: 0,
            dropped: 0,
            next_span_id: 0,
            records: VecDeque::with_capacity(capacity),
        }
    }

    fn push(&mut self, mut record: TraceRecord) {
        self.emitted += 1;
        record.sequence = self.emitted;
        if self.records.len() == self.capacity {
            self.records.pop_front();
            self.dropped += 1;
        }
        self.records.push_back(record);
    }

    fn clear(&mut self) {
        self.emitted = 0;
        self.dropped = 0;
        self.next_span_id = 0;
        self.records.clear();
    }

    fn allocate_span_id(&mut self) -> u64 {
        self.next_span_id = self.next_span_id.wrapping_add(1);
        if self.next_span_id == 0 {
            self.next_span_id = 1;
        }
        self.next_span_id
    }

    fn snapshot(&self) -> TraceSnapshot {
        TraceSnapshot {
            schema: TRACE_SCHEMA,
            capacity: self.capacity,
            emitted: self.emitted,
            dropped: self.dropped,
            records: self.records.iter().cloned().collect(),
        }
    }
}

#[derive(Clone)]
struct SpanState {
    trace_id: u64,
    span_id: u64,
    parent_span_id: Option<u64>,
    created_at: f64,
    entered_at: Vec<f64>,
    fields: BTreeMap<String, TraceValue>,
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, TraceValue>,
}

impl FieldVisitor {
    fn insert(&mut self, field: &Field, value: TraceValue) {
        let value = if sensitive(field.name()) {
            TraceValue::String(REDACTED.to_string())
        } else {
            value
        };
        self.fields.insert(field.name().to_string(), value);
    }
}

impl Visit for FieldVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, TraceValue::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, TraceValue::I64(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, TraceValue::U64(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        let value = if value.is_finite() {
            TraceValue::F64(value)
        } else {
            TraceValue::String(format!("{value:?}"))
        };
        self.insert(field, value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, TraceValue::String(value.to_string()));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.insert(field, TraceValue::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.insert(field, TraceValue::String(format!("{value:?}")));
    }
}

fn sensitive(name: &str) -> bool {
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
    .any(|part| name.contains(part))
}

fn push(record: TraceRecord) {
    if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    TRACE_STATE.with(|state| state.borrow_mut().push(record));
}

fn allocate_span_id() -> u64 {
    TRACE_STATE.with(|state| state.borrow_mut().allocate_span_id())
}

fn record(
    kind: &'static str,
    trace_id: u64,
    span_id: u64,
    parent_span_id: Option<u64>,
    metadata: &tracing::Metadata<'_>,
    duration_ms: Option<f64>,
    fields: BTreeMap<String, TraceValue>,
) -> TraceRecord {
    TraceRecord {
        sequence: 0,
        kind,
        trace_id,
        span_id,
        parent_span_id,
        name: metadata.name().to_string(),
        target: metadata.target().to_string(),
        level: metadata.level().as_str().to_string(),
        at_ms: wall_time_ms(),
        started_monotonic_ms: None,
        duration_ms,
        fields,
    }
}

pub(crate) struct TraceLayer;

impl<S> Layer<S> for TraceLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let parent = attrs.parent().cloned().or_else(|| {
            attrs
                .is_contextual()
                .then(|| ctx.current_span().id().cloned())
                .flatten()
        });
        let parent_state = parent
            .as_ref()
            .and_then(|parent| ctx.span(parent))
            .and_then(|span| span.extensions().get::<SpanState>().cloned());
        let span_id = allocate_span_id();
        let parent_span_id = parent_state.as_ref().map(|state| state.span_id);
        let trace_id = parent_state
            .as_ref()
            .map_or(span_id, |state| state.trace_id);
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        let fields = visitor.fields;
        let created_at = monotonic_ms();
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(SpanState {
                trace_id,
                span_id,
                parent_span_id,
                created_at,
                entered_at: Vec::new(),
                fields: fields.clone(),
            });
        }
        let mut opening = record(
            "span_open",
            trace_id,
            span_id,
            parent_span_id,
            attrs.metadata(),
            None,
            fields,
        );
        opening.started_monotonic_ms = Some(created_at);
        push(opening);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        if let Some(span) = ctx.span(id) {
            if let Some(state) = span.extensions_mut().get_mut::<SpanState>() {
                let mut visitor = FieldVisitor::default();
                values.record(&mut visitor);
                state.fields.extend(visitor.fields);
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let parent = event.parent().cloned().or_else(|| {
            event
                .is_contextual()
                .then(|| ctx.current_span().id().cloned())
                .flatten()
        });
        let parent_state = parent.as_ref().and_then(|id| {
            ctx.span(id).and_then(|span| {
                span.extensions()
                    .get::<SpanState>()
                    .map(|state| (state.trace_id, state.span_id))
            })
        });
        let (trace_id, parent_span_id) = parent_state.unwrap_or((0, 0));
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        push(record(
            "event",
            trace_id,
            0,
            (parent_span_id != 0).then_some(parent_span_id),
            event.metadata(),
            None,
            visitor.fields,
        ));
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        if let Some(span) = ctx.span(id) {
            if let Some(state) = span.extensions_mut().get_mut::<SpanState>() {
                state.entered_at.push(monotonic_ms());
            }
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let Some(span) = ctx.span(id) else {
            return;
        };
        let metadata = span.metadata();
        let state = {
            let mut extensions = span.extensions_mut();
            let Some(state) = extensions.get_mut::<SpanState>() else {
                return;
            };
            let started = state.entered_at.pop();
            (
                state.trace_id,
                state.span_id,
                state.parent_span_id,
                started.map(|started| (monotonic_ms() - started).max(0.0)),
                state.fields.clone(),
            )
        };
        push(record(
            "span_exit",
            state.0,
            state.1,
            state.2,
            metadata,
            state.3,
            state.4,
        ));
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if !CAPTURE_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let metadata = span.metadata();
        let state = {
            let extensions = span.extensions();
            let Some(state) = extensions.get::<SpanState>() else {
                return;
            };
            (
                state.trace_id,
                state.span_id,
                state.parent_span_id,
                (monotonic_ms() - state.created_at).max(0.0),
                state.fields.clone(),
                state.created_at,
            )
        };
        let mut closing = record(
            "span_close",
            state.0,
            state.1,
            state.2,
            metadata,
            Some(state.3),
            state.4,
        );
        closing.started_monotonic_ms = Some(state.5);
        push(closing);
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn install() -> Result<(), String> {
    use tracing_subscriber::prelude::*;

    let subscriber = tracing_subscriber::registry().with(TraceLayer);

    #[cfg(feature = "user-timing")]
    {
        use wasm_tracing::{ConsoleConfig, WasmLayer, WasmLayerConfig};

        let mut config = WasmLayerConfig::new();
        config
            .set_report_logs_in_timings(true)
            .set_console_config(ConsoleConfig::NoReporting)
            .set_show_fields(false)
            .set_show_origin(false)
            .set_max_level(tracing::Level::TRACE);
        tracing::subscriber::set_global_default(subscriber.with(WasmLayer::new(config)))
            .map_err(|error| format!("could not install browser trace subscriber: {error}"))
    }

    #[cfg(not(feature = "user-timing"))]
    {
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|error| format!("could not install browser trace subscriber: {error}"))
    }
}

pub(crate) fn clear() {
    TRACE_STATE.with(|state| state.borrow_mut().clear());
}

pub(crate) fn snapshot_json() -> Result<String, String> {
    TRACE_STATE.with(|state| {
        serde_json::to_string(&state.borrow().snapshot()).map_err(|error| error.to_string())
    })
}

#[derive(Serialize)]
struct Benchmark {
    samples: u32,
    iterations: u32,
    disabled_ms: Vec<f64>,
    enabled_ms: Vec<f64>,
    disabled_median_ms: f64,
    enabled_median_ms: f64,
    overhead_median_ms: f64,
    overhead_us_per_iteration: f64,
}

pub(crate) fn benchmark_json(iterations: u32, samples: u32) -> Result<String, String> {
    if !(1..=10_000).contains(&iterations) {
        return Err("trace benchmark iterations must be between 1 and 10000".to_string());
    }
    if !(1..=30).contains(&samples) {
        return Err("trace benchmark samples must be between 1 and 30".to_string());
    }

    let mut disabled_ms = Vec::with_capacity(samples as usize);
    let mut enabled_ms = Vec::with_capacity(samples as usize);
    for _ in 0..samples {
        CAPTURE_ENABLED.store(false, Ordering::Relaxed);
        let started = monotonic_ms();
        let disabled_checksum = probe_work(iterations, false);
        disabled_ms.push((monotonic_ms() - started).max(0.0));

        CAPTURE_ENABLED.store(true, Ordering::Relaxed);
        clear();
        let started = monotonic_ms();
        let enabled_checksum = probe_work(iterations, true);
        enabled_ms.push((monotonic_ms() - started).max(0.0));
        std::hint::black_box((disabled_checksum, enabled_checksum));
    }
    CAPTURE_ENABLED.store(true, Ordering::Relaxed);

    let disabled_median_ms = median(&disabled_ms);
    let enabled_median_ms = median(&enabled_ms);
    let overhead_median_ms = enabled_median_ms - disabled_median_ms;
    serde_json::to_string(&Benchmark {
        samples,
        iterations,
        disabled_ms,
        enabled_ms,
        disabled_median_ms,
        enabled_median_ms,
        overhead_median_ms,
        overhead_us_per_iteration: overhead_median_ms * 1000.0 / f64::from(iterations),
    })
    .map_err(|error| error.to_string())
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn probe_work(iterations: u32, instrumented: bool) -> u64 {
    let mut checksum = 0u64;
    for iteration in 0..iterations {
        checksum = checksum.wrapping_add(u64::from(iteration).wrapping_mul(31));
        if instrumented {
            let span = tracing::info_span!(
                "atlas.trace_probe",
                iteration = u64::from(iteration),
                active = true
            );
            let _entered = span.enter();
            tracing::info!(
                event = "atlas.trace_probe.tick",
                checksum,
                valid = true,
                label = "probe",
                api_token = PROBE_SECRET
            );
        } else {
            std::hint::black_box((iteration, checksum));
        }
    }
    checksum
}

#[cfg(target_arch = "wasm32")]
fn wall_time_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
fn wall_time_ms() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0)
}

#[cfg(target_arch = "wasm32")]
fn monotonic_ms() -> f64 {
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen::prelude::wasm_bindgen(js_namespace = performance, js_name = now)]
        fn performance_now() -> f64;
    }
    performance_now()
}

#[cfg(not(target_arch = "wasm32"))]
fn monotonic_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn a_nested_trace_keeps_types_parents_and_redacts_secrets() {
        clear();
        let subscriber = tracing_subscriber::registry().with(TraceLayer);
        tracing::subscriber::with_default(subscriber, || {
            let outer = tracing::info_span!("atlas.snapshot", operation = "snapshot");
            let outer_entered = outer.enter();
            let inner = tracing::info_span!("atlas.read", source = "browser_crdt");
            {
                let _inner_entered = inner.enter();
                tracing::info!(
                    event = "atlas.snapshot.ready",
                    node_count = 3u64,
                    valid = true,
                    surface = "same-page-atlas",
                    api_token = PROBE_SECRET
                );
            }
            drop(inner);
            drop(outer_entered);
            drop(outer);
        });

        let snapshot = TRACE_STATE.with(|state| state.borrow().snapshot());
        let outer = snapshot
            .records
            .iter()
            .find(|record| record.kind == "span_open" && record.name == "atlas.snapshot")
            .expect("outer span");
        let inner = snapshot
            .records
            .iter()
            .find(|record| record.kind == "span_open" && record.name == "atlas.read")
            .expect("inner span");
        let event = snapshot
            .records
            .iter()
            .find(|record| record.kind == "event")
            .expect("event");
        let outer_close = snapshot
            .records
            .iter()
            .find(|record| record.kind == "span_close" && record.span_id == outer.span_id)
            .expect("outer close");
        assert_eq!(outer.started_monotonic_ms, outer_close.started_monotonic_ms);
        assert!(
            inner.started_monotonic_ms.expect("inner start")
                >= outer.started_monotonic_ms.expect("outer start")
        );
        assert_eq!(inner.parent_span_id, Some(outer.span_id));
        assert_eq!(event.parent_span_id, Some(inner.span_id));
        assert_eq!(outer.trace_id, outer.span_id);
        assert_ne!(inner.span_id, outer.span_id);
        assert_eq!(event.fields.get("valid"), Some(&TraceValue::Bool(true)));
        assert_eq!(event.fields.get("node_count"), Some(&TraceValue::U64(3)));
        assert_eq!(
            event.fields.get("surface"),
            Some(&TraceValue::String("same-page-atlas".to_string()))
        );
        assert_eq!(
            event.fields.get("api_token"),
            Some(&TraceValue::String(REDACTED.to_string()))
        );
        assert!(!snapshot_json().expect("trace JSON").contains(PROBE_SECRET));
    }

    #[test]
    fn overflow_is_bounded_and_reports_exact_dropped_records() {
        clear();
        let subscriber = tracing_subscriber::registry().with(TraceLayer);
        tracing::subscriber::with_default(subscriber, || {
            probe_work(TRACE_CAPACITY as u32, true);
        });
        let snapshot = TRACE_STATE.with(|state| state.borrow().snapshot());
        assert_eq!(snapshot.records.len(), TRACE_CAPACITY);
        assert_eq!(
            snapshot.emitted,
            snapshot.records.len() as u64 + snapshot.dropped
        );
        assert!(snapshot.dropped > 0);
        assert!(snapshot
            .records
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1));
        let root_ids = snapshot
            .records
            .iter()
            .filter(|record| record.kind == "span_open")
            .map(|record| record.span_id)
            .collect::<std::collections::BTreeSet<_>>();
        let root_count = snapshot
            .records
            .iter()
            .filter(|record| record.kind == "span_open")
            .count();
        assert_eq!(root_ids.len(), root_count);
    }

    #[test]
    fn benchmark_bounds_refuse_zero_work_before_mutating_trace_state() {
        assert!(benchmark_json(0, 1).is_err());
        assert!(benchmark_json(1, 0).is_err());
    }
}
