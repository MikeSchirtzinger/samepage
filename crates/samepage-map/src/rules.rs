//! The vocabulary and limits of a card and a link, and the checks that hold
//! them. Plans use these before proposing a write; `validate` applies the same
//! checks to every committed change and every browser frame.

use std::collections::{BTreeMap, BTreeSet};

use samepage_capability::{CapError, Record, Scalar, View};

pub const NODE: &str = "node";
pub const EDGE: &str = "edge";
pub const CONSTRAINT: &str = "constraint";
/// One membership of one constraint. Normalized so each `target` is a whole
/// property: a comma-joined scalar cannot carry a minted id, and the kernel
/// resolves a reference into a whole property only.
pub const CONSTRAINT_MEMBER: &str = "constraint-member";

pub const K_LABEL: &str = "label";
pub const K_NOTE: &str = "note";
pub const K_TONE: &str = "tone";
pub const K_STATUS: &str = "status";
pub const K_X: &str = "x";
pub const K_Y: &str = "y";
pub const K_W: &str = "w";
/// Legacy measured height. **Read-only history.** No route writes it: a
/// browser's measurement is that browser's, and two peers with different
/// metrics would otherwise fight over one shared number. Existing values are
/// projected as `legacy_h` and never presented as this render's height.
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

pub const K_OP: &str = "op";
/// The Atlas's comma-joined membership. **Frozen history.** It is decoded once
/// for a constraint that has not been converted, and after conversion it is
/// never read and never written, by any route.
pub const K_MEMBERS: &str = "members";
/// Set to [`SCHEMA_RECORDS`] when a constraint's membership moved into
/// `constraint-member` records. The marker, not the record count, decides
/// where membership is read from, so emptying a constraint cannot resurrect
/// the frozen scalar. One way: nothing may clear or rewrite it while the
/// constraint lives.
pub const K_MEMBER_SCHEMA: &str = "member_schema";
pub const SCHEMA_RECORDS: &str = "records";
pub const K_AXIS: &str = "axis";
pub const K_TEXT: &str = "text";
pub const K_SALIENCE: &str = "salience";
pub const K_CONSTRAINT: &str = "constraint";
pub const K_TARGET: &str = "target";
pub const K_ORDER: &str = "order";
/// Keys the diagrams capability owns on a link. A link carrying any of them,
/// with any value (even empty or not a string), is a state-machine transition
/// and is never rewritten by the map.
pub const TRANSITION_KEYS: &[&str] = &["event", "guard", "transition_order"];

pub fn is_transition(record: &Record) -> bool {
    TRANSITION_KEYS
        .iter()
        .any(|key| record.props.contains_key(*key))
}

pub const NODE_KEYS: &[&str] = &[
    K_LABEL, K_NOTE, K_TONE, K_STATUS, K_X, K_Y, K_W, K_H, K_H_PREF, K_W_MIN, K_H_MIN, K_PARENT,
    K_COLOR, K_EMPHASIS, K_SIZE, K_SHAPE,
];
pub const EDGE_KEYS: &[&str] = &[K_FROM, K_TO, K_LABEL];
pub const CONSTRAINT_KEYS: &[&str] =
    &[K_OP, K_MEMBERS, K_MEMBER_SCHEMA, K_AXIS, K_TEXT, K_SALIENCE];
pub const CONSTRAINT_MEMBER_KEYS: &[&str] = &[K_CONSTRAINT, K_TARGET, K_ORDER];

pub const CONSTRAINT_OPS: &[&str] = &["group", "sequence", "attaches", "voids", "labels"];
pub const CONSTRAINT_AXES: &[&str] = &["x", "y"];
pub const CONSTRAINT_SALIENCES: &[&str] = &["fore", "back"];

/// How many members an op takes: `(least, most)`, where `None` is unbounded.
/// `group` and `sequence` have no upper bound, which is why membership is
/// records and not numbered keys: a `KindClaim` lists its keys explicitly and
/// there is no wildcard.
pub fn arity(op: &str) -> (usize, Option<usize>) {
    match op {
        "group" | "sequence" => (2, None),
        "attaches" => (2, Some(2)),
        _ => (1, Some(1)),
    }
}

