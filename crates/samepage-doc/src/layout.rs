//! What the document may contain, and how it is read.
//!
//! The layout is one root map, `objects`, holding one map per record: `kind`
//! plus flat scalar properties (string, finite number, bool). Everything else
//! a Yrs update could create (another root, a nested map, list or text, a
//! subdocument, a null, a buffer, a non-finite number) is outside the layout.
//!
//! [`inspect`] reads a transaction into the records a capability sees and a
//! map of **anomalies**: every piece of state outside the layout, with a
//! canonical fingerprint of its content. The host refuses any replica frame
//! whose candidate document has different anomalies from the live one, so
//! state outside the layout can be preserved from an old document but never
//! created, changed or removed by a replica. Nothing outside the layout is
//! ever handed to a capability. That includes an indexed sequence hidden
//! inside a map branch (see [`sequence_component`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use samepage_capability::{Record, Scalar, View};
use sha2::{Digest, Sha256};
use yrs::branch::Branch;
use yrs::types::ToJson;
use yrs::{Any, Map, MapRef, Out, ReadTxn};

/// The one root the layout allows.
pub const OBJECTS: &str = "objects";

/// Records the layout understands, plus everything it does not.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    pub records: Vec<Record>,
    /// Location -> fingerprint of content outside the layout.
    pub anomalies: BTreeMap<String, String>,
}

pub fn inspect<T: ReadTxn>(txn: &T) -> Inspection {
    let mut records = Vec::new();
    let mut anomalies = BTreeMap::new();
    for (name, root) in txn.root_refs() {
        if name == OBJECTS {
            let Out::YMap(objects) = root else {
                anomalies.insert(format!("root:{name}"), fingerprint(txn, &root));
                continue;
            };
            sequence_component(&objects, "objects", &mut anomalies);
            for (id, entry) in objects.iter(txn) {
                let Out::YMap(map) = &entry else {
                    anomalies.insert(format!("object:{id}"), fingerprint(txn, &entry));
                    continue;
                };
                sequence_component(map, &format!("record:{id}"), &mut anomalies);
                let mut kind = None;
                let mut props = BTreeMap::new();
                for (key, value) in map.iter(txn) {
                    match (key, scalar(&value)) {
                        ("kind", Some(Scalar::Str(text))) if !text.is_empty() => kind = Some(text),
                        ("kind", _) => {
                            anomalies.insert(format!("kind:{id}"), fingerprint(txn, &value));
                        }
                        (_, Some(value)) => {
                            props.insert(key.to_string(), value);
                        }
                        (_, None) => {
                            anomalies.insert(format!("value:{id}:{key}"), fingerprint(txn, &value));
                        }
                    }
                }
                match kind {
                    Some(kind) => records.push(Record {
                        id: id.to_string(),
                        kind,
                        props,
                    }),
                    None => {
                        // A map with no usable kind is preserved whole, not
                        // guessed at.
                        anomalies
                            .entry(format!("kind:{id}"))
                            .or_insert_with(|| fingerprint(txn, &entry));
                        anomalies.insert(format!("object:{id}"), fingerprint(txn, &entry));
                    }
                }
            }
        } else {
            anomalies.insert(format!("root:{name}"), fingerprint(txn, &root));
        }
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    Inspection { records, anomalies }
}

/// A map branch can also hold an indexed sequence (a peer that declared the
/// same root, or wrote to the same branch, as an array or text). Map reads and
/// map events never show it, so its length is recorded as an anomaly: new or
/// changed sequence content is refused, and existing content is preserved.
/// The content itself is not reachable through public Yrs APIs, so a change
/// that keeps both lengths equal is not distinguishable here; such content is
/// never read by any capability.
fn sequence_component(map: &MapRef, location: &str, anomalies: &mut BTreeMap<String, String>) {
    let branch: &Branch = map.as_ref();
    if branch.len() > 0 || branch.content_len() > 0 {
        anomalies.insert(
            format!("sequence:{location}"),
            format!("len={} content_len={}", branch.len(), branch.content_len()),
        );
    }
}

/// The records as a C1 [`View`] at `revision`.
pub fn view(inspection: &Inspection, revision: &str) -> View {
    View::new(revision, inspection.records.clone())
        .expect("inspect produces unique ids, non-empty kinds and finite numbers")
}

fn scalar(value: &Out) -> Option<Scalar> {
    match value {
        Out::Any(Any::String(text)) => Some(Scalar::Str(text.to_string())),
        Out::Any(Any::Number(number)) if number.is_finite() => Some(Scalar::Num(*number)),
        // Integers the Atlas scene layer may have written; they read as numbers.
        Out::Any(Any::BigInt(number)) => Some(Scalar::Num(*number as f64)),
        Out::Any(Any::Bool(flag)) => Some(Scalar::Bool(*flag)),
        _ => None,
    }
}

/// A content fingerprint that does not depend on hash-map iteration order.
fn fingerprint<T: ReadTxn>(txn: &T, value: &Out) -> String {
    let mut text = String::new();
    match value {
        Out::Any(any) => {
            text.push_str("any:");
            canonical(any, &mut text);
        }
        Out::YDoc(doc) => {
            let _ = write!(text, "subdoc:{}", doc.guid());
        }
        other => {
            let _ = write!(text, "type:{}:", type_name(other));
            canonical(&other.to_json(txn), &mut text);
        }
    }
    hex(&Sha256::digest(text.as_bytes()))
}

fn type_name(value: &Out) -> &'static str {
    match value {
        Out::Any(_) => "any",
        Out::YText(_) => "text",
        Out::YArray(_) => "array",
        Out::YMap(_) => "map",
        Out::YXmlElement(_) => "xml-element",
        Out::YXmlFragment(_) => "xml-fragment",
        Out::YXmlText(_) => "xml-text",
        Out::YDoc(_) => "doc",
        _ => "other",
    }
}

