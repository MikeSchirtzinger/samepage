//! Portable diagnostics. Local measurements are inputs, never document writes.
use crate::{prop_str, Map};
use samepage_capability::{CapError, Capability, View};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Relation {
    axis: String,
    from: String,
    to: String,
    constraint: String,
}
/// Retained sequence-cycle diagnostic: a shortest cycle, with sorted owners.
/// Individual constraints can be SAT while their combined order is UNSAT.
pub fn constraint_state(constraints: &[Value]) -> Value {
    let mut relations = Vec::new();
    for c in constraints
        .iter()
        .filter(|c| c["op"] == "sequence" && c["status"] == "sat")
    {
        if let Some(members) = c["members"].as_array() {
            for pair in members.windows(2) {
                relations.push(Relation {
                    axis: c["axis"].as_str().unwrap_or("x").into(),
                    from: pair[0]["target"].as_str().unwrap_or_default().into(),
                    to: pair[1]["target"].as_str().unwrap_or_default().into(),
                    constraint: c["id"].as_str().unwrap_or_default().into(),
                });
            }
        }
    }
    relations.sort();
    let mut best: Option<(usize, Vec<String>)> = None;
    for relation in &relations {
        let mut queue = VecDeque::from([(relation.to.clone(), Vec::<usize>::new())]);
        let mut visited = BTreeSet::from([relation.to.clone()]);
        while let Some((at, path)) = queue.pop_front() {
            if at == relation.from {
                let mut core = vec![relation.constraint.clone()];
                core.extend(path.iter().map(|i| relations[*i].constraint.clone()));
                core.sort();
                core.dedup();
                let candidate = (path.len() + 1, core);
                if best.as_ref().is_none_or(|b| candidate < *b) {
                    best = Some(candidate);
                }
                break;
            }
            for (i, next) in relations.iter().enumerate() {
                if next.axis == relation.axis && next.from == at && visited.insert(next.to.clone())
                {
                    let mut path = path.clone();
                    path.push(i);
                    queue.push_back((next.to.clone(), path));
                }
            }
        }
    }
    match best {
        Some((_, core)) => json!({"status":"unsat","core":core}),
        None => json!({"status":"sat","core":[]}),
    }
}

/// Report overlap using native estimates or browser-local measured rectangles.
/// Containment is not overlap. Supplied rectangles never stamp attribution.
pub fn report(view: &View, measured: &BTreeMap<String, [f64; 4]>) -> Result<Value, CapError> {
    let projection = Map.project(view)?;
    let cards = projection["cards"]
        .as_array()
        .ok_or_else(|| CapError::new("invalid", "map cards missing"))?;
    let ancestor = |a: &str, b: &str| {
        let mut at = b;
        let mut seen = BTreeSet::new();
        while let Some(r) = view.get(at) {
            let parent = prop_str(r, "parent").unwrap_or_default();
            if parent == a {
                return true;
            }
            if parent.is_empty() || !seen.insert(parent) {
                break;
            }
            at = parent;
        }
        false
    };
    let mut errors = Vec::new();
    let bounds = |card: &Value| {
        let id = card["id"].as_str().unwrap_or_default();
        measured.get(id).copied().unwrap_or_else(|| {
            let x = card["x"].as_f64().unwrap_or(0.0);
            let y = card["y"].as_f64().unwrap_or(0.0);
            [
                x,
                y,
                x + card["width"].as_f64().unwrap_or(232.0),
                y + card["height"].as_f64().unwrap_or(100.0),
            ]
        })
    };
    for (i, a) in cards.iter().enumerate() {
        let aid = a["id"].as_str().unwrap_or_default();
        let aa = bounds(a);
        for b in &cards[i + 1..] {
            let bid = b["id"].as_str().unwrap_or_default();
            if ancestor(aid, bid) || ancestor(bid, aid) {
                continue;
            }
            let bb = bounds(b);
            if aa[0] < bb[2] - 1.0
                && aa[2] > bb[0] + 1.0
                && aa[1] < bb[3] - 1.0
                && aa[3] > bb[1] + 1.0
            {
                errors.push(json!({"code":"card_overlap","ids":[aid,bid],"message":format!("\"{}\" overlaps \"{}\"",a["label"].as_str().unwrap_or_default(),b["label"].as_str().unwrap_or_default())}));
            }
        }
    }
    if projection["constraint_state"]["status"] == "unsat" {
        errors.push(json!({"code":"constraint_unsat","ids":projection["constraint_state"]["core"],"message":"Sequence constraints form a cycle"}));
    }
    for c in projection["constraints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "sat")
    {
        errors.push(json!({"code":"broken_constraint","ids":[c["id"]],"message":format!("Constraint is {}",c["status"].as_str().unwrap_or_default())}));
    }
    Ok(json!({"status":if errors.is_empty(){"passed"}else{"failed"},"errors":errors}))
}