pub fn arity_words(op: &str) -> &'static str {
    match op {
        "group" | "sequence" => "at least two members",
        "attaches" => "exactly two members",
        _ => "exactly one member",
    }
}

/// Decode the frozen scalar. Empty segments are dropped, as the Atlas does.
pub fn decode_members(joined: &str) -> Vec<String> {
    joined
        .split(',')
        .filter(|member| !member.is_empty())
        .map(str::to_string)
        .collect()
}

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
///
/// Growth policy: a change that creates a card or link is refused when the
/// map would then hold more than this many. Edits and deletions are always
/// allowed, so a document imported above the limit stays usable and can be
/// brought back under it. Enforced in plans and in `validate`, so browser
/// frames are held to it too.
pub const MAX_MAP_OBJECTS: usize = 2_000;

/// Automatic placement: bands of `GRID_COLUMNS` columns, each band as tall as
/// the world allows, side by side. The first band is the Atlas's original
/// four-column layout, so existing pages place new cards where they always
/// did; later bands keep every slot up to [`MAX_MAP_OBJECTS`] inside the world
/// limit.
pub const GRID_ORIGIN: (f64, f64) = (48.0, 64.0);
pub const GRID_STEP: (f64, f64) = (300.0, 168.0);
pub const GRID_COLUMNS: usize = 4;

pub fn grid_rows() -> usize {
    ((WORLD_LIMIT - GRID_ORIGIN.1) / GRID_STEP.1).floor() as usize + 1
}

/// Position of automatic slot `index`.
pub fn grid_slot(index: usize) -> (f64, f64) {
    let rows = grid_rows();
    let per_band = GRID_COLUMNS * rows;
    let band = index / per_band;
    let within = index % per_band;
    let column = band * GRID_COLUMNS + within % GRID_COLUMNS;
    let row = within / GRID_COLUMNS;
    (
        GRID_ORIGIN.0 + column as f64 * GRID_STEP.0,
        GRID_ORIGIN.1 + row as f64 * GRID_STEP.1,
    )
}

/// Refuse growth past [`MAX_MAP_OBJECTS`].
pub fn check_growth(count_after: usize, created: usize) -> Result<(), CapError> {
    if created > 0 && count_after > MAX_MAP_OBJECTS {
        return Err(err(
            "full",
            format!(
                "the map would hold {count_after} cards and links; the limit is {MAX_MAP_OBJECTS}"
            ),
        ));
    }
    Ok(())
}

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

