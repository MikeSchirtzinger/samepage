//! The vocabulary and limits of a card and a link, and the checks that hold
//! them. Plans use these before proposing a write; `validate` applies the same
//! checks to every committed change and every browser frame.

use std::collections::{BTreeMap, BTreeSet};

use samepage_capability::{CapError, Record, Scalar, View};

pub const NODE: &str = "node";
pub const EDGE: &str = "edge";

pub const K_LABEL: &str = "label";
pub const K_NOTE: &str = "note";
pub const K_TONE: &str = "tone";
pub const K_STATUS: &str = "status";
pub const K_X: &str = "x";
pub const K_Y: &str = "y";
pub const K_W: &str = "w";
/// Measured content height at width `w`, written by the renderer.
pub const K_H: &str = "h";
pub const K_H_PREF: &str = "h_pref";
pub const K_W_MIN: &str = "w_min";
pub const K_H_MIN: &str = "h_min";
pub const K_PARENT: &str = "parent";
pub const K_COLOR: &str = "node_color";
pub const K_EMPHASIS: &str = "emphasis";
pub const K_SIZE: &str = "size";
pub const K_SHAPE: &str = "node_kind";
pub const K_FROM: &str = "from";
pub const K_TO: &str = "to";
/// A link carrying this key is a state-machine transition (diagrams' key).
pub const K_EVENT: &str = "event";

pub const NODE_KEYS: &[&str] = &[
    K_LABEL, K_NOTE, K_TONE, K_STATUS, K_X, K_Y, K_W, K_H, K_H_PREF, K_W_MIN, K_H_MIN, K_PARENT,
    K_COLOR, K_EMPHASIS, K_SIZE, K_SHAPE,
];
pub const EDGE_KEYS: &[&str] = &[K_FROM, K_TO, K_LABEL];

pub const TONES: &[&str] = &[
    "crate", "example", "doc", "spec", "concept", "question", "risk",
];
pub const STATUSES: &[&str] = &["open", "agreed", "disputed", "done"];
pub const INKS: &[&str] = &["slate", "blue", "green", "amber", "red", "violet"];
pub const EMPHASES: &[&str] = &["normal", "strong", "muted"];
pub const SIZES: &[&str] = &["normal", "primary", "hero"];
pub const SHAPES: &[&str] = &["rect", "ellipse", "diamond"];

pub const WORLD_LIMIT: f64 = 20_000.0;
pub const DEFAULT_WIDTH: f64 = 232.0;
pub const MIN_W: f64 = 120.0;
/// Bounded initial policy (architecture 0.1 item 2), checked here and in D1.
pub const MAX_W: f64 = 1_600.0;
pub const MIN_H: f64 = 48.0;
pub const MAX_H: f64 = 1_600.0;
pub const MAX_LABEL: usize = 140;
pub const MAX_NOTE: usize = 4_000;
pub const MAX_DEPTH: usize = 6;
/// Cards plus links this capability will hold. Above the Atlas's shared 300
/// objects on purpose: the integration gate observes cost at 500 cards.
pub const MAX_MAP_OBJECTS: usize = 2_000;

pub fn err(code: &str, message: impl Into<String>) -> CapError {
    CapError::new(code, message)
}

