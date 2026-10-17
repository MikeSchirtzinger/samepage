use crate::{
    layout::{self, Spot},
    view::{self, Node},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
const MAX_PANES: usize = 24;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Human,
    Agent,
    /// An assistant that is not this room's own agent — the one the person is
    /// already talking to in a terminal, reaching in over HTTP.
    ///
    /// Worth its own name rather than being folded into [`Author::Agent`]:
    /// the room exists to establish who said what, and "the agent in this
    /// room said it" and "the assistant you were already talking to said it"
    /// are different claims. Folding them loses the one the person needs.
    Companion,
}

impl Author {
    pub fn word(self) -> &'static str {
        match self {
            Self::Human => "person",
            Self::Agent => "agent",
            Self::Companion => "companion",
        }
    }
}
pub fn credit(author: Author, name: Option<&str>) -> String {
    match (author, name) {
        (Author::Human, None) => "someone".to_string(),
        (_, None) => author.word().to_string(),
        (_, Some(name)) => format!("{} “{name}”", author.word()),
    }
}

/// Who set a mark or wrote a note. Always a person — the actions that write
/// these have no agent twin — so the category is implied and only the name is
/// worth printing.
///
/// An unnamed marker is "someone", never "they". With one person in the room
/// "they" was unambiguous; with two it silently picks one of them.
pub fn marker(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("“{name}”"),
        None => "someone".to_string(),
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    #[default]
    None,
    Question,
    Important,
    Agree,
    Disagree,
}

impl Mark {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Mark::None),
            "question" => Ok(Mark::Question),
            "important" => Ok(Mark::Important),
            "agree" => Ok(Mark::Agree),
            "disagree" => Ok(Mark::Disagree),
            other => Err(format!("unknown mark {other:?}")),
        }
    }

    pub fn describe(self) -> Option<&'static str> {
        match self {
            Mark::None => None,
            Mark::Question => Some("? (they do not follow this)"),
            Mark::Important => Some("! (this matters to them)"),
            Mark::Agree => Some("✓ (they agree with this)"),
            Mark::Disagree => Some("✗ (they think this is wrong)"),
        }
    }
}

/// Bumped when the stored shape changes in a way `load` has to repair.
///
/// 1 → 2 replaced `span`/`height` tokens with a free rectangle per pane.
/// 2 → 3 replaced the `you` author with `human` plus a participant id, because
/// `you` meant something different depending on who opened the room.
///
/// An older document is migrated in [`migrate`] on the way in and rewritten, so
/// an old room opens where it left off rather than refusing.
const SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Theme {
    pub accent: String,
    pub surface: String,
    pub density: String,
    pub radius: u8,
    pub scale: f64,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "#6ea8ff".to_string(),
            surface: "ink".to_string(),
            density: "cozy".to_string(),
            radius: 12,
            scale: 1.0,
        }
    }
}

impl Theme {
    pub fn summarize(&self) -> String {
        format!(
            "{} surface, {} density, accent {}, radius {}px, text scale {:.2}",
            self.surface, self.density, self.accent, self.radius, self.scale
        )
    }
}

const SURFACES: &[&str] = &["ink", "slate", "warm", "paper"];
const DENSITIES: &[&str] = &["compact", "cozy", "roomy"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Pane {
    pub id: String,
    pub title: String,
    pub author: Author,
    /// The display name of whoever last wrote this pane, when they announced
    /// one. Absent on every pane written before bylines existed, which is why
    /// it defaults rather than being required — an older room still opens.
    ///
    /// Kept beside [`Pane::by_id`] rather than looked up from it, so a pane
    /// written by someone who has since left still shows the name it was
    /// written under. The record keeps what was true when it was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_name: Option<String>,
    /// The participant id of whoever last wrote this pane. What a viewer
    /// compares against its own to decide whether this pane says "you".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_id: Option<String>,
    pub view: Node,
    /// Where this pane sits on the canvas. Free geometry: the person drags it
    /// anywhere and it stays there. Nothing agent-facing ever reads these
    /// numbers — [`layout::describe`] turns them into relations between named
    /// panes, which is the only form a coordinate is allowed to leave in.
    pub spot: Spot,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub mark: Mark,
    /// Who set the mark, and who wrote the note.
    ///
    /// Separate from [`Pane::by_id`], which is whoever last *wrote* the pane. A
    /// mark is a claim about somebody else's pane, so crediting it to the pane's
    /// author would credit the agent with agreeing with itself — and it is
    /// precisely because an agent cannot set one of these that a `✓` means
    /// anything at all.
    ///
    /// Separate from each other because they are set at different times by
    /// different people. Folding them into one field would let whoever wrote
    /// the note inherit whoever's mark was already there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_by_name: Option<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_by_name: Option<String>,
    /// The last thing the person clicked inside this pane's html sandbox.
    ///
    /// Kept apart from `note` on purpose. Pointing and writing are both the
    /// person speaking, but one is a gesture the page recorded and the other is
    /// a sentence they chose — putting them in one slot meant the note box came
    /// up pre-filled with whatever was last clicked, and the person had to
    /// delete it before they could say anything.
    ///
    /// Attributed for the same reason a mark is: with two people in a room,
    /// "somebody clicked Run" is not something either of them can act on.
    #[serde(default)]
    pub pointed: String,
    /// What the thing they touched was sitting inside, innermost first.
    ///
    /// The element physically under the pointer is usually not the one they
    /// mean. "This padding is wrong" is said while touching a decorative span,
    /// about the container three levels above it. Sending only the top hit
    /// forces the agent to guess which, so the whole stack under the point
    /// travels and the agent picks by meaning.
    ///
    /// Roles and names rather than selectors, because a selector is unreadable
    /// to a model and stops resolving the moment the agent rewrites the pane,
    /// which it does constantly. Absent on every pane recorded before this
    /// field existed, which is why it defaults rather than being required, an
    /// older saved room still opens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pointed_path: Vec<PointLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointed_by_name: Option<String>,
    /// Which pane is in front where two overlap. Higher is nearer. Bumped when
    /// the person touches a pane, so the last thing they reached for is the
    /// thing they can see — the same rule every window on their desktop uses.
    #[serde(default)]
    pub stack: u64,
    pub revision: u64,
}

/// One level of what was under the point, as the page described itself.
///
/// Deliberately not a selector. `role` is the element's explicit `role` or the
/// one its tag implies, `name` is its accessible name or, failing that, its
/// own text, and `point` is the author's own `data-point` label when the
/// level carries one. That last is the only identifier here the pane's
/// author chose on purpose, so it is the only one worth trying to match
/// against a later version of the same pane.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PointLevel {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<String>,
}