fn canonical(any: &Any, out: &mut String) {
    match any {
        Any::Null => out.push_str("null"),
        Any::Undefined => out.push_str("undefined"),
        Any::Bool(flag) => {
            let _ = write!(out, "{flag}");
        }
        Any::Number(number) => {
            let _ = write!(out, "n{:016x}", number.to_bits());
        }
        Any::BigInt(number) => {
            let _ = write!(out, "i{number}");
        }
        Any::String(text) => {
            let _ = write!(out, "{:?}", text.as_ref());
        }
        Any::Buffer(bytes) => {
            out.push_str("b:");
            out.push_str(&hex(bytes));
        }
        Any::Array(items) => {
            out.push('[');
            for item in items.iter() {
                canonical(item, out);
                out.push(',');
            }
            out.push(']');
        }
        Any::Map(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (key, value) in entries {
                let _ = write!(out, "{key:?}:");
                canonical(value, out);
                out.push(',');
            }
            out.push('}');
        }
    }
}

/// The revision: hex SHA-256 over the state vector and the delete set, both in
/// sorted order, so every peer holding the same state computes the same string
/// and a delete-only change still moves it.
pub fn revision<T: ReadTxn>(txn: &T) -> String {
    let snapshot = txn.snapshot();
    let mut clocks: Vec<(u64, u32)> = snapshot
        .state_map
        .iter()
        .map(|(client, clock)| (client.get(), *clock))
        .collect();
    clocks.sort_unstable();
    let mut deleted: Vec<(u64, Vec<(u32, u32)>)> = snapshot
        .delete_set
        .iter()
        .map(|(client, ranges)| {
            let mut spans: Vec<(u32, u32)> = ranges.iter().map(|r| (r.start, r.end)).collect();
            spans.sort_unstable();
            (client.get(), merge_spans(spans))
        })
        .filter(|(_, spans)| !spans.is_empty())
        .collect();
    deleted.sort_unstable();
    let mut text = String::from("sv:");
    for (client, clock) in clocks {
        let _ = write!(text, "{client}={clock};");
    }
    text.push_str("ds:");
    for (client, spans) in deleted {
        let _ = write!(text, "{client}=");
        for (start, end) in spans {
            let _ = write!(text, "{start}-{end},");
        }
        text.push(';');
    }
    hex(&Sha256::digest(text.as_bytes()))
}

fn merge_spans(spans: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