/// A write of the legacy `h` key, from any route.
pub fn refuse_h_write(id: &str) -> CapError {
    err(
        "read_only",
        format!(
            "{id}.{K_H} is legacy measured height and is read-only; \
             author {K_H_PREF} or {K_H_MIN} instead"
        ),
    )
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
    // Legacy `h` survives untouched, but no change may write it. `has` is
    // true for every key of a created record and for each modified key, so
    // this catches a new card carrying `h` and an update that sets it, on
    // every route.
    if has(K_H) && record.props.contains_key(K_H) {
        return Err(refuse_h_write(&record.id));
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
            format!(
                "no card {parent:?} to put {} inside",
                view.get(id)
                    .and_then(|r| r.props.get(K_LABEL).and_then(Scalar::as_str))
                    .filter(|l| !l.is_empty())
                    .map(|l| format!("{l:?}"))
                    .unwrap_or_else(|| if id.is_empty() { "the new card".into() } else { id.to_string() })
            ),
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
    if from == to && !is_transition(record) {
        return Err(err("invalid", "a link needs two different cards"));
    }
    if let Some(label) = string(record, K_LABEL)? {
        text(label, MAX_LABEL, "label")?;
    }
    Ok(())
}

// ── constraints ──────────────────────────────────────────────────────────

/// The membership of one constraint, in order, however it is stored.
///
/// The marker decides: a converted constraint reads from its member records
/// even when it has none, so deleting the last one leaves it empty rather than
/// reinstating the frozen scalar.
pub fn membership(view: &View, constraint: &str) -> Vec<String> {
    let Some(record) = view.get(constraint) else {
        return Vec::new();
    };
    if converted(record) {
        let mut rows: Vec<(f64, &str, &str)> = view
            .records()
            .filter(|member| member.kind == CONSTRAINT_MEMBER)
            .filter(|member| {
                member.props.get(K_CONSTRAINT).and_then(Scalar::as_str) == Some(constraint)
            })
            .map(|member| {
                let order = match member.props.get(K_ORDER) {
                    Some(Scalar::Num(value)) => *value,
                    _ => f64::MAX,
                };
                let target = member
                    .props
                    .get(K_TARGET)
                    .and_then(Scalar::as_str)
                    .unwrap_or_default();
                (order, member.id.as_str(), target)
            })
            .collect();
        // Order first, then the record id: a total order both peers compute
        // the same way, with no dependence on iteration.
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        return rows
            .into_iter()
            .map(|(_, _, target)| target.to_string())
            .collect();
    }
    decode_members(
        record
            .props
            .get(K_MEMBERS)
            .and_then(Scalar::as_str)
            .unwrap_or_default(),
    )
}

pub fn converted(record: &Record) -> bool {
    record.props.get(K_MEMBER_SCHEMA).and_then(Scalar::as_str) == Some(SCHEMA_RECORDS)
}

/// The member records of one constraint, ordered the same way.
pub fn member_records<'a>(view: &'a View, constraint: &str) -> Vec<&'a Record> {
    let mut rows: Vec<&Record> = view
        .records()
        .filter(|member| member.kind == CONSTRAINT_MEMBER)
        .filter(|member| {
            member.props.get(K_CONSTRAINT).and_then(Scalar::as_str) == Some(constraint)
        })
        .collect();
    rows.sort_by(|a, b| {
        let order = |record: &Record| match record.props.get(K_ORDER) {
            Some(Scalar::Num(value)) => *value,
            _ => f64::MAX,
        };
        order(a).total_cmp(&order(b)).then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// Whether the map can see a member's target at all. A target it cannot see is
/// **unresolved**, never "missing": an id absent from a scoped view does not
/// reveal its kind, so this capability cannot tell a deleted card from a shape
/// owned by a capability it does not read.
pub fn resolves(view: &View, target: &str) -> bool {
    view.get(target).is_some()
}

/// Check one constraint record's own keys.
pub fn check_constraint(
    record: &Record,
    keys: &BTreeSet<String>,
    created: bool,
) -> Result<String, CapError> {
    let has = |key: &str| created || keys.contains(key);
    let op = record
        .props
        .get(K_OP)
        .and_then(Scalar::as_str)
        .unwrap_or_default()
        .to_string();
    if created || has(K_OP) {
        one_of(&op, CONSTRAINT_OPS, "constraint op")?;
    }
    if has(K_AXIS) {
        if let Some(axis) = record.props.get(K_AXIS).and_then(Scalar::as_str) {
            if op == "sequence" {
                one_of(axis, CONSTRAINT_AXES, "constraint axis")?;
            } else if !axis.is_empty() {
                return Err(err(
                    "invalid",
                    format!("only a sequence has an axis, not {op:?}"),
                ));
            }
        }
    }
    if has(K_TEXT) {
        if let Some(body) = record.props.get(K_TEXT).and_then(Scalar::as_str) {
            text(body, MAX_TEXT, "constraint text")?;
            if op != "labels" && !body.is_empty() {
                return Err(err(
                    "invalid",
                    format!("only a labels constraint carries text, not {op:?}"),
                ));
            }
        }
    }
    if has(K_SALIENCE) {
        if let Some(salience) = record.props.get(K_SALIENCE).and_then(Scalar::as_str) {
            one_of(salience, CONSTRAINT_SALIENCES, "constraint salience")?;
        }
    }
    Ok(op)
}

pub const MAX_TEXT: usize = 1_200;

/// What a change did to one constraint's membership, in the terms the
/// differential rules need. Everything here is derived from `before`, `after`
/// and the ops; nothing consults an action name or an entry label, because a
/// replica frame carries neither and a rule that trusted one would be
/// bypassable by writing the same bytes as a raw frame.
pub struct MembershipChange {
    /// Whether the constraint existed before this change.
    pub existed: bool,
    pub before: Vec<String>,
    pub after: Vec<String>,
    /// Targets this change added to the membership.
    pub added: Vec<String>,
    /// True when the change removed a member record of this constraint, or
    /// deleted a target it held. A shortfall that follows a removal is
    /// inherited from it, not introduced.
    pub removed: bool,
    /// True when the change carried this constraint's legacy membership into
    /// records for the first time.
    pub converting: bool,
}

pub fn membership_change(
    id: &str,
    before: &View,
    after: &View,
    deleted: &BTreeSet<String>,
) -> MembershipChange {
    let was = membership(before, id);
    let now = membership(after, id);
    let converting = before
        .get(id)
        .map(|record| !converted(record))
        .unwrap_or(true)
        && after.get(id).is_some_and(converted);
    let added: Vec<String> = now
        .iter()
        .filter(|target| !was.contains(target))
        .cloned()
        .collect();
    let removed = was.iter().any(|target| !now.contains(target))
        || was.iter().any(|target| deleted.contains(target))
        || member_records(before, id).len() > member_records(after, id).len();
    MembershipChange {
        existed: before.get(id).is_some(),
        before: was,
        after: now,
        added,
        removed,
        converting,
    }
}

/// The differential rules for one constraint. Strict about what this change
/// introduces, silent about what it inherits.
pub fn check_membership(
    id: &str,
    op: &str,
    change: &MembershipChange,
    after: &View,
) -> Result<(), CapError> {
    // Authored here means the constraint did not exist before this change.
    let authored = !change.existed;
    // A conversion carries the legacy membership over, broken members and all.
    // Anything it adds beyond that is a genuine new assignment and is checked
    // like one.
    let strict_targets: Vec<&String> = if change.converting {
        change
            .added
            .iter()
            .filter(|target| !change.before.contains(target))
            .collect()
    } else {
        change.added.iter().collect()
    };
    for target in strict_targets {
        if !resolves(after, target) {
            return Err(err(
                "not_found",
                format!("constraint {id} member {target:?} does not resolve to an object"),
            ));
        }
        if after
            .get(target)
            .is_some_and(|record| record.kind == CONSTRAINT || record.kind == CONSTRAINT_MEMBER)
        {
            return Err(err(
                "invalid",
                format!("constraint {id} member {target:?} names another constraint"),
            ));
        }
        if target == id {
            return Err(err(
                "invalid",
                format!("constraint {id} cannot contain itself"),
            ));
        }
    }
    // Duplicates: refused when this change introduced one.
    let mut seen = BTreeSet::new();
    for target in &change.after {
        if !seen.insert(target.clone()) && change.added.contains(target) {
            return Err(err(
                "invalid",
                format!("constraint {id} holds {target:?} twice"),
            ));
        }
    }
    // Arity: judged on semantics. A shortfall that follows this change's own
    // removals is inherited from them and stays a diagnostic; a newly authored
    // constraint, or one this change grew wrongly, is refused.
    let (least, most) = arity(op);
    let count = change.after.len();
    // A shortfall already present before this change is inherited, and an
    // unrelated edit to such a constraint must still commit. A shortfall this
    // change caused by removing members is inherited from the removal. What is
    // left is a shortfall this change introduced some other way.
    let short_before = change.existed && change.before.len() < least;
    if count < least && !short_before && (authored || !change.removed) {
        return Err(err(
            "invalid",
            format!(
                "constraint op {op:?} needs {}; got {count}",
                arity_words(op)
            ),
        ));
    }
    if most.is_some_and(|most| count > most) && !change.added.is_empty() {
        return Err(err(
            "invalid",
            format!(
                "constraint op {op:?} needs {}; got {count}",
                arity_words(op)
            ),
        ));
    }
    Ok(())
}