impl PointLevel {
    /// How one level reads in a sentence. A role with no name is still worth
    /// saying, "inside a list" locates something, but a level with neither is
    /// not, and never reaches here.
    pub fn describe(&self) -> String {
        match (self.role.is_empty(), self.name.is_empty()) {
            (false, false) => format!("{} “{}”", self.role, self.name),
            (false, true) => format!("a {}", self.role),
            (true, false) => format!("“{}”", self.name),
            (true, true) => String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LogEntry {
    pub revision: u64,
    pub by: Author,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_id: Option<String>,
    /// Past-tense phrase, already written from the agent's point of view, so
    /// the delta reads as prose instead of as a diff the model has to narrate.
    pub said: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RoomDoc {
    pub schema_version: u32,
    pub revision: u64,
    pub intent: String,
    pub theme: Theme,
    pub panes: Vec<Pane>,
    pub log: Vec<LogEntry>,
}

pub fn parse_size(value: &str) -> Result<(f64, f64), String> {
    let wide = layout::DEFAULT_W * 2.0 + 20.0;
    let tall = layout::DEFAULT_H * 2.0;
    match value.trim().to_ascii_lowercase().as_str() {
        "small" => Ok((320.0, 200.0)),
        "medium" | "default" => Ok((layout::DEFAULT_W, layout::DEFAULT_H)),
        "wide" => Ok((wide, layout::DEFAULT_H)),
        "tall" => Ok((layout::DEFAULT_W, tall)),
        "large" | "big" => Ok((wide, tall)),
        other => Err(format!(
            "unknown size {other:?}; use small, medium, wide, tall or large"
        )),
    }
}

/// Read a rectangle sent by the browser. Every field is required — a partial
/// rectangle is a bug in the drag handler, not a shorthand worth supporting.
pub fn parse_spot(raw: &JsonValue) -> Result<Spot, String> {
    let number = |key: &str| -> Result<f64, String> {
        raw.get(key)
            .and_then(JsonValue::as_f64)
            .ok_or_else(|| format!("spot needs a numeric {key}"))
    };
    Ok(Spot::new(
        number("x")?,
        number("y")?,
        number("w")?,
        number("h")?,
    ))
}

/// Bring a stored document up to [`SCHEMA_VERSION`], in place, as JSON.
///
/// Doing this before deserializing is what lets [`Pane`] carry a plain `Spot`
/// instead of an `Option<Spot>` that every later reader has to re-check. The
/// cost is that this function talks in raw keys; the benefit is that the rest
/// of the file never learns there was another shape.
pub fn migrate(body: &mut JsonValue) -> Result<(), String> {
    let version = body
        .get("schema_version")
        .and_then(JsonValue::as_u64)
        .unwrap_or(1);
    if version >= SCHEMA_VERSION as u64 {
        return Ok(());
    }

    // Each step is gated on the version it upgrades *from*. Running the flow
    // rebuild against an already-migrated document would find no `span` or
    // `height`, fall back to defaults, and relay every pane into a grid — which
    // is to say it would silently throw away the arrangement the person made.
    if version < 2 {
        let columns = body
            .get("columns")
            .and_then(JsonValue::as_u64)
            .unwrap_or(2)
            .clamp(1, 3) as u8;
        let panes = body
            .get_mut("panes")
            .and_then(JsonValue::as_array_mut)
            .ok_or_else(|| "room document has no panes array".to_string())?;

        // Rebuild the flow the old model implied, so the room opens looking like
        // the person left it rather than as a pile at the origin.
        let flow: Vec<(u8, String)> = panes
            .iter()
            .map(|pane| {
                let span = pane
                    .get("span")
                    .and_then(JsonValue::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 3) as u8;
                let height = pane
                    .get("height")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("auto")
                    .to_string();
                (span, height)
            })
            .collect();
        let borrowed: Vec<(u8, &str)> = flow
            .iter()
            .map(|(span, height)| (*span, height.as_str()))
            .collect();
        let spots = layout::migrate_flow(&borrowed, columns);

        for (pane, spot) in panes.iter_mut().zip(spots) {
            let object = pane
                .as_object_mut()
                .ok_or_else(|| "a stored pane is not an object".to_string())?;
            object.remove("span");
            object.remove("height");
            object.insert(
                "spot".to_string(),
                serde_json::to_value(spot)
                    .map_err(|error| format!("could not write a migrated pane: {error}"))?,
            );
        }
    }

    if version < 3 {
        // `you` stops being something a document can say. It was never a
        // participant, only the way one participant was rendered to one reader,
        // and a stored room reopened by a second person would have credited
        // them with everything the first one wrote.
        //
        // The rewrite deliberately adds no participant id. Nobody knows which
        // person wrote these — that information never existed — and inventing
        // one would turn "we cannot tell" into a confident wrong answer.
        for pane in body
            .get_mut("panes")
            .and_then(JsonValue::as_array_mut)
            .ok_or_else(|| "room document has no panes array".to_string())?
        {
            if pane.get("author").and_then(JsonValue::as_str) == Some("you") {
                pane["author"] = json!("human");
            }
        }
        if let Some(log) = body.get_mut("log").and_then(JsonValue::as_array_mut) {
            for entry in log {
                if entry.get("by").and_then(JsonValue::as_str) == Some("you") {
                    entry["by"] = json!("human");
                }
            }
        }
    }

    body["schema_version"] = json!(SCHEMA_VERSION);
    Ok(())
}

pub fn merge_theme(current: &Theme, args: &JsonValue) -> Result<Theme, String> {
    let mut theme = current.clone();
    if let Some(accent) = optional_string(args, "accent")? {
        let valid = accent.len() == 7
            && accent.starts_with('#')
            && accent[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return Err(format!("accent {accent:?} must be a #rrggbb hex colour"));
        }
        theme.accent = accent.to_lowercase();
    }
    if let Some(surface) = optional_string(args, "surface")? {
        if !SURFACES.contains(&surface.as_str()) {
            return Err(format!("surface must be one of {}", SURFACES.join(", ")));
        }
        theme.surface = surface;
    }
    if let Some(density) = optional_string(args, "density")? {
        if !DENSITIES.contains(&density.as_str()) {
            return Err(format!("density must be one of {}", DENSITIES.join(", ")));
        }
        theme.density = density;
    }
    if let Some(radius) = optional_u64(args, "radius")? {
        if radius > 24 {
            return Err("radius must be between 0 and 24".to_string());
        }
        theme.radius = radius as u8;
    }
    if let Some(scale) = args.get("scale")
        && !scale.is_null()
    {
        let scale = scale
            .as_f64()
            .ok_or_else(|| "scale must be a number".to_string())?;
        if !(0.8..=1.4).contains(&scale) {
            return Err("scale must be between 0.8 and 1.4".to_string());
        }
        theme.scale = (scale * 100.0).round() / 100.0;
    }
    Ok(theme)
}

pub fn validate_doc(doc: &RoomDoc) -> Result<(), String> {
    if doc.panes.len() > MAX_PANES {
        return Err(format!("a room holds at most {MAX_PANES} panes"));
    }
    let mut seen = std::collections::HashSet::new();
    for pane in &doc.panes {
        if !seen.insert(&pane.id) {
            return Err(format!("duplicate pane id {}", pane.id));
        }
        view::validate(&pane.view)?;
    }
    Ok(())
}

pub fn pane_id(value: &str) -> Result<String, String> {
    let valid = !value.is_empty()
        && value.len() <= 48
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !valid {
        return Err(format!(
            "pane id {value:?} must be lowercase letters, digits, '_' or '-', starting with a letter or digit"
        ));
    }
    Ok(value.to_string())
}

pub fn checked(field: &str, value: &str, minimum: usize, maximum: usize) -> Result<String, String> {
    let trimmed = value.trim();
    let length = trimmed.chars().count();
    if length < minimum {
        return Err(format!("{field} must not be empty"));
    }
    if length > maximum {
        return Err(format!(
            "{field} is {length} characters; the limit is {maximum}"
        ));
    }
    if trimmed
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(format!("{field} contains control characters"));
    }
    Ok(trimmed.to_string())
}

fn optional_u64(args: &JsonValue, field: &str) -> Result<Option<u64>, String> {
    match args.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{field} must be a non-negative integer when supplied")),
    }
}

fn optional_string(args: &JsonValue, field: &str) -> Result<Option<String>, String> {
    match args.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| format!("{field} must be a string when supplied")),
    }
}
