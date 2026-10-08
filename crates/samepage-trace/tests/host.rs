use samepage_capability::{activity::ActivityRecord, Call, Capability, View};
use samepage_trace::{Ring, Trace, CAPACITY};
use serde_json::json;
fn record(id: &str, parent: Option<&str>, kind: &str, cap: &str) -> ActivityRecord {
    ActivityRecord {
        kind: kind.into(),
        capability: cap.into(),
        span_id: id.into(),
        trace_id: "wasm:doc:1".into(),
        parent_span_id: parent.map(str::to_string),
        name: format!("{cap}.test"),
        at_ms: 10.0,
        started_monotonic_ms: Some(5.0),
        duration_ms: (kind == "span_close").then_some(5.0),
        fields: [
            ("valid".into(), json!(true)),
            ("node_count".into(), json!(3)),
            ("surface".into(), json!("samepage")),
            ("api_token".into(), json!("trace-secret-negative-control")),
            (
                "nested".into(),
                json!({"Authorization":"Bearer secret","list":[{"password":"secret"}]}),
            ),
        ]
        .into(),
    }
}
#[test]
fn a_nested_trace_keeps_types_parents_and_redacts_secrets() {
    let mut ring = Ring::default();
    for r in [
        record("wasm:doc:1", None, "span_open", "doc"),
        record("wasm:doc:2", Some("wasm:doc:1"), "span_open", "doc"),
        record("wasm:doc:2", Some("wasm:doc:2"), "event", "doc"),
        record("wasm:doc:2", Some("wasm:doc:1"), "span_close", "doc"),
        record("wasm:doc:1", None, "span_close", "doc"),
    ] {
        ring.record(r).unwrap();
    }
    let snapshot = ring.snapshot();
    let records = &snapshot.records;
    assert_eq!(
        records[1].event.parent_span_id.as_deref(),
        Some(records[0].event.span_id.as_str())
    );
    assert_eq!(
        records[2].event.parent_span_id.as_deref(),
        Some(records[1].event.span_id.as_str())
    );
    assert_eq!(
        records[0].event.started_monotonic_ms,
        records[4].event.started_monotonic_ms
    );
    assert_eq!(records[2].event.fields["valid"], json!(true));
    assert_eq!(records[2].event.fields["node_count"], json!(3));
    assert_eq!(records[2].event.fields["surface"], json!("samepage"));
    assert_eq!(records[2].event.fields["api_token"], json!("[REDACTED]"));
    let raw = serde_json::to_string(&snapshot).unwrap();
    assert!(!raw.contains("trace-secret-negative-control"));
    assert!(!raw.contains("Bearer secret"));
}
#[test]
fn overflow_is_bounded_and_reports_exact_dropped_records() {
    let mut ring = Ring::default();
    ring.benchmark(CAPACITY as u32, 1, || 0.0).unwrap();
    let snapshot = ring.snapshot();
    assert_eq!(snapshot.records.len(), CAPACITY);
    assert_eq!(snapshot.emitted, 1024);
    assert_eq!(snapshot.dropped, 768);
    assert_eq!(
        snapshot.emitted,
        snapshot.records.len() as u64 + snapshot.dropped
    );
    assert!(snapshot
        .records
        .windows(2)
        .all(|p| p[1].sequence == p[0].sequence + 1));
    let ids: std::collections::BTreeSet<_> = snapshot
        .records
        .iter()
        .filter(|r| r.event.kind == "span_open")
        .map(|r| &r.event.span_id)
        .collect();
    assert_eq!(ids.len(), 64);
}
#[test]
fn benchmark_bounds_refuse_zero_work_before_mutating_trace_state() {
    let mut ring = Ring::default();
    ring.record(record("wasm:doc:1", None, "span_open", "doc"))
        .unwrap();
    let before = ring.snapshot();
    for (i, s) in [(0, 1), (1, 0), (10001, 1), (1, 31)] {
        assert!(ring
            .benchmark(i, s, || panic!("invalid work read clock"))
            .is_err());
        assert_eq!(ring.snapshot(), before);
    }
}
#[test]
fn cross_artifact_capture_keeps_doc_and_map_ids_and_parents_distinct() {
    let mut ring = Ring::default();
    for cap in ["doc", "map"] {
        let root = format!("wasm:{cap}:1");
        let child = format!("wasm:{cap}:2");
        ring.record(record(&root, None, "span_open", cap)).unwrap();
        ring.record(record(&child, Some(&root), "span_open", cap))
            .unwrap();
    }
    let s = ring.snapshot();
    assert_ne!(s.records[0].event.span_id, s.records[2].event.span_id);
    assert_eq!(
        s.records[1].event.parent_span_id.as_ref(),
        Some(&s.records[0].event.span_id)
    );
    assert_eq!(
        s.records[3].event.parent_span_id.as_ref(),
        Some(&s.records[2].event.span_id)
    );
}
#[test]
fn trace_plans_do_not_change_the_document_and_bad_records_are_atomic() {
    let view = View::new("r", []).unwrap();
    let batch = Trace
        .plan_batch(
            &Call {
                action: "trace_clear".into(),
                args: json!({}),
            },
            &view,
        )
        .unwrap();
    assert_eq!(batch.entries.len(), 1);
    assert!(batch.entries[0].plan.ops.is_empty());
    assert!(samepage_trace::descriptor().depends_on.is_empty());
    let mut ring = Ring::default();
    let before = ring.snapshot();
    let mut r = record("id", None, "span_close", "doc");
    r.duration_ms = Some(-1.0);
    assert!(ring.record(r).is_err());
    assert_eq!(before, ring.snapshot());
    assert!(Ring::new(0).is_err());
}