pub fn text(value: &str, limit: usize, field: &str) -> Result<String, CapError> {
    let trimmed = value.trim();
    if trimmed.chars().count() > limit {
        return Err(err(
            "invalid",
            format!("{field} is longer than {limit} characters"),
        ));
    }
    if trimmed.chars().any(|c| c.is_control() && c != '\n') {
        return Err(err(
            "invalid",
            format!("{field} contains control characters"),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn one_of(value: &str, allowed: &[&str], field: &str) -> Result<String, CapError> {
    let value = value.trim();
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(err(
            "invalid",
            format!("{field} must be one of {allowed:?}, not {value:?}"),
        ))
    }
}

pub fn coordinate(value: f64, field: &str) -> Result<f64, CapError> {
    if !value.is_finite() || value.abs() > WORLD_LIMIT {
        return Err(err(
            "invalid",
            format!("{field} must be a finite number within ±{WORLD_LIMIT}"),
        ));
    }
    Ok(value)
}

pub fn within(value: f64, low: f64, high: f64, field: &str) -> Result<f64, CapError> {
    if !value.is_finite() || value < low || value > high {
        return Err(err(
            "invalid",
            format!("{field} must be between {low} and {high}, not {value}"),
        ));
    }
    Ok(value)
}

/// `h_pref` is 0 (fit content) or a real preferred height.
pub fn preferred_height(value: f64) -> Result<f64, CapError> {
    if value == 0.0 {
        return Ok(0.0);
    }
    within(value, MIN_H, MAX_H, K_H_PREF)
}

fn number(record: &Record, key: &str) -> Result<Option<f64>, CapError> {
    match record.props.get(key) {
        None => Ok(None),
        Some(Scalar::Num(value)) => Ok(Some(*value)),
        Some(_) => Err(err(
            "invalid",
            format!("{}.{key} must be a number", record.id),
        )),
    }
}

fn string<'a>(record: &'a Record, key: &str) -> Result<Option<&'a str>, CapError> {
    match record.props.get(key) {
        None => Ok(None),
        Some(Scalar::Str(value)) => Ok(Some(value)),
        Some(_) => Err(err(
            "invalid",
            format!("{}.{key} must be a string", record.id),
        )),
    }
}

/// Check the keys of a card that changed (all of them for a new card).
pub fn check_node(
    record: &Record,
    keys: &BTreeSet<String>,
    created: bool,
    view: &View,
) -> Result<(), CapError> {
    let has = |key: &str| created || keys.contains(key);
    if created {
        let label = string(record, K_LABEL)?.unwrap_or_default();
        if label.trim().is_empty() {
            return Err(err("invalid", format!("{} needs a label", record.id)));
        }
    }
    if has(K_LABEL) {
        if let Some(label) = string(record, K_LABEL)? {
            if text(label, MAX_LABEL, "label")? != label {
                return Err(err("invalid", "label must not have surrounding whitespace"));
            }
            if label.is_empty() {
                return Err(err("invalid", format!("{} needs a label", record.id)));
            }
        }
    }
    if has(K_NOTE) {
        if let Some(note) = string(record, K_NOTE)? {
            text(note, MAX_NOTE, "note")?;
        }
    }
    for (key, allowed) in [
        (K_TONE, TONES),
        (K_STATUS, STATUSES),
        (K_EMPHASIS, EMPHASES),
        (K_SIZE, SIZES),
        (K_SHAPE, SHAPES),
    ] {
        if has(key) {
            if let Some(value) = string(record, key)? {
                one_of(value, allowed, key)?;
            }
        }
    }
    if has(K_COLOR) {
        if let Some(color) = string(record, K_COLOR)? {
            if !color.is_empty() {
                one_of(color, INKS, "color")?;
            }
        }
    }
    for key in [K_X, K_Y] {
        if has(key) {
            if let Some(value) = number(record, key)? {
                coordinate(value, key)?;
            }
        }
    }
    if has(K_W) || has(K_W_MIN) {
        let w = number(record, K_W)?;
        if let Some(w) = w {
            within(w, MIN_W, MAX_W, K_W)?;
        }
        if let Some(w_min) = number(record, K_W_MIN)? {
            within(w_min, MIN_W, MAX_W, K_W_MIN)?;
            if w_min > w.unwrap_or(DEFAULT_WIDTH) {
                return Err(err("invalid", "w_min is above the preferred width w"));
            }
        }
    }
    if has(K_H) {
        if let Some(h) = number(record, K_H)? {
            within(h, 0.0, WORLD_LIMIT, K_H)?;
        }
    }
    if has(K_H_PREF) || has(K_H_MIN) {
        let h_pref = number(record, K_H_PREF)?.unwrap_or(0.0);
        preferred_height(h_pref)?;
        if let Some(h_min) = number(record, K_H_MIN)? {
            within(h_min, MIN_H, MAX_H, K_H_MIN)?;
            if h_pref > 0.0 && h_min > h_pref {
                return Err(err("invalid", "h_min is above the preferred height h_pref"));
            }
        }
    }
    if has(K_PARENT) {
        if let Some(parent) = string(record, K_PARENT)? {
            check_parent(&record.id, parent, view)?;
        }
    }
    Ok(())
}

/// A parent is empty or an existing card; containment stays a shallow tree.
pub fn check_parent(id: &str, parent: &str, view: &View) -> Result<(), CapError> {
    if parent.is_empty() {
        return Ok(());
    }
    if parent == id {
        return Err(err("invalid", format!("{id} cannot contain itself")));
    }
    let parents = parents(view);
    if !matches!(view.get(parent), Some(record) if record.kind == NODE) {
        return Err(err(
            "not_found",
            format!("no card {parent:?} to put {id} inside"),
        ));
    }
    // Walk up from the new parent: reaching `id` is a cycle; the chain above
    // plus the deepest chain below `id` must fit the depth limit.
    let mut depth = 1;
    let mut cursor = parent.to_string();
    let mut seen = BTreeSet::new();
    while let Some(next) = parents.get(&cursor).filter(|p| !p.is_empty()) {
        if next == id || !seen.insert(next.clone()) {
            return Err(err(
                "invalid",
                format!("putting {id} inside {parent} would make containment a cycle"),
            ));
        }
        depth += 1;
        cursor = next.clone();
    }
    if depth + below(id, &parents) > MAX_DEPTH {
        return Err(err(
            "invalid",
            format!("containment deeper than {MAX_DEPTH} levels"),
        ));
    }
    Ok(())
}

pub fn parents(view: &View) -> BTreeMap<String, String> {
    view.records()
        .filter(|record| record.kind == NODE)
        .map(|record| {
            let parent = record
                .props
                .get(K_PARENT)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            (record.id.clone(), parent.to_string())
        })
        .collect()
}

/// Depth of the deepest chain of children under `id`, counting `id`'s own
/// level as 0.
fn below(id: &str, parents: &BTreeMap<String, String>) -> usize {
    fn walk(id: &str, parents: &BTreeMap<String, String>, guard: usize) -> usize {
        if guard > MAX_DEPTH + 1 {
            return guard;
        }
        parents
            .iter()
            .filter(|(_, parent)| parent.as_str() == id)
            .map(|(child, _)| 1 + walk(child, parents, guard + 1))
            .max()
            .unwrap_or(0)
    }
    walk(id, parents, 0)
}

pub fn check_edge(record: &Record, view: &View) -> Result<(), CapError> {
    let from = string(record, K_FROM)?.unwrap_or_default();
    let to = string(record, K_TO)?.unwrap_or_default();
    for (end, id) in [("from", from), ("to", to)] {
        if !matches!(view.get(id), Some(target) if target.kind == NODE) {
            return Err(err(
                "not_found",
                format!("link {} {end} {id:?} is not a card", record.id),
            ));
        }
    }
    if from == to {
        return Err(err("invalid", "a link needs two different cards"));
    }
    if let Some(label) = string(record, K_LABEL)? {
        text(label, MAX_LABEL, "label")?;
    }
    Ok(())
}
