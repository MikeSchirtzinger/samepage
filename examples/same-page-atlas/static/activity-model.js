// Timing is derived only from recorded spans or explicitly paired host events.
// Missing endpoints remain missing. Separate clocks are never joined by proximity.
export function wasmSpans(snapshot) {
  const records = snapshot?.records || [];
  const opens = new Map(records.filter(r => r.kind === "span_open").map(r => [r.span_id, r]));
  const anchor = records.find(r => r.kind === "span_open" && Number.isFinite(r.started_monotonic_ms));
  const origin = anchor ? anchor.at_ms - anchor.started_monotonic_ms : null;
  return records.filter(r => r.kind === "span_close" && Number.isFinite(r.duration_ms)).map(r => ({
    id: String(r.span_id), parent: r.parent_span_id === null ? null : String(r.parent_span_id),
    name: r.name, start: origin !== null && Number.isFinite(r.started_monotonic_ms)
      ? origin + r.started_monotonic_ms : opens.get(r.span_id)?.at_ms ?? r.at_ms - r.duration_ms,
    duration: r.duration_ms, fields: r.fields, trace: r.trace_id,
    partial: !opens.has(r.span_id), source: "Browser WASM",
  }));
}

export function hostSpans(snapshot) {
  return (snapshot?.events || []).filter(e => e.timing && Number.isFinite(e.timing.durationUs)).flatMap(e => {
    const root = {
      id: e.correlation.eventId, parent: null, name: e.kind.replace(/^action\./, ""),
      start: e.timing.startedAtMs, duration: e.timing.durationUs / 1000,
      fields: { actor: e.actor.label, outcome: e.outcome, before: e.stateRevisionBefore, after: e.stateRevisionAfter },
      trace: e.correlation.eventId, run: e.correlation.runId, source: "Host dispatcher", partial: false,
    };
    return [root, ...(e.timing.spans || []).map((span, index) => ({
      ...root, id: `${root.id}:${index}`, parent: root.id, name: span.name,
      start: root.start + span.offsetUs / 1000, duration: span.durationUs / 1000,
    }))];
  });
}

export function layoutSpans(spans) {
  if (!spans.length) return { spans: [], start: 0, duration: 0, depth: 0 };
  const byId = new Map(spans.map(span => [span.id, span]));
  const start = Math.min(...spans.map(s => s.start));
  const end = Math.max(...spans.map(s => s.start + s.duration));
  const duration = Math.max(end - start, 0);
  const scale = duration || 1;
  const lanes = [];
  const placed = new Map();
  const sorted = [...spans].sort((a, b) => a.start - b.start || b.duration - a.duration);
  const place = (span, ancestry = new Set()) => {
    if (placed.has(span.id)) return placed.get(span.id);
    if (ancestry.has(span.id)) return null;
    ancestry.add(span.id);
    const parent = span.parent && byId.get(span.parent);
    const parentPlaced = parent && place(parent, ancestry);
    let lane = parentPlaced ? parentPlaced.lane + 1 : 0;
    while ((lanes[lane] || []).some(s => span.start < s.start + Math.max(s.duration, .001) && span.start + Math.max(span.duration, .001) > s.start)) lane++;
    const result = { ...span, lane, left: (span.start - start) / scale * 100, width: Math.max(span.duration / scale * 100, .15) };
    (lanes[lane] ||= []).push(span);
    placed.set(span.id, result);
    return result;
  };
  sorted.forEach(span => place(span));
  return { spans: [...placed.values()], start, duration, depth: lanes.length };
}
